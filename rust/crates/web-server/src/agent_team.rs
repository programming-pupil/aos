use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool};
use tokio::sync::Notify;

use crate::semantic_kernel_store::{
    acquire_sqlite_write_lock, append_agent_team_domain_in_transaction,
    record_child_spawn_in_transaction, transfer_child_slot_to_agent_team_in_transaction,
    SemanticStoreError,
};

const DEFAULT_TEAM_MAX_DEPTH: i64 = 3;
const DEFAULT_TEAM_MAX_CONCURRENCY: i64 = 4;

fn team_notifier(tenant_id: &str, team_id: &str) -> Arc<Notify> {
    static NOTIFIERS: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    let map = NOTIFIERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(format!("{tenant_id}:{team_id}"))
        .or_insert_with(|| Arc::new(Notify::new()))
        .clone()
}

fn notify_team(tenant_id: &str, team_id: &str) {
    team_notifier(tenant_id, team_id).notify_waiters();
}

fn configured_limit(name: &str, default: i64, maximum: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(1, maximum)
}

fn sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentTeamMember {
    pub team_id: String,
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub name: String,
    pub role: String,
    pub depth: i64,
    pub status: String,
    pub context_mode: String,
    pub model: Option<String>,
    pub wake_requested: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SpawnRegistration {
    pub team_id: String,
    pub child_thread_id: String,
    pub existing: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoverableAgentMember {
    pub tenant_id: String,
    pub owner_user_id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MailboxDelivery {
    pub delivery_id: String,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingMailboxItem {
    pub id: String,
    pub sender_thread_id: String,
    pub delivery: String,
    pub message: String,
    pub accepted_at: String,
}

pub(crate) async fn ensure_root_member(
    db: &SqlitePool,
    tenant_id: &str,
    owner_user_id: &str,
    thread_id: &str,
) -> Result<(), SemanticStoreError> {
    let owner = sqlx::query_as::<Sqlite, (String, String)>(
        "SELECT tenant_id, owner_user_id FROM agent_threads WHERE id = ?",
    )
    .bind(thread_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("agent thread is missing".into()))?;
    if owner.0 != tenant_id || owner.1 != owner_user_id {
        return Err(SemanticStoreError::InvalidEvent(
            "agent team root crossed tenant or owner scope".into(),
        ));
    }
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_team_members
            (tenant_id, team_id, thread_id, parent_thread_id, name, role, depth,
             status, context_mode, model, spawn_idempotency_key, wake_requested)
         VALUES (?, ?, ?, NULL, 'root', 'coordinator', 0, 'running', 'fresh', NULL,
                 'root', 0)
         ON CONFLICT(tenant_id, team_id, thread_id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(thread_id)
    .execute(db)
    .await?;
    Ok(())
}

pub(crate) async fn find_spawn_by_key(
    db: &SqlitePool,
    tenant_id: &str,
    parent_thread_id: &str,
    idempotency_key: &str,
) -> Result<Option<String>, SemanticStoreError> {
    Ok(sqlx::query_scalar::<Sqlite, String>(
        "SELECT thread_id FROM agent_team_members
         WHERE tenant_id = ? AND parent_thread_id = ? AND spawn_idempotency_key = ?",
    )
    .bind(tenant_id)
    .bind(parent_thread_id)
    .bind(idempotency_key)
    .fetch_optional(db)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn register_spawn(
    db: &SqlitePool,
    tenant_id: &str,
    owner_user_id: &str,
    parent_thread_id: &str,
    child_thread_id: &str,
    name: &str,
    task: &str,
    context_mode: &str,
    model: Option<&str>,
    idempotency_key: &str,
) -> Result<SpawnRegistration, SemanticStoreError> {
    let name = name.trim();
    let task = task.trim();
    if name.is_empty() || name.chars().count() > 64 || task.is_empty() {
        return Err(SemanticStoreError::InvalidEvent(
            "agent name and task are required and name must be at most 64 characters".into(),
        ));
    }
    if !matches!(context_mode, "fresh" | "fork") {
        return Err(SemanticStoreError::InvalidEvent(
            "agent context mode must be fresh or fork".into(),
        ));
    }
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    if let Some((existing_team, existing_thread)) = sqlx::query_as::<Sqlite, (String, String)>(
        "SELECT team_id, thread_id FROM agent_team_members
         WHERE tenant_id = ? AND parent_thread_id = ? AND spawn_idempotency_key = ?",
    )
    .bind(tenant_id)
    .bind(parent_thread_id)
    .bind(idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(SpawnRegistration {
            team_id: existing_team,
            child_thread_id: existing_thread,
            existing: true,
        });
    }
    let parent = sqlx::query_as::<Sqlite, (String, String)>(
        "SELECT tenant_id, owner_user_id FROM agent_threads WHERE id = ?",
    )
    .bind(parent_thread_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("parent agent thread is missing".into()))?;
    if parent.0 != tenant_id || parent.1 != owner_user_id {
        return Err(SemanticStoreError::InvalidEvent(
            "agent team spawn crossed tenant or owner scope".into(),
        ));
    }
    let parent_member = sqlx::query_as::<Sqlite, (String, i64)>(
        "SELECT team_id, depth FROM agent_team_members
         WHERE tenant_id = ? AND thread_id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(parent_thread_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (team_id, parent_depth) =
        parent_member.unwrap_or_else(|| (parent_thread_id.to_string(), 0));
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_team_members
            (tenant_id, team_id, thread_id, parent_thread_id, name, role, depth,
             status, context_mode, model, spawn_idempotency_key, wake_requested)
         VALUES (?, ?, ?, NULL, 'root', 'coordinator', 0, 'running', 'fresh', NULL,
                 'root', 0)
         ON CONFLICT(tenant_id, team_id, thread_id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(&team_id)
    .bind(parent_thread_id)
    .execute(&mut *tx)
    .await?;
    let depth = parent_depth.saturating_add(1);
    if depth > configured_limit("AOS_AGENT_TEAM_MAX_DEPTH", DEFAULT_TEAM_MAX_DEPTH, 16) {
        return Err(SemanticStoreError::InvalidEvent(
            "agent team maximum nesting depth exceeded".into(),
        ));
    }
    sqlx::query::<Sqlite>(
        "DELETE FROM agent_concurrency_permits
         WHERE tenant_id = ? AND scope = ? AND julianday(expires_at) < julianday('now')",
    )
    .bind(tenant_id)
    .bind(format!("agent_team:{team_id}"))
    .execute(&mut *tx)
    .await?;
    let active_permits = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COUNT(*) FROM agent_concurrency_permits WHERE tenant_id = ? AND scope = ?",
    )
    .bind(tenant_id)
    .bind(format!("agent_team:{team_id}"))
    .fetch_one(&mut *tx)
    .await?;
    if active_permits
        >= configured_limit(
            "AOS_AGENT_TEAM_MAX_CONCURRENCY",
            DEFAULT_TEAM_MAX_CONCURRENCY,
            64,
        )
    {
        return Err(SemanticStoreError::InvalidEvent(
            "agent team global concurrency permit exhausted".into(),
        ));
    }
    record_child_spawn_in_transaction(
        &mut tx,
        tenant_id,
        owner_user_id,
        parent_thread_id,
        child_thread_id,
        &format!("agent-team-spawn:{idempotency_key}"),
        false,
    )
    .await?;
    transfer_child_slot_to_agent_team_in_transaction(
        &mut tx,
        tenant_id,
        parent_thread_id,
        child_thread_id,
    )
    .await?;
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_team_members
            (tenant_id, team_id, thread_id, parent_thread_id, name, role, depth,
             status, context_mode, model, spawn_idempotency_key, wake_requested)
         VALUES (?, ?, ?, ?, ?, 'worker', ?, 'queued', ?, ?, ?, 1)",
    )
    .bind(tenant_id)
    .bind(&team_id)
    .bind(child_thread_id)
    .bind(parent_thread_id)
    .bind(name)
    .bind(depth)
    .bind(context_mode)
    .bind(model)
    .bind(idempotency_key)
    .execute(&mut *tx)
    .await?;
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_concurrency_permits
            (tenant_id, scope, holder_thread_id, lease_fencing, expires_at)
         VALUES (?, ?, ?, 1, datetime('now', '+10 minutes'))",
    )
    .bind(tenant_id)
    .bind(format!("agent_team:{team_id}"))
    .bind(child_thread_id)
    .execute(&mut *tx)
    .await?;
    let mailbox_id = format!(
        "mailbox-{}",
        sha256(&format!("{team_id}:{idempotency_key}"))
    );
    let task_hash = sha256(task);
    let ciphertext = agent_gateway::crypto::encrypt_scoped(
        task,
        &agent_gateway::crypto::scoped_aad("agent_team.mailbox", tenant_id, &mailbox_id),
    )
    .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_mailbox_items
            (id, tenant_id, team_id, sender_thread_id, target_thread_id, delivery,
             content_ciphertext, content_hash, idempotency_key)
         VALUES (?, ?, ?, ?, ?, 'followup', ?, ?, ?)",
    )
    .bind(&mailbox_id)
    .bind(tenant_id)
    .bind(&team_id)
    .bind(parent_thread_id)
    .bind(child_thread_id)
    .bind(ciphertext)
    .bind(&task_hash)
    .bind(format!("spawn:{idempotency_key}"))
    .execute(&mut *tx)
    .await?;
    let task_id = format!(
        "team-task-{}",
        sha256(&format!("{team_id}:{idempotency_key}"))
    );
    let task_ciphertext = agent_gateway::crypto::encrypt_scoped(
        task,
        &agent_gateway::crypto::scoped_aad("agent_team.task", tenant_id, &task_id),
    )
    .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
    sqlx::query::<Sqlite>(
        "INSERT INTO agent_team_tasks
            (id, tenant_id, team_id, subject, description_ciphertext,
             description_hash, status, owner_thread_id)
         VALUES (?, ?, ?, ?, ?, ?, 'assigned', ?)",
    )
    .bind(&task_id)
    .bind(tenant_id)
    .bind(&team_id)
    .bind(name)
    .bind(task_ciphertext)
    .bind(&task_hash)
    .bind(child_thread_id)
    .execute(&mut *tx)
    .await?;
    append_agent_team_domain_in_transaction(
        &mut tx,
        tenant_id,
        owner_user_id,
        parent_thread_id,
        "agent_spawned",
        serde_json::json!({
            "teamId": team_id,
            "childThreadId": child_thread_id,
            "name": name,
            "depth": depth,
            "contextMode": context_mode,
            "taskHash": task_hash,
        }),
        format!("agent-team-spawn:{idempotency_key}"),
    )
    .await?;
    tx.commit().await?;
    notify_team(tenant_id, &team_id);
    Ok(SpawnRegistration {
        team_id,
        child_thread_id: child_thread_id.to_string(),
        existing: false,
    })
}

async fn resolve_target(
    db: &SqlitePool,
    tenant_id: &str,
    sender_thread_id: &str,
    target: &str,
) -> Result<(String, String), SemanticStoreError> {
    let team_id = sqlx::query_scalar::<Sqlite, String>(
        "SELECT team_id FROM agent_team_members WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(sender_thread_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("sender is not an agent team member".into()))?;
    let target_thread_id = sqlx::query_scalar::<Sqlite, String>(
        "SELECT thread_id FROM agent_team_members
         WHERE tenant_id = ? AND team_id = ? AND (thread_id = ? OR name = ?) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&team_id)
    .bind(target)
    .bind(target)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("target agent is not in this team".into()))?;
    Ok((team_id, target_thread_id))
}

pub(crate) async fn deliver_message(
    db: &SqlitePool,
    tenant_id: &str,
    owner_user_id: &str,
    sender_thread_id: &str,
    target: &str,
    content: &str,
    wake: bool,
    idempotency_key: &str,
) -> Result<serde_json::Value, SemanticStoreError> {
    let content = content.trim();
    if content.is_empty() || content.chars().count() > 16_000 {
        return Err(SemanticStoreError::InvalidEvent(
            "agent mailbox content must contain 1..16000 characters".into(),
        ));
    }
    let (team_id, target_thread_id) =
        resolve_target(db, tenant_id, sender_thread_id, target).await?;
    let id = format!(
        "mailbox-{}",
        sha256(&format!("{tenant_id}:{target_thread_id}:{idempotency_key}"))
    );
    let content_hash = sha256(content);
    let ciphertext = agent_gateway::crypto::encrypt_scoped(
        content,
        &agent_gateway::crypto::scoped_aad("agent_team.mailbox", tenant_id, &id),
    )
    .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    let inserted = sqlx::query::<Sqlite>(
        "INSERT INTO agent_mailbox_items
            (id, tenant_id, team_id, sender_thread_id, target_thread_id, delivery,
             content_ciphertext, content_hash, idempotency_key)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(tenant_id, target_thread_id, idempotency_key) DO NOTHING",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(&team_id)
    .bind(sender_thread_id)
    .bind(&target_thread_id)
    .bind(if wake { "followup" } else { "quiet" })
    .bind(ciphertext)
    .bind(&content_hash)
    .bind(idempotency_key)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        let stored = sqlx::query_as::<Sqlite, (String, String, String, String)>(
            "SELECT content_hash, sender_thread_id, team_id, delivery
             FROM agent_mailbox_items
             WHERE tenant_id = ? AND target_thread_id = ? AND idempotency_key = ?",
        )
        .bind(tenant_id)
        .bind(&target_thread_id)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await?;
        let requested_delivery = if wake { "followup" } else { "quiet" };
        if stored.0 != content_hash
            || stored.1 != sender_thread_id
            || stored.2 != team_id
            || stored.3 != requested_delivery
        {
            return Err(SemanticStoreError::InvalidEvent(
                "mailbox idempotency key was reused with different content or delivery semantics"
                    .into(),
            ));
        }
    }
    if wake {
        sqlx::query::<Sqlite>(
            "UPDATE agent_team_members
             SET wake_requested = 1,
                 status = CASE WHEN status IN ('completed','idle','failed') THEN 'queued' ELSE status END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND team_id = ? AND thread_id = ?",
        )
        .bind(tenant_id)
        .bind(&team_id)
        .bind(&target_thread_id)
        .execute(&mut *tx)
        .await?;
        let task_id = format!(
            "team-task-{}",
            sha256(&format!("{team_id}:{target_thread_id}:{idempotency_key}"))
        );
        let task_ciphertext = agent_gateway::crypto::encrypt_scoped(
            content,
            &agent_gateway::crypto::scoped_aad("agent_team.task", tenant_id, &task_id),
        )
        .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
        sqlx::query::<Sqlite>(
            "INSERT INTO agent_team_tasks
                (id, tenant_id, team_id, subject, description_ciphertext,
                 description_hash, status, owner_thread_id)
             VALUES (?, ?, ?, 'follow-up task', ?, ?, 'assigned', ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&task_id)
        .bind(tenant_id)
        .bind(&team_id)
        .bind(task_ciphertext)
        .bind(&content_hash)
        .bind(&target_thread_id)
        .execute(&mut *tx)
        .await?;
    }
    append_agent_team_domain_in_transaction(
        &mut tx,
        tenant_id,
        owner_user_id,
        sender_thread_id,
        if wake {
            "agent_followup"
        } else {
            "agent_message"
        },
        serde_json::json!({
            "teamId": team_id,
            "targetThreadId": target_thread_id,
            "contentHash": content_hash,
            "delivery": if wake { "followup" } else { "quiet" },
        }),
        format!("agent-mailbox:{idempotency_key}"),
    )
    .await?;
    tx.commit().await?;
    notify_team(tenant_id, &team_id);
    Ok(serde_json::json!({
        "accepted": true,
        "deduplicated": inserted.rows_affected() == 0,
        "teamId": team_id,
        "target": target_thread_id,
        "delivery": if wake { "followup" } else { "quiet" },
    }))
}

pub(crate) async fn pending_mailbox(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
) -> Result<Vec<PendingMailboxItem>, SemanticStoreError> {
    let rows = sqlx::query::<Sqlite>(
        "SELECT id, sender_thread_id, delivery, content_ciphertext, accepted_at
         FROM agent_mailbox_items
         WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
         ORDER BY accepted_at ASC, id ASC",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|row| {
            let id = row.try_get::<String, _>("id")?;
            let message = agent_gateway::crypto::decrypt_scoped(
                &row.try_get::<String, _>("content_ciphertext")?,
                &agent_gateway::crypto::scoped_aad("agent_team.mailbox", tenant_id, &id),
            )
            .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
            Ok(PendingMailboxItem {
                id,
                sender_thread_id: row.try_get("sender_thread_id")?,
                delivery: row.try_get("delivery")?,
                message,
                accepted_at: row.try_get("accepted_at")?,
            })
        })
        .collect()
}

pub(crate) async fn acknowledge_pending_mailbox(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    turn_id: &str,
) -> Result<u64, SemanticStoreError> {
    Ok(sqlx::query::<Sqlite>(
        "UPDATE agent_mailbox_items
         SET consumed_turn_id = COALESCE(consumed_turn_id, observed_turn_id, ?),
             consumed_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
           AND observed_turn_id IS NOT NULL",
    )
    .bind(turn_id)
    .bind(tenant_id)
    .bind(thread_id)
    .execute(db)
    .await?
    .rows_affected())
}

pub(crate) async fn claim_pending_mailbox_items(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    delivery_id: &str,
    item_ids: &[String],
) -> Result<u64, SemanticStoreError> {
    let mut claimed = 0_u64;
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    for item_id in item_ids {
        claimed = claimed.saturating_add(
            sqlx::query::<Sqlite>(
                "UPDATE agent_mailbox_items
                 SET observed_turn_id = COALESCE(observed_turn_id, ?)
                 WHERE tenant_id = ? AND target_thread_id = ? AND id = ?
                   AND consumed_at IS NULL",
            )
            .bind(delivery_id)
            .bind(tenant_id)
            .bind(thread_id)
            .bind(item_id)
            .execute(&mut *tx)
            .await?
            .rows_affected(),
        );
    }
    tx.commit().await?;
    Ok(claimed)
}

pub(crate) async fn mailbox_result_was_delivered(
    db: &SqlitePool,
    tenant_id: &str,
    sender_thread_id: &str,
    delivery_id: &str,
) -> Result<bool, SemanticStoreError> {
    Ok(sqlx::query_scalar::<Sqlite, i64>(
        "SELECT EXISTS(
             SELECT 1 FROM agent_mailbox_items
             WHERE tenant_id = ? AND sender_thread_id = ? AND idempotency_key = ?
         )",
    )
    .bind(tenant_id)
    .bind(sender_thread_id)
    .bind(format!("agent-result:{delivery_id}"))
    .fetch_one(db)
    .await?
        != 0)
}

pub(crate) async fn consume_mailbox(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
) -> Result<MailboxDelivery, SemanticStoreError> {
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    let existing_delivery = sqlx::query_scalar::<Sqlite, String>(
        "SELECT consumed_turn_id FROM agent_mailbox_items
         WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
           AND consumed_turn_id IS NOT NULL AND delivery_attempts < 3
         ORDER BY accepted_at ASC, id ASC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_optional(&mut *tx)
    .await?;
    let rows = if let Some(delivery_id) = existing_delivery.as_deref() {
        sqlx::query::<Sqlite>(
            "SELECT id, content_ciphertext FROM agent_mailbox_items
             WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
               AND consumed_turn_id = ? AND delivery_attempts < 3
             ORDER BY accepted_at ASC, id ASC",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .bind(delivery_id)
        .fetch_all(&mut *tx)
        .await?
    } else {
        sqlx::query::<Sqlite>(
            "SELECT id, content_ciphertext FROM agent_mailbox_items
             WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
               AND consumed_turn_id IS NULL AND delivery_attempts < 3
             ORDER BY accepted_at ASC, id ASC",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .fetch_all(&mut *tx)
        .await?
    };
    let ids = rows
        .iter()
        .map(|row| row.try_get::<String, _>("id"))
        .collect::<Result<Vec<_>, _>>()?;
    let delivery_id =
        existing_delivery.unwrap_or_else(|| format!("agent-team-turn-{}", sha256(&ids.join("\n"))));
    let mut messages = Vec::with_capacity(rows.len());
    for row in &rows {
        let id = row.try_get::<String, _>("id")?;
        let plaintext = agent_gateway::crypto::decrypt_scoped(
            &row.try_get::<String, _>("content_ciphertext")?,
            &agent_gateway::crypto::scoped_aad("agent_team.mailbox", tenant_id, &id),
        )
        .map_err(|error| SemanticStoreError::InvalidEvent(error.to_string()))?;
        let changed = sqlx::query::<Sqlite>(
            "UPDATE agent_mailbox_items
             SET consumed_turn_id = ?, delivery_attempts = delivery_attempts + 1
             WHERE tenant_id = ? AND id = ? AND consumed_at IS NULL
               AND delivery_attempts < 3",
        )
        .bind(&delivery_id)
        .bind(tenant_id)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() == 1 {
            messages.push(plaintext);
        }
    }
    sqlx::query::<Sqlite>(
        "UPDATE agent_team_members SET wake_requested = 0, status = 'running',
                 updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(MailboxDelivery {
        delivery_id,
        messages,
    })
}

pub(crate) async fn acknowledge_mailbox_turn(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    turn_id: &str,
) -> Result<u64, SemanticStoreError> {
    Ok(sqlx::query::<Sqlite>(
        "UPDATE agent_mailbox_items SET consumed_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND target_thread_id = ? AND consumed_turn_id = ?
           AND consumed_at IS NULL",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(turn_id)
    .execute(db)
    .await?
    .rows_affected())
}

pub(crate) async fn requeue_unacknowledged_mailbox(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
) -> Result<bool, SemanticStoreError> {
    let retryable = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COUNT(*) FROM agent_mailbox_items
         WHERE tenant_id = ? AND target_thread_id = ? AND consumed_at IS NULL
           AND delivery_attempts < 3",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_one(db)
    .await?
        > 0;
    if retryable {
        mark_member_status(db, tenant_id, thread_id, "queued", None).await?;
        sqlx::query::<Sqlite>(
            "UPDATE agent_team_members SET wake_requested = 1, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND thread_id = ?",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .execute(db)
        .await?;
    }
    Ok(retryable)
}

pub(crate) async fn claim_worker(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    lease_owner: &str,
) -> Result<bool, SemanticStoreError> {
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    let member = sqlx::query_as::<Sqlite, (String, String)>(
        "SELECT team_id, status FROM agent_team_members
         WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("agent team member is missing".into()))?;
    if member.1 != "queued" {
        tx.commit().await?;
        return Ok(false);
    }
    let scope = format!("agent_team:{}", member.0);
    sqlx::query::<Sqlite>(
        "DELETE FROM agent_concurrency_permits
         WHERE tenant_id = ? AND scope = ? AND julianday(expires_at) < julianday('now')",
    )
    .bind(tenant_id)
    .bind(&scope)
    .execute(&mut *tx)
    .await?;
    let permit_exists = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COUNT(*) FROM agent_concurrency_permits
         WHERE tenant_id = ? AND scope = ? AND holder_thread_id = ?",
    )
    .bind(tenant_id)
    .bind(&scope)
    .bind(thread_id)
    .fetch_one(&mut *tx)
    .await?
        > 0;
    if !permit_exists {
        let active = sqlx::query_scalar::<Sqlite, i64>(
            "SELECT COUNT(*) FROM agent_concurrency_permits WHERE tenant_id = ? AND scope = ?",
        )
        .bind(tenant_id)
        .bind(&scope)
        .fetch_one(&mut *tx)
        .await?;
        if active
            >= configured_limit(
                "AOS_AGENT_TEAM_MAX_CONCURRENCY",
                DEFAULT_TEAM_MAX_CONCURRENCY,
                64,
            )
        {
            tx.commit().await?;
            return Ok(false);
        }
        sqlx::query::<Sqlite>(
            "INSERT INTO agent_concurrency_permits
                (tenant_id, scope, holder_thread_id, lease_fencing, expires_at)
             VALUES (?, ?, ?, 1, datetime('now', '+10 minutes'))",
        )
        .bind(tenant_id)
        .bind(&scope)
        .bind(thread_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query::<Sqlite>(
            "UPDATE agent_concurrency_permits
             SET expires_at = datetime('now', '+10 minutes'), updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND scope = ? AND holder_thread_id = ?",
        )
        .bind(tenant_id)
        .bind(&scope)
        .bind(thread_id)
        .execute(&mut *tx)
        .await?;
    }
    let changed = sqlx::query::<Sqlite>(
        "UPDATE agent_team_members
         SET status = 'running', wake_requested = 0, lease_owner = ?,
             lease_fencing = lease_fencing + 1,
             lease_expires_at = datetime('now', '+10 minutes'),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND thread_id = ? AND status = 'queued'",
    )
    .bind(lease_owner)
    .bind(tenant_id)
    .bind(thread_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(changed.rows_affected() == 1)
}

pub(crate) async fn renew_worker_lease(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    lease_owner: &str,
) -> Result<bool, SemanticStoreError> {
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    let team_id = sqlx::query_scalar::<Sqlite, String>(
        "SELECT team_id FROM agent_team_members
         WHERE tenant_id = ? AND thread_id = ? AND status = 'running' AND lease_owner = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(lease_owner)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(team_id) = team_id else {
        tx.commit().await?;
        return Ok(false);
    };
    let member = sqlx::query::<Sqlite>(
        "UPDATE agent_team_members
         SET lease_expires_at = datetime('now', '+10 minutes'), updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND thread_id = ? AND status = 'running' AND lease_owner = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .bind(lease_owner)
    .execute(&mut *tx)
    .await?;
    if member.rows_affected() != 1 {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query::<Sqlite>(
        "UPDATE agent_concurrency_permits
         SET expires_at = datetime('now', '+10 minutes'), updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND scope = ? AND holder_thread_id = ?",
    )
    .bind(tenant_id)
    .bind(format!("agent_team:{team_id}"))
    .bind(thread_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub(crate) async fn member_status(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
) -> Result<Option<String>, SemanticStoreError> {
    Ok(sqlx::query_scalar::<Sqlite, String>(
        "SELECT status FROM agent_team_members WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_optional(db)
    .await?)
}

/// Reclaim process-local workers after an exclusive server restart. The
/// platform lifecycle lock guarantees no previous web-server process is still
/// executing these leases. Interrupted members stay idle; queued/running
/// members with pending work become recoverable.
pub(crate) async fn reclaim_startup_workers(
    db: &SqlitePool,
) -> Result<Vec<RecoverableAgentMember>, SemanticStoreError> {
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    sqlx::query::<Sqlite>(
        "UPDATE agent_team_members
         SET status = 'idle', lease_owner = NULL, lease_expires_at = NULL,
             updated_at = CURRENT_TIMESTAMP
         WHERE status = 'interrupt_requested'",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query::<Sqlite>(
        "UPDATE agent_team_members
         SET status = 'queued', wake_requested = 1, lease_owner = NULL,
             lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE parent_thread_id IS NOT NULL AND status = 'running'",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query::<Sqlite>("DELETE FROM agent_concurrency_permits")
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query::<Sqlite>(
        "SELECT m.tenant_id, t.owner_user_id, m.thread_id
         FROM agent_team_members m
         JOIN agent_threads t ON t.id = m.thread_id AND t.tenant_id = m.tenant_id
         WHERE m.parent_thread_id IS NOT NULL
           AND m.status = 'queued'
           AND (m.wake_requested = 1 OR EXISTS (
               SELECT 1 FROM agent_mailbox_items b
               WHERE b.tenant_id = m.tenant_id AND b.target_thread_id = m.thread_id
                 AND b.consumed_at IS NULL
           ))
         ORDER BY m.created_at ASC",
    )
    .fetch_all(&mut *tx)
    .await?;
    let members = rows
        .into_iter()
        .map(|row| RecoverableAgentMember {
            tenant_id: row.get("tenant_id"),
            owner_user_id: row.get("owner_user_id"),
            thread_id: row.get("thread_id"),
        })
        .collect();
    tx.commit().await?;
    Ok(members)
}

pub(crate) async fn request_descendant_cancellation(
    db: &SqlitePool,
    tenant_id: &str,
    root_thread_id: &str,
) -> Result<Vec<String>, SemanticStoreError> {
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    let descendants = sqlx::query_scalar::<Sqlite, String>(
        "WITH RECURSIVE descendants(thread_id) AS (
             SELECT thread_id FROM agent_team_members
              WHERE tenant_id = ? AND parent_thread_id = ? AND detached = 0
             UNION ALL
             SELECT child.thread_id FROM agent_team_members child
             JOIN descendants parent ON child.parent_thread_id = parent.thread_id
              WHERE child.tenant_id = ? AND child.detached = 0
         )
         SELECT thread_id FROM descendants",
    )
    .bind(tenant_id)
    .bind(root_thread_id)
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    for thread_id in &descendants {
        sqlx::query::<Sqlite>(
            "UPDATE agent_team_members
             SET status = 'interrupt_requested', wake_requested = 0,
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND thread_id = ?
               AND status IN ('queued','running','idle','completed','failed')",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query::<Sqlite>(
            "DELETE FROM agent_concurrency_permits
             WHERE tenant_id = ? AND holder_thread_id = ?",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(descendants)
}

pub(crate) async fn list_members(
    db: &SqlitePool,
    tenant_id: &str,
    caller_thread_id: &str,
    path_prefix: Option<&str>,
) -> Result<Vec<AgentTeamMember>, SemanticStoreError> {
    let team_id = sqlx::query_scalar::<Sqlite, String>(
        "SELECT team_id FROM agent_team_members WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(caller_thread_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("caller is not an agent team member".into()))?;
    let rows = sqlx::query::<Sqlite>(
        "SELECT team_id, thread_id, parent_thread_id, name, role, depth, status,
                context_mode, model, wake_requested, updated_at
         FROM agent_team_members WHERE tenant_id = ? AND team_id = ?
         ORDER BY depth ASC, created_at ASC",
    )
    .bind(tenant_id)
    .bind(&team_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let name = row.get::<String, _>("name");
            let thread_id = row.get::<String, _>("thread_id");
            if path_prefix
                .is_some_and(|prefix| !name.starts_with(prefix) && !thread_id.starts_with(prefix))
            {
                return None;
            }
            Some(AgentTeamMember {
                team_id: row.get("team_id"),
                thread_id,
                parent_thread_id: row.get("parent_thread_id"),
                name,
                role: row.get("role"),
                depth: row.get("depth"),
                status: row.get("status"),
                context_mode: row.get("context_mode"),
                model: row.get("model"),
                wake_requested: row.get::<i64, _>("wake_requested") != 0,
                updated_at: row.get("updated_at"),
            })
        })
        .collect())
}

pub(crate) async fn wait_for_change(
    db: &SqlitePool,
    tenant_id: &str,
    caller_thread_id: &str,
    timeout_ms: u64,
) -> Result<serde_json::Value, SemanticStoreError> {
    let before = list_members(db, tenant_id, caller_thread_id, None).await?;
    let before_mailbox = pending_mailbox(db, tenant_id, caller_thread_id).await?;
    if !before_mailbox.is_empty() {
        return Ok(serde_json::json!({
            "changed": true,
            "reason": "mailbox",
            "agents": before,
            "mailbox": before_mailbox,
        }));
    }
    let active_peers = before.iter().filter(|member| {
        member.thread_id != caller_thread_id
            && matches!(
                member.status.as_str(),
                "queued" | "running" | "interrupt_requested"
            )
    });
    if active_peers.count() == 0 {
        return Ok(serde_json::json!({
            "changed": false,
            "reason": "no_active_peer",
            "agents": before,
            "mailbox": before_mailbox,
        }));
    }
    let team_id = before
        .first()
        .map(|member| member.team_id.clone())
        .ok_or_else(|| SemanticStoreError::InvalidEvent("agent team roster is empty".into()))?;
    let notifier = team_notifier(tenant_id, &team_id);
    let notified = notifier.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let armed = list_members(db, tenant_id, caller_thread_id, None).await?;
    if armed != before {
        return Ok(serde_json::json!({
            "changed": true,
            "reason": "team_event",
            "agents": armed,
        }));
    }
    let notified = tokio::time::timeout(
        Duration::from_millis(timeout_ms.clamp(10, 60_000)),
        notified,
    )
    .await
    .is_ok();
    let after = list_members(db, tenant_id, caller_thread_id, None).await?;
    let mailbox = pending_mailbox(db, tenant_id, caller_thread_id).await?;
    let changed = notified || after != before || !mailbox.is_empty();
    Ok(serde_json::json!({
        "changed": changed,
        "reason": if !mailbox.is_empty() { "mailbox" } else if changed { "team_event" } else { "timeout" },
        "agents": after,
        "mailbox": mailbox,
    }))
}

pub(crate) async fn mark_member_status(
    db: &SqlitePool,
    tenant_id: &str,
    thread_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), SemanticStoreError> {
    if !matches!(
        status,
        "queued" | "running" | "idle" | "completed" | "failed" | "interrupt_requested"
    ) {
        return Err(SemanticStoreError::InvalidEvent(
            "invalid agent team member status".into(),
        ));
    }
    let team_id = sqlx::query_scalar::<Sqlite, String>(
        "SELECT team_id FROM agent_team_members WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(tenant_id)
    .bind(thread_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SemanticStoreError::InvalidEvent("agent team member is missing".into()))?;
    let protected_error = error.map(|value| {
        runtime::protect_sensitive_text(value, runtime::configured_data_protection_mode()).value
    });
    sqlx::query::<Sqlite>(
        "UPDATE agent_team_members SET status = ?, last_error = ?,
                lease_owner = NULL, lease_expires_at = NULL,
                updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND thread_id = ?",
    )
    .bind(status)
    .bind(protected_error)
    .bind(tenant_id)
    .bind(thread_id)
    .execute(db)
    .await?;
    if matches!(status, "completed" | "failed" | "idle") {
        sqlx::query::<Sqlite>(
            "DELETE FROM agent_concurrency_permits
             WHERE tenant_id = ? AND holder_thread_id = ?",
        )
        .bind(tenant_id)
        .bind(thread_id)
        .execute(db)
        .await?;
        if matches!(status, "completed" | "failed") {
            sqlx::query::<Sqlite>(
                "UPDATE agent_team_tasks SET status = ?, revision = revision + 1,
                         updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND owner_thread_id = ? AND status = 'assigned'",
            )
            .bind(status)
            .bind(tenant_id)
            .bind(thread_id)
            .execute(db)
            .await?;
        }
    }
    notify_team(tenant_id, &team_id);
    Ok(())
}

pub(crate) async fn interrupt_member(
    db: &SqlitePool,
    tenant_id: &str,
    caller_thread_id: &str,
    target: &str,
) -> Result<String, SemanticStoreError> {
    let (team_id, target_thread_id) =
        resolve_target(db, tenant_id, caller_thread_id, target).await?;
    if target_thread_id == caller_thread_id {
        return Err(SemanticStoreError::InvalidEvent(
            "agent cannot interrupt itself through the team control plane".into(),
        ));
    }
    let mut tx = db.begin().await?;
    acquire_sqlite_write_lock(&mut tx).await?;
    sqlx::query::<Sqlite>(
        "UPDATE agent_team_members SET status = 'interrupt_requested', updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND team_id = ? AND thread_id = ?
           AND status IN ('queued','running')",
    )
    .bind(tenant_id)
    .bind(&team_id)
    .bind(&target_thread_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query::<Sqlite>(
        "DELETE FROM agent_concurrency_permits
         WHERE tenant_id = ? AND holder_thread_id = ?",
    )
    .bind(tenant_id)
    .bind(&target_thread_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    notify_team(tenant_id, &team_id);
    Ok(target_thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_thread(db: &SqlitePool, tenant: &str, owner: &str, thread: &str) {
        sqlx::query::<Sqlite>(
            "INSERT INTO agent_threads
                (id, tenant_id, owner_user_id, status, schema_version, created_at, updated_at)
             VALUES (?, ?, ?, 'running', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(thread)
        .bind(tenant)
        .bind(owner)
        .execute(db)
        .await
        .expect("seed agent thread");
    }

    async fn spawn(
        db: &SqlitePool,
        parent: &str,
        child: &str,
        name: &str,
        key: &str,
    ) -> Result<SpawnRegistration, SemanticStoreError> {
        register_spawn(
            db,
            "tenant",
            "owner",
            parent,
            child,
            name,
            &format!("task for {name}"),
            "fresh",
            Some("test-model"),
            key,
        )
        .await
    }

    #[tokio::test]
    async fn durable_team_enforces_idempotency_mailbox_and_lifecycle_invariants() {
        let db = crate::test_sqlite_pool().await;
        seed_thread(&db, "tenant", "owner", "root").await;
        ensure_root_member(&db, "tenant", "owner", "root")
            .await
            .unwrap();

        let first = spawn(&db, "root", "child-a", "worker-a", "spawn-a")
            .await
            .unwrap();
        let duplicate = spawn(&db, "root", "ignored-child", "ignored-name", "spawn-a")
            .await
            .unwrap();
        assert!(!first.existing);
        assert!(duplicate.existing);
        assert_eq!(duplicate.team_id, "root");
        assert_eq!(duplicate.child_thread_id, "child-a");
        assert_eq!(
            sqlx::query_scalar::<Sqlite, i64>(
                "SELECT COUNT(*) FROM agent_threads WHERE id = 'ignored-child'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0
        );

        let conflict = spawn(&db, "root", "child-bad", "worker-a", "spawn-bad").await;
        assert!(conflict.is_err(), "same-name registration must fail");
        assert_eq!(
            sqlx::query_scalar::<Sqlite, i64>(
                "SELECT COUNT(*) FROM agent_threads WHERE id = 'child-bad'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0,
            "failed spawn transaction must not leave an orphan thread"
        );

        mark_member_status(&db, "tenant", "child-a", "completed", None)
            .await
            .unwrap();
        deliver_message(
            &db,
            "tenant",
            "owner",
            "root",
            "child-a",
            "quiet update",
            false,
            "quiet-1",
        )
        .await
        .unwrap();
        assert_eq!(
            member_status(&db, "tenant", "child-a").await.unwrap(),
            Some("completed".into()),
            "quiet delivery must not wake a completed member"
        );
        assert!(
            deliver_message(
                &db,
                "tenant",
                "owner",
                "root",
                "child-a",
                "quiet update",
                true,
                "quiet-1",
            )
            .await
            .is_err(),
            "an idempotency key cannot change quiet delivery into follow-up work"
        );
        deliver_message(
            &db,
            "tenant",
            "owner",
            "root",
            "child-a",
            "follow-up",
            true,
            "followup-1",
        )
        .await
        .unwrap();
        assert_eq!(
            member_status(&db, "tenant", "child-a").await.unwrap(),
            Some("queued".into())
        );

        let first_delivery = consume_mailbox(&db, "tenant", "child-a").await.unwrap();
        assert_eq!(first_delivery.messages.len(), 3);
        for expected in ["task for worker-a", "quiet update", "follow-up"] {
            assert!(first_delivery
                .messages
                .iter()
                .any(|message| message == expected));
        }
        assert!(requeue_unacknowledged_mailbox(&db, "tenant", "child-a")
            .await
            .unwrap());
        let retry = consume_mailbox(&db, "tenant", "child-a").await.unwrap();
        assert_eq!(retry.delivery_id, first_delivery.delivery_id);
        assert_eq!(retry.messages, first_delivery.messages);
        assert_eq!(
            acknowledge_mailbox_turn(&db, "tenant", "child-a", &retry.delivery_id)
                .await
                .unwrap(),
            3
        );
        assert!(consume_mailbox(&db, "tenant", "child-a")
            .await
            .unwrap()
            .messages
            .is_empty());

        mark_member_status(&db, "tenant", "child-a", "completed", None)
            .await
            .unwrap();
        let waited = wait_for_change(&db, "tenant", "root", 10).await.unwrap();
        assert_eq!(waited["reason"], "no_active_peer");

        deliver_message(
            &db,
            "tenant",
            "owner",
            "child-a",
            "root",
            "child result",
            false,
            "result-1",
        )
        .await
        .unwrap();
        let waited = wait_for_change(&db, "tenant", "root", 10).await.unwrap();
        assert_eq!(waited["reason"], "mailbox");
        let item_ids = waited["mailbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            claim_pending_mailbox_items(&db, "tenant", "root", "wait-tool", &item_ids)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            acknowledge_pending_mailbox(&db, "tenant", "root", "parent-turn")
                .await
                .unwrap(),
            1
        );
        assert!(pending_mailbox(&db, "tenant", "root")
            .await
            .unwrap()
            .is_empty());

        deliver_message(
            &db,
            "tenant",
            "owner",
            "root",
            "child-a",
            "preserve me",
            true,
            "interrupt-message",
        )
        .await
        .unwrap();
        interrupt_member(&db, "tenant", "root", "child-a")
            .await
            .unwrap();
        assert_eq!(
            pending_mailbox(&db, "tenant", "child-a")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<Sqlite, i64>(
                "SELECT COUNT(*) FROM agent_concurrency_permits
                 WHERE tenant_id = 'tenant' AND holder_thread_id = 'child-a'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0,
            "interrupting a queued member must release its permit"
        );

        seed_thread(&db, "other", "other-owner", "other-root").await;
        ensure_root_member(&db, "other", "other-owner", "other-root")
            .await
            .unwrap();
        assert!(deliver_message(
            &db,
            "other",
            "other-owner",
            "other-root",
            "child-a",
            "cross tenant",
            false,
            "cross-tenant",
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn durable_team_recovers_workers_limits_concurrency_and_cancels_descendants() {
        let db = crate::test_sqlite_pool().await;
        seed_thread(&db, "tenant", "owner", "root").await;
        ensure_root_member(&db, "tenant", "owner", "root")
            .await
            .unwrap();
        for index in 1..=4 {
            spawn(
                &db,
                "root",
                &format!("child-{index}"),
                &format!("worker-{index}"),
                &format!("spawn-{index}"),
            )
            .await
            .unwrap();
        }
        assert!(spawn(&db, "root", "child-5", "worker-5", "spawn-5")
            .await
            .is_err());

        assert!(claim_worker(&db, "tenant", "child-1", "lease-1")
            .await
            .unwrap());
        let recovered = reclaim_startup_workers(&db).await.unwrap();
        assert!(recovered.iter().any(|member| member.thread_id == "child-1"));
        assert_eq!(
            member_status(&db, "tenant", "child-1").await.unwrap(),
            Some("queued".into())
        );

        for index in 2..=4 {
            mark_member_status(&db, "tenant", &format!("child-{index}"), "completed", None)
                .await
                .unwrap();
        }
        let nested = spawn(&db, "child-1", "grandchild", "grandchild", "nested-1")
            .await
            .unwrap();
        assert_eq!(nested.team_id, "root");
        let deep = spawn(&db, "grandchild", "great-grandchild", "great", "nested-2")
            .await
            .unwrap();
        assert_eq!(deep.team_id, "root");
        assert!(
            spawn(&db, "great-grandchild", "too-deep", "too-deep", "nested-3")
                .await
                .is_err()
        );

        let cancelled = request_descendant_cancellation(&db, "tenant", "root")
            .await
            .unwrap();
        assert!(cancelled.contains(&"child-1".to_string()));
        assert!(cancelled.contains(&"grandchild".to_string()));
        assert!(cancelled.contains(&"great-grandchild".to_string()));
        assert_eq!(
            sqlx::query_scalar::<Sqlite, i64>(
                "SELECT COUNT(*) FROM agent_concurrency_permits WHERE tenant_id = 'tenant'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0
        );
    }
}
