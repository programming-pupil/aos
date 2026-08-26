//! Durable WatchDog command, projection, outbox, and notification workers.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{Timelike, Utc};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};
use tokio::sync::Semaphore;

use crate::{
    error::{AppError, Result},
    routes::agent_ops::{self, WatchDogActionAudit},
    state::AppState,
};

const COMMAND_BATCH: i64 = 16;
const OUTBOX_BATCH: i64 = 64;
const DELIVERY_BATCH: i64 = 32;
const LEASE_SECONDS: i64 = 60;
const LEASE_HEARTBEAT_SECONDS: u64 = 20;

fn retry_backoff_seconds(attempt_count: i64) -> i64 {
    let exponent = u32::try_from(attempt_count.max(0))
        .unwrap_or(u32::MAX)
        .min(9);
    1_i64.checked_shl(exponent).unwrap_or(i64::MAX).min(300)
}

fn watchdog_worker_db_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let limit = std::env::var("WATCHDOG_DB_MAX_CONCURRENT")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(1, 32);
        Arc::new(Semaphore::new(limit))
    })
}

fn durable_notification_delivery_enabled_for(raw_mode: Option<&str>) -> bool {
    raw_mode.is_some_and(|mode| mode.trim().eq_ignore_ascii_case("on"))
}

fn global_notification_mode() -> String {
    let mode = std::env::var("WATCHDOG_NOTIFICATION_OUTBOX_MODE")
        .unwrap_or_else(|_| "on".to_string())
        .trim()
        .to_ascii_lowercase();
    if matches!(mode.as_str(), "off" | "shadow" | "on") {
        mode
    } else {
        tracing::error!(
            mode,
            "invalid WATCHDOG_NOTIFICATION_OUTBOX_MODE; falling back to shadow"
        );
        "shadow".to_string()
    }
}

pub(crate) fn durable_notification_delivery_enabled() -> bool {
    durable_notification_delivery_enabled_for(Some(&global_notification_mode()))
}

async fn tenant_notification_mode(state: &AppState, tenant_id: &str) -> Result<String> {
    crate::routes::task_control::watchdog_feature_mode(
        state,
        tenant_id,
        "watchdog_notification_outbox",
        &global_notification_mode(),
    )
    .await
}

pub(crate) async fn legacy_capability_notifications_enabled(
    state: &AppState,
    tenant_id: &str,
) -> bool {
    match tenant_notification_mode(state, tenant_id).await {
        Ok(mode) => mode != "on",
        Err(error) => {
            tracing::error!(
                tenant_id,
                error = %error,
                error_debug = ?error,
                "failed to resolve WatchDog notification rollout mode; suppressing legacy delivery"
            );
            false
        }
    }
}

pub async fn ensure_task_control_schema(state: &AppState) -> Result<()> {
    for (table, columns) in [
        (
            "agent_tasks",
            "short_code, root_task_id, initiator_user_id, state_version, progress_json, last_progress_at, budget_json, cost_json, projection_active",
        ),
        (
            "agent_task_command_requests",
            "id, task_id, status, lease_expires_at, idempotency_key",
        ),
        (
            "agent_task_outbox",
            "id, event_id, task_id, root_task_id, state_version, status",
        ),
        (
            "agent_notification_deliveries",
            "id, outbox_id, task_id, user_id, status, idempotency_key, dispatch_started_at",
        ),
        (
            "agent_external_identity_links",
            "id, user_id, platform, external_user_id, channel_id, external_conversation_id, status",
        ),
        (
            "agent_task_attempts",
            "id, task_id, attempt_no, trigger_type, status",
        ),
        (
            "agent_task_subscriptions",
            "id, task_id, task_key, user_id, destination_type, destination_key, enabled",
        ),
        (
            "agent_watch_rules",
            "id, user_id, condition_json, action_json, enabled",
        ),
        (
            "tenant_agent_features",
            "tenant_id, feature_key, mode",
        ),
    ] {
        sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(format!(
            "SELECT {columns} FROM {table} LIMIT 0"
        )))
            .execute(state.control_db())
            .await
            .map_err(|error| {
                AppError::Internal(format!(
                    "WatchDog control-plane schema is incomplete at {table}; run migrations 188 through 202: {error}"
                ))
            })?;
    }
    Ok(())
}

pub fn start_task_control_workers(state: AppState) {
    let notification_mode = global_notification_mode();
    let notifications_enabled = durable_notification_delivery_enabled();
    let command_state = state.clone();
    tokio::spawn(async move {
        let worker_id = format!(
            "task-command:{}:{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        run_worker_loop(
            "task command",
            Duration::from_millis(200),
            Duration::from_millis(500),
            Duration::from_secs(2),
            move || {
                let state = command_state.clone();
                let worker_id = worker_id.clone();
                async move { process_command_batch(&state, &worker_id).await }
            },
        )
        .await;
    });

    let outbox_state = state.clone();
    tokio::spawn(async move {
        let worker_id = format!(
            "task-outbox:{}:{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        run_worker_loop(
            "task outbox",
            Duration::from_millis(450),
            Duration::from_millis(750),
            Duration::from_secs(3),
            move || {
                let state = outbox_state.clone();
                let worker_id = worker_id.clone();
                async move { process_outbox_batch(&state, &worker_id, notifications_enabled).await }
            },
        )
        .await;
    });

    let delivery_state = state.clone();
    tokio::spawn(async move {
        let worker_id = format!(
            "task-delivery:{}:{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        run_worker_loop(
            "task delivery",
            Duration::from_millis(700),
            Duration::from_millis(750),
            Duration::from_secs(3),
            move || {
                let state = delivery_state.clone();
                let worker_id = worker_id.clone();
                async move { process_delivery_batch(&state, &worker_id).await }
            },
        )
        .await;
    });
    if !notifications_enabled {
        tracing::info!(
            mode = notification_mode,
            "WatchDog notification outbox defaults to non-delivery mode; tenant overrides remain active"
        );
    }

    let projector_state = state.clone();
    tokio::spawn(async move {
        run_worker_loop(
            "task projector",
            Duration::from_millis(1_200),
            Duration::from_secs(2),
            Duration::from_secs(5),
            move || {
                let state = projector_state.clone();
                async move { reconcile_active_task_projections(&state).await }
            },
        )
        .await;
    });

    let maintenance_state = state.clone();
    tokio::spawn(async move {
        run_worker_loop(
            "task claim recovery",
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(30),
            move || {
                let state = maintenance_state.clone();
                async move { recover_task_control_claims(&state).await }
            },
        )
        .await;
    });

    tokio::spawn(async move {
        run_worker_loop(
            "task guard",
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(60),
            move || {
                let state = state.clone();
                async move { emit_task_guard_events(&state).await }
            },
        )
        .await;
    });
}

async fn run_worker_loop<F, Fut>(
    name: &'static str,
    initial_delay: Duration,
    interval: Duration,
    max_idle_interval: Duration,
    mut run: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<usize>>,
{
    tokio::time::sleep(initial_delay).await;
    let mut next_delay = interval;
    loop {
        let permit = match watchdog_worker_db_slots().clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                tracing::error!(worker = name, "WatchDog DB concurrency gate closed");
                return;
            }
        };
        let mut transient_attempt = 0_u32;
        let result = loop {
            match run().await {
                Err(error) if is_sqlite_transient_lock_error(&error) && transient_attempt < 8 => {
                    transient_attempt += 1;
                    let retry_delay = Duration::from_millis(25 * (1_u64 << transient_attempt));
                    tracing::debug!(
                        worker = name,
                        attempt = transient_attempt,
                        retry_delay_ms = retry_delay.as_millis(),
                        "WatchDog worker hit transient SQLite contention; retrying"
                    );
                    tokio::time::sleep(retry_delay).await;
                }
                result => break result,
            }
        };
        drop(permit);
        match result {
            Ok(processed) => {
                next_delay = next_worker_delay(next_delay, interval, max_idle_interval, processed);
            }
            Err(error) if is_sqlite_transient_lock_error(&error) => {
                tracing::debug!(worker = name, error = %error, "WatchDog worker SQLite contention persisted after retries; backing off");
                next_delay = Duration::from_millis(500).min(max_idle_interval);
            }
            Err(error) => {
                tracing::error!(worker = name, error = %error, error_debug = ?error, "WatchDog worker iteration failed");
                next_delay = next_delay
                    .saturating_mul(2)
                    .max(Duration::from_secs(2))
                    .min(max_idle_interval.min(Duration::from_secs(15)));
            }
        }
        tokio::time::sleep(next_delay).await;
    }
}

fn is_sqlite_transient_lock_error(error: &AppError) -> bool {
    if matches!(error, AppError::Database(sqlx::Error::PoolTimedOut)) {
        return true;
    }
    let AppError::Database(sqlx::Error::Database(database_error)) = error else {
        return false;
    };
    let code_matches = database_error
        .code()
        .as_deref()
        .is_some_and(|code| matches!(code, "5" | "6" | "SQLITE_BUSY" | "SQLITE_LOCKED"));
    let message = database_error.message().to_ascii_lowercase();
    code_matches
        || message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("database schema is locked")
        || message.contains("database is busy")
}

fn next_worker_delay(
    current: Duration,
    active_interval: Duration,
    max_idle_interval: Duration,
    processed: usize,
) -> Duration {
    if processed > 0 {
        active_interval
    } else {
        current.saturating_mul(2).min(max_idle_interval)
    }
}

async fn claim_rows(
    state: &AppState,
    table: &str,
    worker_id: &str,
    limit: i64,
) -> Result<Vec<String>> {
    if !claimed_table_supports_heartbeat(table) {
        return Err(AppError::Internal(
            "unsupported WatchDog claim table".to_string(),
        ));
    }
    let priority = if table == "agent_task_command_requests" {
        "CASE command_type WHEN 'kill' THEN 0 WHEN 'cancel' THEN 1 \
         WHEN 'provide_input' THEN 2 WHEN 'approve' THEN 2 WHEN 'reject' THEN 2 \
         WHEN 'retry' THEN 4 ELSE 3 END, "
    } else {
        ""
    };
    let select_sql = format!(
        "SELECT id FROM {table}
         WHERE status = 'queued' AND available_at <= CURRENT_TIMESTAMP
         ORDER BY {priority}available_at ASC, created_at ASC LIMIT ?"
    );
    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let candidates = sqlx::query_scalar::<sqlx::Sqlite, String>(sqlx::AssertSqlSafe(select_sql))
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
    if candidates.is_empty() {
        tx.rollback().await?;
        return Ok(Vec::new());
    }
    let mut update = QueryBuilder::<Sqlite>::new(format!(
        "UPDATE {table} SET status = 'claimed', claimed_by = "
    ));
    update
        .push_bind(worker_id)
        .push(", claimed_at = CURRENT_TIMESTAMP, lease_expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ")
        .push_bind(LEASE_SECONDS)
        .push(")), attempt_count = attempt_count + 1, updated_at = CURRENT_TIMESTAMP WHERE id IN (");
    let mut ids = update.separated(", ");
    for id in &candidates {
        ids.push_bind(id);
    }
    ids.push_unseparated(") AND status = 'queued' AND available_at <= CURRENT_TIMESTAMP");
    update.build().execute(&mut *tx).await?;

    let mut selected =
        QueryBuilder::<Sqlite>::new(format!("SELECT id FROM {table} WHERE claimed_by = "));
    selected.push_bind(worker_id).push(" AND id IN (");
    let mut ids = selected.separated(", ");
    for id in &candidates {
        ids.push_bind(id);
    }
    ids.push_unseparated(")");
    let claimed = selected
        .build_query_scalar::<String>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(claimed)
}

pub(crate) async fn recover_expired_claims(state: &AppState, table: &str) -> Result<()> {
    if table == "agent_notification_deliveries" {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_notification_deliveries
             SET status = 'unknown', claimed_by = NULL, claimed_at = NULL,
                 lease_expires_at = NULL,
                 last_error = 'delivery receipt is unknown after worker lease expired; manual replay may duplicate an externally accepted message',
                 updated_at = CURRENT_TIMESTAMP
             WHERE status = 'claimed' AND lease_expires_at < CURRENT_TIMESTAMP
               AND dispatch_started_at IS NOT NULL",
        )
        .execute(state.control_db())
        .await?;
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_notification_deliveries
             SET status = 'queued', claimed_by = NULL, claimed_at = NULL,
                 lease_expires_at = NULL, available_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE status = 'claimed' AND lease_expires_at < CURRENT_TIMESTAMP
               AND dispatch_started_at IS NULL",
        )
        .execute(state.control_db())
        .await?;
    } else {
        if table != "agent_task_command_requests" {
            return Err(AppError::Internal(
                "unsupported WatchDog claim recovery table".to_string(),
            ));
        }
        let recover_sql = format!(
            "UPDATE {table}
             SET status = 'queued', claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
                 available_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE status = 'claimed' AND lease_expires_at < CURRENT_TIMESTAMP"
        );
        sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(recover_sql))
            .execute(state.control_db())
            .await?;
    }
    Ok(())
}

async fn recover_expired_outbox_claims(state: &AppState) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_task_outbox
         SET status = 'pending', claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
             available_at = CURRENT_TIMESTAMP
         WHERE status = 'claimed' AND lease_expires_at < CURRENT_TIMESTAMP",
    )
    .execute(state.control_db())
    .await?;
    Ok(())
}

async fn recover_task_control_claims(state: &AppState) -> Result<usize> {
    recover_expired_claims(state, "agent_task_command_requests").await?;
    recover_expired_outbox_claims(state).await?;
    recover_expired_claims(state, "agent_notification_deliveries").await?;
    Ok(0)
}

fn claimed_table_supports_heartbeat(table: &str) -> bool {
    matches!(
        table,
        "agent_task_command_requests" | "agent_notification_deliveries"
    )
}

async fn run_with_claim_heartbeat<T, F>(
    state: &AppState,
    table: &str,
    id: &str,
    worker_id: &str,
    future: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    if !claimed_table_supports_heartbeat(table) {
        return Err(AppError::Internal(
            "unsupported WatchDog claim heartbeat table".to_string(),
        ));
    }
    let update_sql = Arc::new(format!(
        "UPDATE {table}
         SET lease_expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)), updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?"
    ));
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(LEASE_HEARTBEAT_SECONDS),
        Duration::from_secs(LEASE_HEARTBEAT_SECONDS),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = heartbeat.tick() => {
                let renewed = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(update_sql.clone()))
                    .bind(LEASE_SECONDS)
                    .bind(id)
                    .bind(worker_id)
                    .execute(state.control_db())
                    .await?;
                if renewed.rows_affected() != 1 {
                    return Err(AppError::Conflict(format!(
                        "WatchDog claim lease was lost for {table}:{id}"
                    )));
                }
            }
        }
    }
}

async fn process_command_batch(state: &AppState, worker_id: &str) -> Result<usize> {
    let ids = claim_rows(
        state,
        "agent_task_command_requests",
        worker_id,
        COMMAND_BATCH,
    )
    .await?;
    let count = ids.len();
    for id in ids {
        if let Err(error) = run_with_claim_heartbeat(
            state,
            "agent_task_command_requests",
            &id,
            worker_id,
            execute_claimed_command(state, worker_id, &id),
        )
        .await
        {
            retry_or_fail_claim(
                state,
                "agent_task_command_requests",
                &id,
                worker_id,
                &error.to_string(),
            )
            .await?;
        }
    }
    Ok(count)
}

async fn command_actor_can_control(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    crate::routes::task_control::actor_can_control_task(state, tenant_id, task_id, actor_user_id)
        .await
}

fn command_state_version_matches(
    expected: Option<u64>,
    current: u64,
    command_type: &str,
    task_status: &str,
) -> bool {
    expected.is_none_or(|version| current == version.saturating_add(1))
        || (matches!(command_type, "cancel" | "kill")
            && (task_status == "cancelling" || is_terminal(task_status)))
}

async fn execute_claimed_command(state: &AppState, worker_id: &str, id: &str) -> Result<()> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, task_id, actor_user_id, actor_type, command_type,
                expected_state_version, CAST(input_json AS TEXT) AS input_json
         FROM agent_task_command_requests
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(id)
    .bind(worker_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::Conflict("task command lease was lost".to_string()))?;
    let tenant_id: String = row.get("tenant_id");
    let task_id: String = row.get("task_id");
    let actor_user_id: String = row.get("actor_user_id");
    let actor_type: String = row.get("actor_type");
    let command_type: String = row.get("command_type");
    let input = row
        .get::<Option<String>, _>("input_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());

    if !command_actor_can_control(state, &tenant_id, &task_id, &actor_user_id).await? {
        return finish_command(
            state,
            id,
            worker_id,
            false,
            json!({ "code": "permission_revoked" }),
            Some("actor is no longer allowed to control this task"),
        )
        .await;
    }
    let task = sqlx::query::<sqlx::Sqlite>(
        "SELECT status, state_version, linked_resource_type, linked_resource_id, owner_user_id
         FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(&tenant_id)
    .bind(&task_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("task not found".to_string()))?;
    let status: String = task.get("status");
    let expected: Option<u64> = row.get("expected_state_version");
    let current_version: u64 = task.get("state_version");
    if !command_state_version_matches(expected, current_version, &command_type, &status) {
        return finish_command(
            state,
            id,
            worker_id,
            false,
            json!({ "code": "state_version_conflict", "currentVersion": current_version }),
            Some("task state changed after this command was accepted"),
        )
        .await;
    }

    let audit = WatchDogActionAudit::command(actor_user_id.clone(), format!("command:{id}"));
    let result = match command_type.as_str() {
        "cancel" | "kill" if is_terminal(&status) => {
            json!({ "status": status, "alreadyTerminal": true })
        }
        "cancel" | "kill" => {
            agent_ops::cancel_agent_task_by_id(state, &tenant_id, &task_id, &audit).await?;
            if command_type == "kill" {
                agent_ops::add_event(
                    state,
                    &tenant_id,
                    &task_id,
                    "force_cancel_requested",
                    None,
                    Some("cancelling"),
                    "warn",
                    "已向目标模型流、工具执行和子任务发送强制取消信号",
                    Some(json!({ "commandId": id, "actorType": actor_type.clone() })),
                )
                .await?;
            }
            json!({
                "status": if command_type == "kill" { "force_cancel_requested" } else { "cancel_requested" }
            })
        }
        "retry"
            if matches!(
                status.as_str(),
                "failed" | "cancelled" | "timed_out" | "stale"
            ) =>
        {
            agent_ops::request_retry_agent_task_by_id(state, &tenant_id, &task_id, &audit).await?
        }
        "retry" => {
            return finish_command(
                state,
                id,
                worker_id,
                false,
                json!({ "code": "invalid_task_state", "status": status }),
                Some("only failed, cancelled, timed out, or stale tasks can be retried"),
            )
            .await;
        }
        "provide_input" if status == "waiting_input" => {
            let Some(input) = input.filter(|value| !value.is_null()) else {
                return finish_command(
                    state,
                    id,
                    worker_id,
                    false,
                    json!({ "code": "input_required" }),
                    Some("provide_input requires a non-empty input payload"),
                )
                .await;
            };
            #[cfg(not(feature = "bot-agents"))]
            let _ = &input;
            #[cfg(feature = "bot-agents")]
            {
                match crate::routes::bot_agents::resume_waiting_bot_task_from_agent_ops(
                    state, &tenant_id, &task_id, id, &input,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error)
                        if matches!(
                            &error,
                            AppError::ValidationError(_) | AppError::NotFound(_)
                        ) =>
                    {
                        let message = error.to_string();
                        return finish_command(
                            state,
                            id,
                            worker_id,
                            false,
                            json!({ "code": "adapter_does_not_support_input_resume" }),
                            Some(&message),
                        )
                        .await;
                    }
                    Err(error) => return Err(error),
                }
            }
            #[cfg(not(feature = "bot-agents"))]
            {
                return finish_command(
                    state,
                    id,
                    worker_id,
                    false,
                    json!({ "code": "adapter_does_not_support_input_resume" }),
                    Some(
                        "the current execution adapter cannot safely resume with supplemental input",
                    ),
                )
                .await;
            }
        }
        "approve" | "reject" if status == "waiting_approval" => {
            let decision = command_type.as_str();
            let resource_type: Option<String> = task.get("linked_resource_type");
            let resource_id: Option<String> = task.get("linked_resource_id");
            let _owner_user_id: Option<String> = task.get("owner_user_id");
            let adapter_result = match (decision, resource_type.as_deref(), resource_id.as_deref())
            {
                ("approve", Some("rd_task"), Some(_resource_id)) => {
                    #[cfg(feature = "rd")]
                    {
                        let claims = agent_ops::claims_for_task_owner(
                            state,
                            &tenant_id,
                            _owner_user_id.as_deref(),
                        )
                        .await?;
                        crate::routes::rd::approve_task_from_agent_ops(
                            state.clone(),
                            claims,
                            _resource_id,
                        )
                        .await
                    }
                    #[cfg(not(feature = "rd"))]
                    {
                        Err(AppError::ValidationError(
                            "RD approval requires the web-server rd feature".to_string(),
                        ))
                    }
                }
                ("reject", Some(_), Some(_)) => {
                    agent_ops::cancel_agent_task_by_id(state, &tenant_id, &task_id, &audit)
                        .await
                        .map(|()| json!({ "status": "rejected" }))
                }
                (_, Some(other), Some(_)) => Err(AppError::ValidationError(format!(
                    "approval is not supported for linked resource type '{other}'"
                ))),
                _ => Err(AppError::ValidationError(
                    "task has no execution resource that supports approval".to_string(),
                )),
            };
            let adapter_result = match adapter_result {
                Ok(value) => value,
                Err(error) => {
                    return finish_command(
                        state,
                        id,
                        worker_id,
                        false,
                        json!({ "code": "approval_adapter_failed" }),
                        Some(&error.to_string()),
                    )
                    .await;
                }
            };
            agent_ops::sync_linked_resource_status(state, &tenant_id, &task_id).await?;
            let projected_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
                "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
            )
            .bind(&tenant_id)
            .bind(&task_id)
            .fetch_one(state.control_db())
            .await?;
            agent_ops::add_event(
                state,
                &tenant_id,
                &task_id,
                if decision == "approve" {
                    "approved"
                } else {
                    "rejected"
                },
                None,
                Some(&projected_status),
                if decision == "approve" {
                    "info"
                } else {
                    "warn"
                },
                if decision == "approve" {
                    "用户审批已通过"
                } else {
                    "用户审批已拒绝"
                },
                Some(json!({
                    "commandId": id,
                    "actorType": actor_type,
                    "input": input,
                    "adapterResult": adapter_result,
                })),
            )
            .await?;
            json!({ "status": projected_status, "adapterResult": adapter_result })
        }
        "pause" | "resume" => {
            return finish_command(
                state,
                id,
                worker_id,
                false,
                json!({ "code": "adapter_does_not_support_pause" }),
                Some("the current execution adapter cannot pause and resume safely"),
            )
            .await;
        }
        _ => {
            return finish_command(
                state,
                id,
                worker_id,
                false,
                json!({ "code": "invalid_task_state", "status": status }),
                Some("command is not valid for the current task state"),
            )
            .await;
        }
    };
    finish_command(state, id, worker_id, true, result, None).await
}

async fn finish_command(
    state: &AppState,
    id: &str,
    worker_id: &str,
    succeeded: bool,
    result: Value,
    error: Option<&str>,
) -> Result<()> {
    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, task_id, command_type, CAST(input_json AS TEXT) AS input_json
         FROM agent_task_command_requests
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(id)
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    let tenant_id: String = row.get("tenant_id");
    let task_id: String = row.get("task_id");
    let command_type: String = row.get("command_type");
    let watch_rule_run_id = row
        .get::<Option<String>, _>("input_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("watchRuleRunId").and_then(Value::as_u64));
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_task_command_requests
         SET status = ?, result_json = ?, error_message = ?, completed_at = CURRENT_TIMESTAMP,
             claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?",
    )
    .bind(if succeeded { "succeeded" } else { "failed" })
    .bind(result.to_string())
    .bind(error)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks SET state_version = state_version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&tenant_id)
    .bind(&task_id)
    .execute(&mut *tx)
    .await?;
    crate::routes::task_control::insert_outbox_tx(
        &mut tx,
        &tenant_id,
        &task_id,
        if succeeded {
            "task.command_succeeded"
        } else {
            "task.command_failed"
        },
        "owner",
        json!({
            "schemaVersion": 1,
            "actorType": "worker",
            "commandId": id,
            "commandType": command_type,
            "result": result,
            "error": error,
        }),
    )
    .await?;
    if let Some(run_id) = watch_rule_run_id {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_watch_rule_runs
             SET action_status = ?, reason_code = ?
             WHERE tenant_id = ? AND task_id = ? AND id = ?
               AND action_status = 'action_queued'",
        )
        .bind(if succeeded { "executed" } else { "failed" })
        .bind(if succeeded {
            "command_succeeded"
        } else {
            "command_failed"
        })
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(crate::sqlite_i64(run_id))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn retry_or_fail_claim(
    state: &AppState,
    table: &str,
    id: &str,
    worker_id: &str,
    error: &str,
) -> Result<()> {
    if !claimed_table_supports_heartbeat(table) {
        return Err(AppError::Internal(
            "unsupported WatchDog retry table".to_string(),
        ));
    }
    let error_column = if table == "agent_task_command_requests" {
        "error_message"
    } else {
        "last_error"
    };
    let sql = if table == "agent_notification_deliveries" {
        format!(
            "UPDATE {table}
             SET status = CASE
                   WHEN dispatch_started_at IS NOT NULL THEN 'unknown'
                   WHEN attempt_count >= max_attempts THEN 'failed'
                   ELSE 'queued'
                 END,
                 available_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)),
                 claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
                 {error_column} = CASE
                   WHEN dispatch_started_at IS NOT NULL
                   THEN ('delivery receipt is unknown; manual replay may duplicate an externally accepted message: ' || ?)
                   ELSE ?
                 END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'claimed' AND claimed_by = ?"
        )
    } else {
        format!(
            "UPDATE {table}
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'failed' ELSE 'queued' END,
             available_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)),
             claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
             {error_column} = ?, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?"
        )
    };
    let error = error.chars().take(3500).collect::<String>();
    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let attempt_count = sqlx::query_scalar::<sqlx::Sqlite, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT attempt_count FROM {table} WHERE id = ? AND status = 'claimed' AND claimed_by = ?"
    )))
    .bind(id)
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(attempt_count) = attempt_count else {
        tx.rollback().await?;
        return Ok(());
    };
    let mut query = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(sql))
        .bind(retry_backoff_seconds(attempt_count))
        .bind(&error);
    if table == "agent_notification_deliveries" {
        query = query.bind(&error);
    }
    query.bind(id).bind(worker_id).execute(&mut *tx).await?;
    tx.commit().await?;
    if table == "agent_task_command_requests" {
        let failed = sqlx::query::<sqlx::Sqlite>(
            "SELECT tenant_id, task_id, CAST(input_json AS TEXT) AS input_json
             FROM agent_task_command_requests WHERE id = ? AND status = 'failed'",
        )
        .bind(id)
        .fetch_optional(state.control_db())
        .await?;
        if let Some(row) = failed {
            let run_id = row
                .get::<Option<String>, _>("input_json")
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| value.get("watchRuleRunId").and_then(Value::as_u64));
            if let Some(run_id) = run_id {
                sqlx::query::<sqlx::Sqlite>(
                    "UPDATE agent_watch_rule_runs
                     SET action_status = 'failed', reason_code = 'command_failed'
                     WHERE tenant_id = ? AND task_id = ? AND id = ?
                       AND action_status = 'action_queued'",
                )
                .bind(row.get::<String, _>("tenant_id"))
                .bind(row.get::<String, _>("task_id"))
                .bind(crate::sqlite_i64(run_id))
                .execute(state.control_db())
                .await?;
            }
        }
    } else if table == "agent_notification_deliveries" {
        update_failed_delivery_watch_rule(state, id).await?;
    }
    Ok(())
}

async fn update_failed_delivery_watch_rule(state: &AppState, id: &str) -> Result<()> {
    let failed = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, CAST(payload_json AS TEXT) AS payload_json
         FROM agent_notification_deliveries WHERE id = ? AND status = 'failed'",
    )
    .bind(id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(row) = failed else {
        return Ok(());
    };
    let payload = row
        .get::<Option<String>, _>("payload_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    if payload.get("confirmationRequired").and_then(Value::as_bool) == Some(false) {
        if let Some(run_id) = payload.get("watchRuleRunId").and_then(Value::as_u64) {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE agent_watch_rule_runs
                 SET action_status = 'failed', reason_code = 'delivery_failed'
                 WHERE tenant_id = ? AND id = ?
                   AND action_status IN ('queued','action_queued')",
            )
            .bind(row.get::<String, _>("tenant_id"))
            .bind(crate::sqlite_i64(run_id))
            .execute(state.control_db())
            .await?;
        }
    }
    Ok(())
}

async fn process_outbox_batch(
    state: &AppState,
    worker_id: &str,
    notifications_enabled: bool,
) -> Result<usize> {
    let ids = claim_outbox_rows(state, worker_id).await?;
    let count = ids.len();
    for id in ids {
        if let Err(error) =
            materialize_outbox_deliveries(state, worker_id, id, notifications_enabled).await
        {
            retry_outbox(state, worker_id, id, &error.to_string()).await?;
        }
    }
    Ok(count)
}

async fn claim_outbox_rows(state: &AppState, worker_id: &str) -> Result<Vec<u64>> {
    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let ids = sqlx::query_scalar::<sqlx::Sqlite, u64>(
        "SELECT id FROM agent_task_outbox
         WHERE status = 'pending' AND available_at <= CURRENT_TIMESTAMP
         ORDER BY available_at ASC, id ASC LIMIT ?",
    )
    .bind(OUTBOX_BATCH)
    .fetch_all(&mut *tx)
    .await?;
    if ids.is_empty() {
        tx.rollback().await?;
        return Ok(Vec::new());
    }
    let mut update = QueryBuilder::<Sqlite>::new(
        "UPDATE agent_task_outbox SET status = 'claimed', claimed_by = ",
    );
    update
        .push_bind(worker_id)
        .push(", claimed_at = CURRENT_TIMESTAMP, lease_expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ")
        .push_bind(LEASE_SECONDS)
        .push(")), attempt_count = attempt_count + 1 WHERE id IN (");
    let mut values = update.separated(", ");
    for id in &ids {
        values.push_bind(crate::sqlite_i64(*id));
    }
    values.push_unseparated(") AND status = 'pending' AND available_at <= CURRENT_TIMESTAMP");
    update.build().execute(&mut *tx).await?;

    let mut selected =
        QueryBuilder::<Sqlite>::new("SELECT id FROM agent_task_outbox WHERE claimed_by = ");
    selected.push_bind(worker_id).push(" AND id IN (");
    let mut values = selected.separated(", ");
    for id in &ids {
        values.push_bind(crate::sqlite_i64(*id));
    }
    values.push_unseparated(")");
    let claimed = selected
        .build_query_scalar::<u64>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(claimed)
}

async fn materialize_outbox_deliveries(
    state: &AppState,
    worker_id: &str,
    outbox_id: u64,
    default_notifications_enabled: bool,
) -> Result<()> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT outbox.tenant_id, outbox.task_id, outbox.root_task_id,
                outbox.event_type, outbox.state_version,
                CAST(outbox.payload_json AS TEXT) AS payload_json,
                root.title, root.short_code, root.owner_user_id, root.initiator_user_id,
                root.source AS root_source,
                root.origin_session_id, root.origin_turn_id, root.external_channel_id,
                root.external_conversation_id, at.capability_key, at.status,
                CASE
                  WHEN at.sensitivity_label IN ('confidential','restricted','secret')
                  THEN at.sensitivity_label ELSE root.sensitivity_label
                END AS sensitivity_label,
                CAST((julianday(CURRENT_TIMESTAMP) - julianday(root.created_at)) * 86400 AS INTEGER) AS elapsed_seconds,
                CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(at.last_progress_at, at.created_at))) * 86400 AS INTEGER) AS idle_seconds,
                CAST(at.cost_json AS TEXT) AS cost_json
         FROM agent_task_outbox outbox
         INNER JOIN agent_tasks at ON at.tenant_id = outbox.tenant_id AND at.id = outbox.task_id
         INNER JOIN agent_tasks root
           ON root.tenant_id = outbox.tenant_id AND root.id = outbox.root_task_id
         WHERE outbox.id = ? AND outbox.status = 'claimed' AND outbox.claimed_by = ?",
    )
    .bind(crate::sqlite_i64(outbox_id))
    .bind(worker_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::Conflict("task outbox lease was lost".to_string()))?;
    let tenant_id: String = row.get("tenant_id");
    let default_mode = if default_notifications_enabled {
        "on"
    } else {
        "shadow"
    };
    let notifications_enabled = crate::routes::task_control::watchdog_feature_mode(
        state,
        &tenant_id,
        "watchdog_notification_outbox",
        default_mode,
    )
    .await?
        == "on";
    let task_id: String = row.get("task_id");
    let root_task_id: String = row.get("root_task_id");
    let event_type: String = row.get("event_type");
    let owner_user_id = row
        .get::<Option<String>, _>("initiator_user_id")
        .or_else(|| row.get::<Option<String>, _>("owner_user_id"));
    let payload = row
        .get::<Option<String>, _>("payload_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let title: String = row.get("title");
    let short_code: Option<String> = row.get("short_code");
    let sensitivity_label: String = row.get("sensitivity_label");
    let session_id: Option<String> = row.get("origin_session_id");
    let turn_id: Option<String> = row.get("origin_turn_id");
    let protected_metadata = notification_requires_metadata_only(&sensitivity_label);
    let notification_title = if protected_metadata {
        "AOS 受保护任务通知".to_string()
    } else {
        title.clone()
    };
    let body = if protected_metadata {
        protected_notification_body(&event_type, short_code.as_deref())
    } else {
        sanitize_notification_text(&event_body(
            &event_type,
            &title,
            short_code.as_deref(),
            &payload,
        ))
    };
    let deep_link = session_id.as_deref().map(|session| {
        format!(
            "/super-assistant?sessionId={}&turnId={}",
            urlencoding::encode(session),
            urlencoding::encode(turn_id.as_deref().unwrap_or_default())
        )
    });
    let delivery_payload = json!({
        "schemaVersion": 1,
        "taskId": root_task_id,
        "sourceTaskId": task_id,
        "shortCode": short_code,
        "eventType": event_type,
        "title": notification_title,
        "body": body,
        "deepLink": deep_link,
        "sensitivityLabel": sensitivity_label,
        "externalConversationId": row.get::<Option<String>, _>("external_conversation_id"),
    });
    let original_channel: Option<String> = row.get("external_channel_id");
    let direct_reply_channel =
        if event_type == "task.completed" && row.get::<String, _>("root_source") == "bot" {
            if let Some(channel_id) = original_channel.as_deref() {
                let direct_reply_sent = sqlx::query_scalar::<sqlx::Sqlite, i64>(
                    "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_events
                 WHERE tenant_id = ? AND task_id = ? AND event_type = 'bot.outbound'
                   AND status = 'running'
                   AND json_extract(metadata_json, '$.channelId') = ?",
                )
                .bind(&tenant_id)
                .bind(&root_task_id)
                .bind(channel_id)
                .fetch_one(state.control_db())
                .await?
                    > 0;
                suppress_original_bot_completion_notification(
                    &event_type,
                    &row.get::<String, _>("root_source"),
                    direct_reply_sent,
                )
                .then(|| channel_id.to_string())
            } else {
                None
            }
        } else {
            None
        };
    let mobile_follow_destination = if notifications_enabled
        && should_create_automatic_delivery(&event_type, row.get("elapsed_seconds"))
        && original_channel.is_none()
    {
        if let Some(owner) = owner_user_id.as_deref() {
            resolve_mobile_follow_destination(state, &tenant_id, owner).await?
        } else {
            None
        }
    } else {
        None
    };

    evaluate_watch_rules(
        state,
        outbox_id,
        &tenant_id,
        &task_id,
        &root_task_id,
        owner_user_id.as_deref(),
        &event_type,
        &row,
        &delivery_payload,
        notifications_enabled,
    )
    .await?;
    if !notifications_enabled {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_outbox
             SET status = 'published', published_at = CURRENT_TIMESTAMP, claimed_by = NULL,
                 claimed_at = NULL, lease_expires_at = NULL,
                 last_error = 'shadow mode: delivery suppressed'
             WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
        )
        .bind(crate::sqlite_i64(outbox_id))
        .bind(worker_id)
        .execute(state.control_db())
        .await?;
        return Ok(());
    }

    let subscriptions = sqlx::query::<sqlx::Sqlite>(
        "SELECT subscriptions.id, subscriptions.user_id,
                CAST(subscriptions.event_types_json AS TEXT) AS event_types_json,
                subscriptions.destination_type, subscriptions.destination_ref,
                CAST(subscriptions.policy_json AS TEXT) AS policy_json
         FROM agent_task_subscriptions subscriptions
         INNER JOIN agent_tasks root
           ON root.tenant_id = subscriptions.tenant_id AND root.id = ?
         WHERE subscriptions.tenant_id = ? AND subscriptions.enabled = 1
           AND (subscriptions.task_id = ? OR subscriptions.task_id = ?
                OR subscriptions.task_id IS NULL)
           AND (
             COALESCE(root.initiator_user_id, root.owner_user_id) = subscriptions.user_id
             OR root.owner_user_id = subscriptions.user_id
             OR EXISTS (
               SELECT 1 FROM agent_task_grants grants
               WHERE grants.tenant_id = root.tenant_id AND grants.task_id = root.id
                 AND grants.grantee_type = 'user'
                 AND grants.grantee_id = subscriptions.user_id
                 AND grants.revoked_at IS NULL
             )
           )",
    )
    .bind(&root_task_id)
    .bind(&tenant_id)
    .bind(&task_id)
    .bind(&root_task_id)
    .fetch_all(state.control_db())
    .await?;
    // Resolve policies before opening the delivery transaction. The previous
    // implementation held one connection while each onlyWhenAway subscription
    // acquired another connection from the shared pool (an N+1 double-acquire).
    let mut eligible_subscriptions = Vec::with_capacity(subscriptions.len());
    for subscription in subscriptions {
        let event_types = subscription
            .get::<Option<String>, _>("event_types_json")
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default();
        if !event_types
            .iter()
            .any(|item| item == "*" || event_type_matches(item, &event_type))
        {
            continue;
        }
        let policy = subscription
            .get::<Option<String>, _>("policy_json")
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        if subscription_suppressed(state, &tenant_id, &subscription, policy.as_ref()).await? {
            continue;
        }
        eligible_subscriptions.push(subscription);
    }

    #[cfg(any(feature = "agent", feature = "bot-agents", feature = "pm"))]
    let configured_channels = sqlx::query::<sqlx::Sqlite>(
        "SELECT channels.id, CAST(channels.config_json AS TEXT) AS config_json
         FROM bot_agent_channels channels
         INNER JOIN bot_agents agents
           ON agents.tenant_id = channels.tenant_id AND agents.id = channels.agent_id
         WHERE channels.tenant_id = ? AND channels.enabled = 1 AND agents.enabled = 1",
    )
    .bind(&tenant_id)
    .fetch_all(state.control_db())
    .await?
    .into_iter()
    .filter_map(|channel| {
        let config = channel
            .get::<Option<String>, _>("config_json")
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        crate::routes::bot_agents_outbound::notification_event_enabled(config.as_ref(), &event_type)
            .then(|| channel.get::<String, _>("id"))
    })
    .collect::<Vec<_>>();
    #[cfg(not(any(feature = "agent", feature = "bot-agents", feature = "pm")))]
    let configured_channels = Vec::<String>::new();
    let mut bot_destinations = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT channel_id FROM agent_notification_deliveries
         WHERE tenant_id = ? AND outbox_id = ? AND platform = 'bot'
           AND channel_id IS NOT NULL",
    )
    .bind(&tenant_id)
    .bind(crate::sqlite_i64(outbox_id))
    .fetch_all(state.control_db())
    .await?
    .into_iter()
    .collect::<HashSet<_>>();
    if let Some(channel_id) = direct_reply_channel {
        bot_destinations.insert(channel_id);
    }

    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    // Watch rules are materialized before subscriptions. Count those deliveries
    // as well so the automatic important-event fallback cannot create the same
    // WebUI notification a second time for the owner.
    let mut has_owner_webui = if let Some(owner) = owner_user_id.as_deref() {
        sqlx::query_scalar::<sqlx::Sqlite, i64>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND user_id = ? AND platform = 'webui'",
        )
        .bind(&tenant_id)
        .bind(crate::sqlite_i64(outbox_id))
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?
            > 0
    } else {
        false
    };
    for subscription in eligible_subscriptions {
        let subscription_id: String = subscription.get("id");
        let user_id: String = subscription.get("user_id");
        let destination_type: String = subscription.get("destination_type");
        let destination_ref: Option<String> = subscription.get("destination_ref");
        let is_owner_webui =
            destination_type == "webui" && owner_user_id.as_deref() == Some(&user_id);
        if destination_type == "bot"
            && destination_ref
                .as_ref()
                .is_some_and(|channel_id| bot_destinations.contains(channel_id))
        {
            continue;
        }
        let insert_result = insert_delivery_tx(
            &mut tx,
            outbox_id,
            &tenant_id,
            &root_task_id,
            Some(&subscription_id),
            &user_id,
            &destination_type,
            destination_ref.as_deref(),
            true,
            delivery_payload.clone(),
            None,
        )
        .await;
        match insert_result {
            Ok(()) => {
                has_owner_webui |= is_owner_webui;
                if destination_type == "bot" {
                    if let Some(channel_id) = destination_ref {
                        bot_destinations.insert(channel_id);
                    }
                }
            }
            Err(error) => {
                if !permanent_delivery_error(&error) {
                    return Err(error);
                }
                sqlx::query::<sqlx::Sqlite>(
                    "UPDATE agent_task_subscriptions SET enabled = 0, updated_at = CURRENT_TIMESTAMP
                     WHERE tenant_id = ? AND id = ? AND user_id = ?",
                )
                .bind(&tenant_id)
                .bind(&subscription_id)
                .bind(&user_id)
                .execute(&mut *tx)
                .await?;
                tracing::warn!(
                    tenant_id,
                    task_id,
                    subscription_id,
                    error = %error,
                    "disabled invalid task notification subscription without blocking other deliveries"
                );
            }
        }
    }
    if let Some(owner) = owner_user_id.as_deref() {
        for channel_id in configured_channels {
            if bot_destinations.contains(&channel_id) {
                continue;
            }
            let delivery_result = insert_delivery_tx(
                &mut tx,
                outbox_id,
                &tenant_id,
                &root_task_id,
                None,
                owner,
                "bot",
                Some(&channel_id),
                true,
                delivery_payload.clone(),
                Some("channel-config"),
            )
            .await;
            match delivery_result {
                Ok(()) => {
                    bot_destinations.insert(channel_id);
                }
                Err(error) if permanent_delivery_error(&error) => {
                    tracing::warn!(
                        tenant_id,
                        task_id,
                        channel_id,
                        error = %error,
                        "skipped configured Bot task notification without an active bound conversation"
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }
    if should_create_automatic_delivery(&event_type, row.get("elapsed_seconds")) {
        if let Some(owner) = owner_user_id.as_deref().filter(|_| !has_owner_webui) {
            insert_delivery_tx(
                &mut tx,
                outbox_id,
                &tenant_id,
                &root_task_id,
                None,
                owner,
                "webui",
                None,
                false,
                delivery_payload.clone(),
                None,
            )
            .await?;
        }
        if let (Some(owner), Some(channel_id)) = (
            owner_user_id.as_deref(),
            original_channel
                .as_deref()
                .filter(|channel_id| !bot_destinations.contains(*channel_id)),
        ) {
            let delivery_result = insert_delivery_tx(
                &mut tx,
                outbox_id,
                &tenant_id,
                &root_task_id,
                None,
                owner,
                "bot",
                Some(channel_id),
                true,
                delivery_payload.clone(),
                None,
            )
            .await;
            match delivery_result {
                Ok(()) => {
                    bot_destinations.insert(channel_id.to_string());
                }
                Err(error) if permanent_delivery_error(&error) => {
                    tracing::warn!(
                        tenant_id,
                        task_id,
                        channel_id,
                        error = %error,
                        "skipped original Bot task notification without an active bound conversation"
                    );
                }
                Err(error) => return Err(error),
            }
        }
        if let (Some(owner), Some(destination)) = (
            owner_user_id.as_deref(),
            mobile_follow_destination
                .as_ref()
                .filter(|destination| !bot_destinations.contains(&destination.channel_id)),
        ) {
            let mut mobile_payload = delivery_payload.clone();
            mobile_payload["externalConversationId"] = json!(destination.external_conversation_id);
            insert_delivery_tx(
                &mut tx,
                outbox_id,
                &tenant_id,
                &root_task_id,
                None,
                owner,
                "bot",
                Some(&destination.channel_id),
                true,
                mobile_payload,
                Some("mobile-follow"),
            )
            .await?;
        }
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_task_outbox
         SET status = 'published', published_at = CURRENT_TIMESTAMP, claimed_by = NULL,
             claimed_at = NULL, lease_expires_at = NULL
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(crate::sqlite_i64(outbox_id))
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

fn suppress_original_bot_completion_notification(
    event_type: &str,
    root_source: &str,
    direct_reply_sent: bool,
) -> bool {
    event_type == "task.completed" && root_source == "bot" && direct_reply_sent
}

struct MobileFollowDestination {
    channel_id: String,
    external_conversation_id: String,
}

fn mobile_follow_delivery_allowed(enabled: bool, active_web_clients: i64) -> bool {
    enabled && active_web_clients == 0
}

async fn resolve_mobile_follow_destination(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<MobileFollowDestination>> {
    let enabled = sqlx::query_scalar::<sqlx::Sqlite, bool>(
        "SELECT mobile_follow_enabled FROM agent_user_presence_leases
         WHERE tenant_id = ? AND user_id = ?
         ORDER BY updated_at DESC, client_id DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?
    .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let active_web_clients: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_user_presence_leases
         WHERE tenant_id = ? AND user_id = ? AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(state.control_db())
    .await?;
    if !mobile_follow_delivery_allowed(enabled, active_web_clients) {
        return Ok(None);
    }
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT channel_id, external_conversation_id FROM agent_external_identity_links
         WHERE tenant_id = ? AND user_id = ? AND status = 'active'
           AND revoked_at IS NULL AND channel_id IS NOT NULL AND channel_id <> ''
           AND external_conversation_id IS NOT NULL AND external_conversation_id <> ''
         ORDER BY COALESCE(last_seen_at, verified_at) DESC, updated_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?;
    Ok(row.map(|row| MobileFollowDestination {
        channel_id: row.get("channel_id"),
        external_conversation_id: row.get("external_conversation_id"),
    }))
}

fn event_type_matches(configured: &str, event_type: &str) -> bool {
    configured.eq_ignore_ascii_case(event_type)
        || configured
            .strip_suffix(".*")
            .is_some_and(|prefix| event_type.starts_with(prefix))
}

fn string_list_matches(value: Option<&Value>, actual: &str) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(value)) => value == "*" || value.eq_ignore_ascii_case(actual),
        Some(Value::Array(values)) => values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value == "*" || value.eq_ignore_ascii_case(actual))
        }),
        _ => false,
    }
}

fn watch_condition_matches(
    condition: &Value,
    event_type: &str,
    status: &str,
    elapsed_seconds: i64,
    idle_seconds: i64,
    cost: Option<&Value>,
) -> bool {
    if !string_list_matches(
        condition
            .get("eventTypes")
            .or_else(|| condition.get("eventType")),
        event_type,
    ) || !string_list_matches(
        condition
            .get("statuses")
            .or_else(|| condition.get("status")),
        status,
    ) {
        return false;
    }
    if condition
        .get("minElapsedSeconds")
        .and_then(Value::as_i64)
        .is_some_and(|minimum| elapsed_seconds < minimum.max(0))
    {
        return false;
    }
    if condition
        .get("noProgressSeconds")
        .and_then(Value::as_i64)
        .is_some_and(|minimum| idle_seconds < minimum.max(0))
    {
        return false;
    }
    if let Some(minimum) = condition.get("minCostUsd").and_then(Value::as_f64) {
        let actual = cost
            .and_then(|value| value.get("totalUsd").or_else(|| value.get("costUsd")))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if actual < minimum.max(0.0) {
            return false;
        }
    }
    true
}

fn quiet_hours_active(quiet_hours: Option<&Value>) -> bool {
    let Some(quiet_hours) = quiet_hours else {
        return false;
    };
    let Some(start) = quiet_hours.get("start").and_then(Value::as_str) else {
        return false;
    };
    let Some(end) = quiet_hours.get("end").and_then(Value::as_str) else {
        return false;
    };
    fn minute_of_day(value: &str) -> Option<u32> {
        let (hour, minute) = value.split_once(':')?;
        let hour = hour.parse::<u32>().ok()?;
        let minute = minute.parse::<u32>().ok()?;
        (hour < 24 && minute < 60).then_some(hour * 60 + minute)
    }
    let (Some(start), Some(end)) = (minute_of_day(start), minute_of_day(end)) else {
        return false;
    };
    let timezone = quiet_hours
        .get("timezone")
        .and_then(Value::as_str)
        .unwrap_or("UTC")
        .parse::<chrono_tz::Tz>();
    let Ok(timezone) = timezone else {
        return false;
    };
    let now = Utc::now().with_timezone(&timezone);
    let current = now.hour() * 60 + now.minute();
    if start <= end {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
}

#[allow(clippy::too_many_arguments)]
const WATCH_RULE_DAILY_ACTION_COUNT_SQL: &str =
    "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_watch_rule_runs
     WHERE tenant_id = ? AND rule_id = ?
       AND action_status IN ('delivered','queued','awaiting_confirmation','action_queued','executed')
       AND created_at >= CURRENT_DATE";

async fn evaluate_watch_rules(
    state: &AppState,
    outbox_id: u64,
    tenant_id: &str,
    task_id: &str,
    root_task_id: &str,
    owner_user_id: Option<&str>,
    event_type: &str,
    task_row: &sqlx::sqlite::SqliteRow,
    delivery_payload: &Value,
    notifications_enabled: bool,
) -> Result<()> {
    let Some(owner_user_id) = owner_user_id else {
        return Ok(());
    };
    let capability_key: String = task_row.get("capability_key");
    let status: String = task_row.get("status");
    let elapsed_seconds: i64 = task_row.get("elapsed_seconds");
    let idle_seconds: i64 = task_row.get("idle_seconds");
    let cost = task_row
        .get::<Option<String>, _>("cost_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let rules = sqlx::query::<sqlx::Sqlite>(
        "SELECT id,
                CAST(condition_json AS TEXT) AS condition_json,
                CAST(action_json AS TEXT) AS action_json,
                CAST(quiet_hours_json AS TEXT) AS quiet_hours_json,
                max_actions_per_day, requires_confirmation
         FROM agent_watch_rules
         WHERE tenant_id = ? AND user_id = ? AND enabled = 1 AND (
           scope_type = 'own' OR (scope_type = 'task' AND scope_ref = ?)
           OR (scope_type = 'task' AND scope_ref = ?)
           OR (scope_type = 'capability' AND scope_ref = ?)
         )",
    )
    .bind(tenant_id)
    .bind(owner_user_id)
    .bind(task_id)
    .bind(root_task_id)
    .bind(&capability_key)
    .fetch_all(state.control_db())
    .await?;
    for rule in rules {
        let rule_id: String = rule.try_get("id")?;
        let condition = rule
            .try_get::<Option<String>, _>("condition_json")?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        let action = rule
            .try_get::<Option<String>, _>("action_json")?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        let quiet_hours = rule
            .try_get::<Option<String>, _>("quiet_hours_json")?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let matched = watch_condition_matches(
            &condition,
            event_type,
            &status,
            elapsed_seconds,
            idle_seconds,
            cost.as_ref(),
        );
        let mut action_status = "condition_not_matched";
        let mut reason_code = "condition_not_matched";
        let mut create_delivery = false;
        if matched && quiet_hours_active(quiet_hours.as_ref()) {
            action_status = "suppressed";
            reason_code = "quiet_hours";
        } else if matched {
            let max_actions: i64 = i64::from(rule.try_get::<u32, _>("max_actions_per_day")?);
            let action_count: i64 =
                sqlx::query_scalar::<sqlx::Sqlite, _>(WATCH_RULE_DAILY_ACTION_COUNT_SQL)
                    .bind(tenant_id)
                    .bind(&rule_id)
                    .fetch_one(state.control_db())
                    .await?;
            if action_count >= max_actions {
                action_status = "suppressed";
                reason_code = "daily_limit";
            } else if !notifications_enabled {
                action_status = "shadow";
                reason_code = "notification_shadow_mode";
            } else {
                let action_type = action
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("notify");
                let requires_confirmation: bool = rule.try_get("requires_confirmation")?;
                create_delivery = true;
                if action_type == "notify" {
                    action_status = "queued";
                    reason_code = "matched";
                } else if requires_confirmation {
                    action_status = "awaiting_confirmation";
                    reason_code = "side_effect_requires_confirmation";
                } else {
                    action_status = "suppressed";
                    reason_code = "unsafe_unconfirmed_action";
                    create_delivery = false;
                }
            }
        }
        let mut tx = state.control_db.begin().await?;
        crate::acquire_sqlite_write_lock(&mut tx).await?;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO agent_watch_rule_runs
               (tenant_id, rule_id, task_id, outbox_id, matched, action_status, reason_code, detail_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT DO UPDATE SET id = agent_watch_rule_runs.id
             RETURNING id",
        )
        .bind(tenant_id)
        .bind(&rule_id)
        .bind(task_id)
        .bind(crate::sqlite_i64(outbox_id))
        .bind(matched)
        .bind(action_status)
        .bind(reason_code)
        .bind(
            json!({
                "eventType": event_type,
                "status": status,
                "action": action,
                "deliveryPayload": delivery_payload,
            })
            .to_string(),
        )
        .fetch_one(&mut *tx)
        .await?;
        if create_delivery {
            let action_type = action
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("notify");
            let requires_confirmation = action_type != "notify";
            let destination_type = if requires_confirmation {
                "webui"
            } else {
                action
                    .get("destinationType")
                    .and_then(Value::as_str)
                    .unwrap_or("webui")
            };
            let destination_ref = if requires_confirmation {
                None
            } else {
                action.get("destinationRef").and_then(Value::as_str)
            };
            let mut payload = delivery_payload.clone();
            payload["watchRuleRunId"] = json!(run_id);
            payload["watchRuleActionType"] = json!(action_type);
            payload["confirmationRequired"] = json!(requires_confirmation);
            if requires_confirmation {
                payload["body"] = json!(format!(
                    "{}\n值守规则请求执行 {action_type}，需要你确认后才能继续。",
                    payload
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ));
            }
            let delivery_result = insert_delivery_tx(
                &mut tx,
                outbox_id,
                tenant_id,
                task_id,
                None,
                owner_user_id,
                destination_type,
                destination_ref,
                true,
                payload,
                Some(&rule_id),
            )
            .await;
            if let Err(error) = delivery_result {
                if !permanent_delivery_error(&error) {
                    return Err(error);
                }
                sqlx::query::<sqlx::Sqlite>(
                    "UPDATE agent_watch_rule_runs
                     SET action_status = 'suppressed', reason_code = 'invalid_destination'
                     WHERE tenant_id = ? AND id = ?",
                )
                .bind(tenant_id)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
                tracing::warn!(
                    tenant_id,
                    task_id,
                    rule_id,
                    run_id,
                    error = %error,
                    "suppressed invalid WatchDog rule destination without blocking task outbox"
                );
            }
        }
        tx.commit().await?;
    }
    Ok(())
}

fn is_important_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "task.failed"
            | "task.waiting_input"
            | "task.waiting_approval"
            | "task.blocked"
            | "task.stalled"
            | "task.sla_breached"
            | "task.budget_warning"
    )
}

fn should_create_automatic_delivery(event_type: &str, elapsed_seconds: i64) -> bool {
    let _ = elapsed_seconds;
    is_important_event(event_type)
}

fn event_body(event_type: &str, title: &str, short_code: Option<&str>, payload: &Value) -> String {
    let code = short_code
        .map(|value| format!(" · #{value}"))
        .unwrap_or_default();
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| match event_type {
            "task.completed" => "任务已完成",
            "task.failed" => "任务执行失败",
            "task.cancelled" => "任务已取消",
            "task.waiting_input" => "任务等待补充信息",
            "task.waiting_approval" => "任务等待审批",
            _ => "任务状态已更新",
        });
    format!("{title}{code}\n{message}")
}

fn notification_requires_metadata_only(sensitivity_label: &str) -> bool {
    matches!(
        sensitivity_label.trim().to_ascii_lowercase().as_str(),
        "confidential" | "restricted" | "secret"
    )
}

fn protected_notification_body(event_type: &str, short_code: Option<&str>) -> String {
    let code = short_code
        .map(|value| format!(" #{value}"))
        .unwrap_or_default();
    let status = match event_type {
        "task.completed" => "已完成",
        "task.failed" => "执行失败",
        "task.cancelled" => "已取消",
        "task.waiting_input" => "等待补充信息",
        "task.waiting_approval" => "等待审批",
        "task.blocked" => "已阻塞",
        _ => "状态已更新",
    };
    format!("受保护任务{code}{status}。请登录 AOS WebUI 查看详情。")
}

fn sanitize_notification_text(value: &str) -> String {
    const MARKERS: &[&str] = &[
        "sk-",
        "Bearer ",
        "bearer ",
        "password=",
        "password:",
        "passwd=",
        "api_key=",
        "apikey=",
        "secret=",
        "token=",
    ];
    let truncated = value.chars().take(8_000).collect::<String>();
    let mut output =
        runtime::protect_sensitive_text(&truncated, runtime::configured_data_protection_mode())
            .value;
    for marker in MARKERS {
        let mut search_from = 0;
        while let Some(relative) = output[search_from..].find(marker) {
            let start = search_from + relative;
            let secret_start = start + marker.len();
            let secret_len = output[secret_start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, ',' | ';' | ')' | ']' | '}')
                })
                .unwrap_or(output.len() - secret_start);
            let end = secret_start + secret_len;
            output.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }
    output
}

async fn subscription_suppressed(
    state: &AppState,
    tenant_id: &str,
    row: &sqlx::sqlite::SqliteRow,
    policy: Option<&Value>,
) -> Result<bool> {
    let only_when_away = policy
        .and_then(|value| value.get("onlyWhenAway"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !only_when_away {
        return Ok(false);
    }
    let user_id: String = row.get("user_id");
    let active: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_user_presence_leases
         WHERE tenant_id = ? AND user_id = ? AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(active > 0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_delivery_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    outbox_id: u64,
    tenant_id: &str,
    task_id: &str,
    subscription_id: Option<&str>,
    user_id: &str,
    destination_type: &str,
    destination_ref: Option<&str>,
    require_bound_conversation: bool,
    mut payload: Value,
    idempotency_scope: Option<&str>,
) -> Result<()> {
    if require_bound_conversation && matches!(destination_type, "bot" | "webhook") {
        let channel_id = destination_ref.ok_or_else(|| {
            AppError::ValidationError("external notification channel is missing".to_string())
        })?;
        let binding = sqlx::query_scalar::<sqlx::Sqlite, Option<String>>(
            "SELECT external_conversation_id FROM agent_external_identity_links
             WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
               AND status = 'active' AND revoked_at IS NULL
             ORDER BY COALESCE(last_seen_at, verified_at) DESC, updated_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(channel_id)
        .fetch_optional(&mut **tx)
        .await?;
        if binding.is_none() {
            return Err(AppError::Forbidden);
        }
        let bound_conversation = binding
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if destination_type == "bot" && bound_conversation.is_none() {
            return Err(AppError::ValidationError(
                "external notification target has no verified private conversation".to_string(),
            ));
        }
        let has_bound_conversation = bound_conversation.is_some();
        payload["externalConversationId"] = match bound_conversation {
            Some(conversation_id) => json!(conversation_id),
            None => Value::Null,
        };
        payload["requireExactConversationBinding"] = json!(has_bound_conversation);
    } else if destination_type == "webui" {
        payload["externalConversationId"] = Value::Null;
    }
    let idempotency_key = delivery_idempotency_key(
        outbox_id,
        idempotency_scope.or(subscription_id).unwrap_or("automatic"),
        destination_type,
        destination_ref.unwrap_or(user_id),
    );
    sqlx::query::<sqlx::Sqlite>(
        "INSERT OR IGNORE INTO agent_notification_deliveries
           (id, tenant_id, outbox_id, subscription_id, task_id, user_id, platform,
            channel_id, external_conversation_id, idempotency_key, payload_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(format!("agtdel-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(crate::sqlite_i64(outbox_id))
    .bind(subscription_id)
    .bind(task_id)
    .bind(user_id)
    .bind(destination_type)
    .bind(destination_ref)
    .bind(
        payload
            .get("externalConversationId")
            .and_then(Value::as_str),
    )
    .bind(idempotency_key)
    .bind(payload.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn delivery_idempotency_key(
    outbox_id: u64,
    scope: &str,
    destination_type: &str,
    destination: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(outbox_id.to_be_bytes());
    for value in [scope, destination_type, destination] {
        hasher.update([0]);
        hasher.update(value.as_bytes());
    }
    format!("delivery:{}", hex::encode(hasher.finalize()))
}

async fn retry_outbox(state: &AppState, worker_id: &str, id: u64, error: &str) -> Result<()> {
    let mut tx = state.control_db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let attempt_count = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT attempt_count FROM agent_task_outbox
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(crate::sqlite_i64(id))
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(attempt_count) = attempt_count else {
        tx.rollback().await?;
        return Ok(());
    };
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_task_outbox
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'failed' ELSE 'pending' END,
             available_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)),
             claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL, last_error = ?
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(retry_backoff_seconds(attempt_count))
    .bind(error.chars().take(4000).collect::<String>())
    .bind(crate::sqlite_i64(id))
    .bind(worker_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn process_delivery_batch(state: &AppState, worker_id: &str) -> Result<usize> {
    let ids = claim_rows(
        state,
        "agent_notification_deliveries",
        worker_id,
        DELIVERY_BATCH,
    )
    .await?;
    let count = ids.len();
    for id in ids {
        if let Err(error) = run_with_claim_heartbeat(
            state,
            "agent_notification_deliveries",
            &id,
            worker_id,
            deliver_claimed_notification(state, worker_id, &id),
        )
        .await
        {
            if permanent_delivery_error(&error) {
                fail_delivery_permanently(state, &id, worker_id, &error.to_string()).await?;
            } else {
                retry_or_fail_claim(
                    state,
                    "agent_notification_deliveries",
                    &id,
                    worker_id,
                    &error.to_string(),
                )
                .await?;
            }
        }
    }
    Ok(count)
}

fn permanent_delivery_error(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Forbidden
            | AppError::NotFound(_)
            | AppError::ValidationError(_)
            | AppError::Unimplemented(_)
    )
}

async fn fail_delivery_permanently(
    state: &AppState,
    id: &str,
    worker_id: &str,
    error: &str,
) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_notification_deliveries
         SET status = 'failed', last_error = ?, claimed_by = NULL, claimed_at = NULL,
             lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(error.chars().take(4000).collect::<String>())
    .bind(id)
    .bind(worker_id)
    .execute(state.control_db())
    .await?;
    update_failed_delivery_watch_rule(state, id).await
}

async fn deliver_claimed_notification(state: &AppState, worker_id: &str, id: &str) -> Result<()> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, task_id, user_id, platform, channel_id, external_conversation_id,
                CAST(payload_json AS TEXT) AS payload_json
         FROM agent_notification_deliveries
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(id)
    .bind(worker_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::Conflict("notification delivery lease was lost".to_string()))?;
    let tenant_id: String = row.get("tenant_id");
    let task_id: String = row.get("task_id");
    let user_id: String = row.get("user_id");
    let platform: String = row.get("platform");
    if tenant_notification_mode(state, &tenant_id).await? != "on" {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_notification_deliveries
             SET status = 'suppressed', last_error = 'tenant notification rollout disabled',
                 claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
        )
        .bind(id)
        .bind(worker_id)
        .execute(state.control_db())
        .await?;
        return Ok(());
    }
    if !crate::routes::task_control::actor_can_read_task(state, &tenant_id, &task_id, &user_id)
        .await?
    {
        return Err(AppError::Forbidden);
    }
    let payload = row
        .get::<Option<String>, _>("payload_json")
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("AOS 任务通知");
    let body = payload
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("任务状态已更新");
    let provider_message_id = match platform.as_str() {
        "webui" => {
            let mut hasher = Sha256::new();
            hasher.update(id.as_bytes());
            let notification_id = hex::encode(hasher.finalize())
                .chars()
                .take(36)
                .collect::<String>();
            sqlx::query::<sqlx::Sqlite>(
                r"INSERT OR IGNORE INTO notifications
                   (id, tenant_id, user_id, title, body, level, `read`)
                 VALUES (?, ?, ?, ?, ?, ?, 0)",
            )
            .bind(&notification_id)
            .bind(&tenant_id)
            .bind(&user_id)
            .bind(title.chars().take(255).collect::<String>())
            .bind(body)
            .bind(notification_level(
                payload
                    .get("eventType")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ))
            .execute(state.control_db())
            .await?;
            Some(notification_id)
        }
        "bot" | "webhook" => {
            let channel_id: String = row
                .get::<Option<String>, _>("channel_id")
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    AppError::ValidationError("notification channel is missing".to_string())
                })?;
            let external_conversation_id = row
                .get::<Option<String>, _>("external_conversation_id")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let require_exact_conversation = payload
                .get("requireExactConversationBinding")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if matches!(platform.as_str(), "bot" | "webhook") {
                let still_bound: i64 = if require_exact_conversation {
                    let conversation_id = external_conversation_id.as_deref().ok_or_else(|| {
                        AppError::ValidationError(
                            "verified notification conversation is missing".to_string(),
                        )
                    })?;
                    sqlx::query_scalar::<sqlx::Sqlite, _>(
                        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_external_identity_links
                         WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
                           AND external_conversation_id = ?
                           AND status = 'active' AND revoked_at IS NULL",
                    )
                    .bind(&tenant_id)
                    .bind(&user_id)
                    .bind(&channel_id)
                    .bind(conversation_id)
                    .fetch_one(state.control_db())
                    .await?
                } else {
                    sqlx::query_scalar::<sqlx::Sqlite, _>(
                        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_external_identity_links
                         WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
                           AND status = 'active' AND revoked_at IS NULL",
                    )
                    .bind(&tenant_id)
                    .bind(&user_id)
                    .bind(&channel_id)
                    .fetch_one(state.control_db())
                    .await?
                };
                if still_bound == 0 {
                    return Err(AppError::Forbidden);
                }
            }
            #[cfg(feature = "bot-agents")]
            {
                let marked = sqlx::query::<sqlx::Sqlite>(
                    "UPDATE agent_notification_deliveries
                     SET dispatch_started_at = COALESCE(dispatch_started_at, CURRENT_TIMESTAMP),
                         updated_at = CURRENT_TIMESTAMP
                     WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
                )
                .bind(id)
                .bind(worker_id)
                .execute(state.control_db())
                .await?;
                if marked.rows_affected() != 1 {
                    return Err(AppError::Conflict(
                        "notification delivery lease was lost before dispatch".to_string(),
                    ));
                }
                let sent = crate::routes::bot_agents_outbound::send_channel_message(
                    state,
                    &tenant_id,
                    &channel_id,
                    crate::routes::bot_agents_outbound::BotOutboundMessage {
                        title: Some(title.to_string()),
                        text: body.to_string(),
                        external_conversation_id,
                    },
                )
                .await?;
                Some(sent.log_id)
            }
            #[cfg(not(feature = "bot-agents"))]
            {
                let _ = channel_id;
                return Err(AppError::ValidationError(
                    "bot notification delivery requires bot-agents feature".to_string(),
                ));
            }
        }
        other => {
            return Err(AppError::ValidationError(format!(
                "unsupported notification destination '{other}'"
            )));
        }
    };
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_notification_deliveries
         SET status = 'sent', provider_message_id = ?, sent_at = CURRENT_TIMESTAMP,
             claimed_by = NULL, claimed_at = NULL, lease_expires_at = NULL,
             last_error = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND status = 'claimed' AND claimed_by = ?",
    )
    .bind(provider_message_id)
    .bind(id)
    .bind(worker_id)
    .execute(state.control_db())
    .await?;
    if payload.get("confirmationRequired").and_then(Value::as_bool) == Some(false) {
        if let Some(run_id) = payload.get("watchRuleRunId").and_then(Value::as_u64) {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE agent_watch_rule_runs
                 SET action_status = 'delivered', reason_code = 'delivery_sent'
                 WHERE tenant_id = ? AND id = ? AND action_status IN ('queued','action_queued')",
            )
            .bind(&tenant_id)
            .bind(crate::sqlite_i64(run_id))
            .execute(state.control_db())
            .await?;
        }
    }
    Ok(())
}

fn notification_level(event_type: &str) -> &'static str {
    match event_type {
        "task.failed" | "task.stalled" => "error",
        "task.waiting_input"
        | "task.waiting_approval"
        | "task.blocked"
        | "task.sla_breached"
        | "task.budget_warning" => "warning",
        "task.completed" => "success",
        _ => "info",
    }
}

async fn reconcile_active_task_projections(state: &AppState) -> Result<usize> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, id, linked_resource_type, linked_resource_id
         FROM agent_tasks
         WHERE projection_active = 1
         ORDER BY last_heartbeat_at ASC, id ASC
         LIMIT 100",
    )
    .fetch_all(state.control_db())
    .await?;
    let tasks = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("tenant_id"),
                row.get::<String, _>("id"),
                row.get::<String, _>("linked_resource_type"),
                row.get::<String, _>("linked_resource_id"),
            )
        })
        .collect::<Vec<_>>();
    let projected = stream::iter(tasks)
        .map(
            |(tenant_id, task_id, resource_type, resource_id)| async move {
                if let Err(error) = agent_ops::sync_linked_resource_status_inner(
                    state,
                    &tenant_id,
                    &task_id,
                    Some(&resource_type),
                    Some(&resource_id),
                )
                .await
                {
                    tracing::warn!(
                        tenant_id,
                        task_id,
                        linked_resource_type = resource_type,
                        linked_resource_id = resource_id,
                        error = %error,
                        error_debug = ?error,
                        "failed to sync linked resource status"
                    );
                }
                1_usize
            },
        )
        // Platform storage is a single SQLite writer. Serial projection avoids
        // creating an eight-way write queue on every reconciliation tick.
        .buffer_unordered(1)
        .fold(0_usize, |count, value| async move { count + value })
        .await;
    Ok(projected)
}

async fn emit_task_guard_events(state: &AppState) -> Result<usize> {
    let stalled_seconds = std::env::var("WATCHDOG_STALLED_AFTER_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(600)
        .clamp(60, 86_400);
    let stale_seconds = std::env::var("WATCHDOG_STALE_AFTER_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| (stalled_seconds * 3).max(1_800))
        .clamp(stalled_seconds + 60, 604_800);
    let stale_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id, id
         FROM agent_tasks
         WHERE archived = 0
           AND status IN ('created','queued','claimed','running','retrying','cancelling')
           AND last_heartbeat_at < datetime(CURRENT_TIMESTAMP, printf('%+d seconds', -?))
         ORDER BY last_heartbeat_at ASC
         LIMIT 50",
    )
    .bind(stale_seconds)
    .fetch_all(state.control_db())
    .await?;
    let mut emitted = 0;
    for row in stale_rows {
        let tenant_id: String = row.get("tenant_id");
        let task_id: String = row.get("id");
        if let Err(error) =
            agent_ops::sync_linked_resource_status(state, &tenant_id, &task_id).await
        {
            tracing::warn!(
                tenant_id,
                task_id,
                error = %error,
                error_debug = ?error,
                "stale task resource recheck failed; deferring stale transition"
            );
            continue;
        }
        if agent_ops::mark_task_stale_if_heartbeat_expired(
            state,
            &tenant_id,
            &task_id,
            stale_seconds,
        )
        .await?
        {
            emitted += 1;
        }
    }
    let stalled = sqlx::query::<sqlx::Sqlite>(
        "SELECT at.tenant_id, at.id
         FROM agent_tasks at
         WHERE at.archived = 0
           AND at.status IN ('claimed','running','retrying','cancelling')
           AND COALESCE(at.last_progress_at, at.started_at, at.created_at)
               < datetime(CURRENT_TIMESTAMP, printf('%+d seconds', -?))
           AND NOT EXISTS (
             SELECT 1 FROM agent_task_outbox outbox
             WHERE outbox.tenant_id = at.tenant_id AND outbox.task_id = at.id
               AND outbox.event_type = 'task.stalled'
               AND outbox.created_at >= COALESCE(at.last_progress_at, at.started_at, at.created_at)
           )
         ORDER BY COALESCE(at.last_progress_at, at.started_at, at.created_at) ASC
         LIMIT 100",
    )
    .bind(stalled_seconds)
    .fetch_all(state.control_db())
    .await?;
    let overdue = sqlx::query::<sqlx::Sqlite>(
        "SELECT at.tenant_id, at.id
         FROM agent_tasks at
         WHERE at.archived = 0 AND at.sla_due_at IS NOT NULL AND at.sla_due_at < CURRENT_TIMESTAMP
           AND at.status NOT IN ('completed','failed','cancelled','timed_out','stale')
           AND NOT EXISTS (
             SELECT 1 FROM agent_task_outbox outbox
             WHERE outbox.tenant_id = at.tenant_id AND outbox.task_id = at.id
               AND outbox.event_type = 'task.sla_breached'
           )
         ORDER BY at.sla_due_at ASC LIMIT 100",
    )
    .fetch_all(state.control_db())
    .await?;
    let budget_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT at.tenant_id, at.id, CAST(at.budget_json AS TEXT) AS budget_json,
                CAST(at.cost_json AS TEXT) AS cost_json
         FROM agent_tasks at
         WHERE at.archived = 0 AND at.budget_json IS NOT NULL AND at.cost_json IS NOT NULL
           AND at.status NOT IN ('completed','failed','cancelled','timed_out','stale')
           AND NOT EXISTS (
             SELECT 1 FROM agent_task_outbox outbox
             WHERE outbox.tenant_id = at.tenant_id AND outbox.task_id = at.id
               AND outbox.event_type = 'task.budget_warning'
           )
         ORDER BY at.updated_at ASC LIMIT 100",
    )
    .fetch_all(state.control_db())
    .await?;
    for row in stalled {
        let tenant_id: String = row.get("tenant_id");
        let task_id: String = row.get("id");
        agent_ops::add_event(
            state,
            &tenant_id,
            &task_id,
            "stalled",
            None,
            None,
            "warn",
            "任务较长时间没有可观察进展，WatchDog 将继续检查执行器心跳",
            Some(json!({ "noProgressSeconds": stalled_seconds })),
        )
        .await?;
        emitted += 1;
    }
    for row in overdue {
        let tenant_id: String = row.get("tenant_id");
        let task_id: String = row.get("id");
        agent_ops::add_event(
            state,
            &tenant_id,
            &task_id,
            "sla_breached",
            None,
            None,
            "warn",
            "任务已超过约定处理时间",
            None,
        )
        .await?;
        emitted += 1;
    }
    for row in budget_rows {
        let budget = row
            .get::<Option<String>, _>("budget_json")
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        let cost = row
            .get::<Option<String>, _>("cost_json")
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        let warning = budget
            .get("warningUsd")
            .or_else(|| budget.get("maxUsd"))
            .and_then(Value::as_f64);
        let actual = cost
            .get("totalUsd")
            .or_else(|| cost.get("costUsd"))
            .and_then(Value::as_f64);
        if !matches!((warning, actual), (Some(warning), Some(actual)) if actual >= warning) {
            continue;
        }
        let tenant_id: String = row.get("tenant_id");
        let task_id: String = row.get("id");
        agent_ops::add_event(
            state,
            &tenant_id,
            &task_id,
            "budget_warning",
            None,
            None,
            "warn",
            "任务成本已达到预算告警线",
            Some(json!({ "budget": budget, "cost": cost })),
        )
        .await?;
        emitted += 1;
    }
    Ok(emitted)
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "timed_out" | "stale"
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        claim_rows, claimed_table_supports_heartbeat, command_state_version_matches,
        delivery_idempotency_key, durable_notification_delivery_enabled_for, event_type_matches,
        finish_command, is_important_event, materialize_outbox_deliveries,
        mobile_follow_delivery_allowed, next_worker_delay, notification_level,
        notification_requires_metadata_only, permanent_delivery_error, protected_notification_body,
        quiet_hours_active, retry_backoff_seconds, sanitize_notification_text,
        should_create_automatic_delivery, suppress_original_bot_completion_notification,
        watch_condition_matches, WATCH_RULE_DAILY_ACTION_COUNT_SQL,
    };

    #[test]
    fn retry_backoff_is_exponential_bounded_and_saturating() {
        assert_eq!(retry_backoff_seconds(-1), 1);
        assert_eq!(retry_backoff_seconds(0), 1);
        assert_eq!(retry_backoff_seconds(1), 2);
        assert_eq!(retry_backoff_seconds(8), 256);
        assert_eq!(retry_backoff_seconds(9), 300);
        assert_eq!(retry_backoff_seconds(i64::MAX), 300);
    }

    fn sqlite_test_state(db: sqlx::SqlitePool) -> crate::state::AppState {
        crate::state::AppState {
            data_dir: std::env::temp_dir(),
            platform_lifecycle: None,
            control_db: db.clone(),
            telemetry_db: db.clone(),
            #[cfg(feature = "pm")]
            pm_telemetry: crate::routes::agent::PmTelemetrySink::for_test(),
            db,
            jwt_secret: std::sync::Arc::new(tokio::sync::RwLock::new("test".repeat(8))),
            base_url: "http://localhost".to_string(),
            default_model: "test-model".to_string(),
            setup_initialized_cache: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            usage_writer: None,
            agent_manager: None,
            #[cfg(feature = "projects")]
            gitlab_manager: None,
            config_registry: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_embedding_store: None,
            #[cfg(feature = "rd")]
            rd_embedding_store: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_routing_engine: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_pool_cache: std::sync::Arc::new(crate::nl2sql::datasource_pool::PoolCache::new()),
            #[cfg(feature = "nl2sql")]
            nl2sql_rate_limiter: std::sync::Arc::new(
                crate::nl2sql::rate_limiter::TenantRateLimiter::default(),
            ),
        }
    }

    #[test]
    fn claim_heartbeats_are_limited_to_known_worker_tables() {
        assert!(claimed_table_supports_heartbeat(
            "agent_task_command_requests"
        ));
        assert!(claimed_table_supports_heartbeat(
            "agent_notification_deliveries"
        ));
        assert!(!claimed_table_supports_heartbeat("agent_tasks"));
        assert!(!claimed_table_supports_heartbeat(
            "agent_task_command_requests; DROP TABLE users"
        ));
    }
    use serde_json::json;

    #[test]
    fn idle_workers_back_off_and_reset_after_processing_work() {
        let active = Duration::from_millis(500);
        let max_idle = Duration::from_secs(2);
        assert_eq!(
            next_worker_delay(active, active, max_idle, 0),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_worker_delay(Duration::from_secs(1), active, max_idle, 0),
            max_idle
        );
        assert_eq!(next_worker_delay(max_idle, active, max_idle, 0), max_idle);
        assert_eq!(next_worker_delay(max_idle, active, max_idle, 1), active);
    }

    #[test]
    fn event_matching_supports_exact_and_prefix_wildcard() {
        assert!(event_type_matches("task.completed", "task.completed"));
        assert!(event_type_matches("task.*", "task.waiting_input"));
        assert!(!event_type_matches("task.failed", "task.completed"));
    }

    #[test]
    fn durable_notification_mode_is_explicit_and_case_insensitive() {
        assert!(durable_notification_delivery_enabled_for(Some("on")));
        assert!(durable_notification_delivery_enabled_for(Some(" ON ")));
        assert!(!durable_notification_delivery_enabled_for(Some("shadow")));
        assert!(!durable_notification_delivery_enabled_for(Some("off")));
        assert!(!durable_notification_delivery_enabled_for(None));
    }

    #[test]
    fn invalid_or_revoked_delivery_targets_fail_without_retrying() {
        assert!(permanent_delivery_error(&crate::error::AppError::Forbidden));
        assert!(permanent_delivery_error(
            &crate::error::AppError::ValidationError("missing destination".to_string())
        ));
        assert!(!permanent_delivery_error(
            &crate::error::AppError::Internal("temporary provider timeout".to_string())
        ));
    }

    #[test]
    fn automatic_notifications_exclude_routine_completion_and_cancellation() {
        assert!(!is_important_event("task.completed"));
        assert!(!is_important_event("task.cancelled"));
        assert!(is_important_event("task.waiting_approval"));
        assert!(!is_important_event("task.progress"));
        assert_eq!(notification_level("task.failed"), "error");
        assert_eq!(notification_level("task.completed"), "success");
        assert!(!should_create_automatic_delivery("task.completed", 59));
        assert!(!should_create_automatic_delivery("task.completed", 60));
        assert!(should_create_automatic_delivery("task.failed", 1));
        assert!(suppress_original_bot_completion_notification(
            "task.completed",
            "bot",
            true,
        ));
        assert!(!suppress_original_bot_completion_notification(
            "task.failed",
            "bot",
            true,
        ));
        assert!(!suppress_original_bot_completion_notification(
            "task.completed",
            "webui",
            true,
        ));
    }

    #[test]
    fn mobile_follow_requires_opt_in_and_no_active_web_client() {
        assert!(!mobile_follow_delivery_allowed(false, 0));
        assert!(!mobile_follow_delivery_allowed(true, 1));
        assert!(!mobile_follow_delivery_allowed(true, 3));
        assert!(mobile_follow_delivery_allowed(true, 0));
    }

    #[test]
    fn watch_conditions_require_all_declared_thresholds() {
        let condition = json!({
            "eventTypes": ["task.failed", "task.stalled"],
            "statuses": ["failed"],
            "minElapsedSeconds": 60,
            "noProgressSeconds": 30,
            "minCostUsd": 1.5
        });
        assert!(watch_condition_matches(
            &condition,
            "task.failed",
            "failed",
            120,
            45,
            Some(&json!({ "totalUsd": 2.0 })),
        ));
        assert!(!watch_condition_matches(
            &condition,
            "task.completed",
            "completed",
            120,
            45,
            Some(&json!({ "totalUsd": 2.0 })),
        ));
        assert!(!watch_condition_matches(
            &condition,
            "task.failed",
            "failed",
            20,
            45,
            Some(&json!({ "totalUsd": 2.0 })),
        ));
    }

    #[test]
    fn malformed_quiet_hours_fail_open_without_suppressing_notifications() {
        assert!(!quiet_hours_active(Some(
            &json!({ "start": "bad", "end": "09:00" })
        )));
        assert!(!quiet_hours_active(None));
    }

    #[test]
    fn notification_dlp_redacts_common_credentials() {
        let sanitized = sanitize_notification_text(
            "Authorization: Bearer abc.def password=hunter2 api_key=sk-live-secret ok\n-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----",
        );
        assert!(!sanitized.contains("abc.def"));
        assert!(!sanitized.contains("hunter2"));
        assert!(!sanitized.contains("sk-live-secret"));
        assert!(!sanitized.contains("BEGIN PRIVATE KEY"));
        assert!(sanitized.contains("ok"));
    }

    #[test]
    fn protected_tasks_emit_metadata_only_notifications() {
        assert!(notification_requires_metadata_only("restricted"));
        assert!(notification_requires_metadata_only("CONFIDENTIAL"));
        assert!(!notification_requires_metadata_only("internal"));
        let body = protected_notification_body("task.completed", Some("ABC123"));
        assert!(body.contains("ABC123"));
        assert!(body.contains("WebUI"));
        assert!(!body.contains("revenue"));
    }

    #[test]
    fn command_rejects_state_changes_after_acceptance() {
        assert!(command_state_version_matches(
            None,
            42,
            "approve",
            "waiting_approval"
        ));
        assert!(command_state_version_matches(
            Some(10),
            11,
            "approve",
            "waiting_approval"
        ));
        assert!(!command_state_version_matches(
            Some(10),
            10,
            "approve",
            "waiting_approval"
        ));
        assert!(!command_state_version_matches(
            Some(10),
            12,
            "approve",
            "waiting_approval"
        ));
        assert!(command_state_version_matches(
            Some(10),
            13,
            "cancel",
            "cancelling"
        ));
        assert!(command_state_version_matches(
            Some(10),
            13,
            "cancel",
            "completed"
        ));
        assert!(command_state_version_matches(
            Some(10),
            13,
            "kill",
            "cancelling"
        ));
    }

    #[test]
    fn delivery_idempotency_keys_are_stable_and_database_safe() {
        let destination = "channel/".repeat(100);
        let first = delivery_idempotency_key(42, "subscription", "bot", &destination);
        let second = delivery_idempotency_key(42, "subscription", "bot", &destination);
        let changed = delivery_idempotency_key(43, "subscription", "bot", &destination);
        assert_eq!(first, second);
        assert_ne!(first, changed);
        assert!(first.len() <= 191);
    }

    #[tokio::test]
    async fn sqlite_watch_rule_daily_limit_query_uses_supported_date_syntax() {
        let db = crate::test_sqlite_pool().await;
        let action_count: i64 =
            sqlx::query_scalar::<sqlx::Sqlite, _>(WATCH_RULE_DAILY_ACTION_COUNT_SQL)
                .bind("missing-tenant")
                .bind("missing-rule")
                .fetch_one(&db)
                .await
                .expect("execute WatchDog daily action count query");

        assert_eq!(action_count, 0);
        db.close().await;
    }

    #[tokio::test]
    async fn sqlite_command_claims_prioritize_kill_cancel_input_and_retry() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db.clone());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, capability_key, title, root_task_id,
                last_heartbeat_at)
             VALUES (?, 'PRIORITY0001', ?, 'test', 'priority fixture', ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert priority task fixture");

        for (index, command_type) in ["retry", "provide_input", "cancel", "kill"]
            .into_iter()
            .enumerate()
        {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_task_command_requests
                   (id, tenant_id, task_id, actor_user_id, command_type, status,
                    idempotency_key, available_at, created_at)
                 VALUES (?, ?, ?, 'priority-user', ?, 'queued', ?, CURRENT_TIMESTAMP,
                         CURRENT_TIMESTAMP)",
            )
            .bind(format!("priority-{command_type}"))
            .bind(&tenant_id)
            .bind(&task_id)
            .bind(command_type)
            .bind(format!("priority-key-{index}"))
            .execute(&db)
            .await
            .expect("insert priority command fixture");
        }

        for (index, expected) in ["kill", "cancel", "provide_input", "retry"]
            .into_iter()
            .enumerate()
        {
            let worker_id = format!("priority-worker-{index}");
            let claimed = claim_rows(&state, "agent_task_command_requests", &worker_id, 1)
                .await
                .expect("claim command by priority");
            assert_eq!(claimed, vec![format!("priority-{expected}")]);
        }

        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_concurrent_worker_claims_serialize_without_busy_errors() {
        let (db, database_path) = crate::test_sqlite_file_pool().await;
        let state = sqlite_test_state(db.clone());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, capability_key, title, root_task_id,
                last_heartbeat_at)
             VALUES (?, 'CLAIMRACE001', ?, 'test', 'claim race fixture', ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert claim-race task fixture");
        for index in 0..16 {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_task_command_requests
                   (id, tenant_id, task_id, actor_user_id, command_type, status,
                    idempotency_key, available_at, created_at)
                 VALUES (?, ?, ?, 'claim-user', 'cancel', 'queued', ?, CURRENT_TIMESTAMP,
                         CURRENT_TIMESTAMP)",
            )
            .bind(format!("claim-race-{index}"))
            .bind(&tenant_id)
            .bind(&task_id)
            .bind(format!("claim-race-key-{index}"))
            .execute(&db)
            .await
            .expect("insert claim-race command fixture");
        }

        let outcomes = futures_util::future::join_all((0..8).map(|index| {
            let state = &state;
            async move {
                let worker_id = format!("claim-race-worker-{index}");
                claim_rows(state, "agent_task_command_requests", &worker_id, 2).await
            }
        }))
        .await;
        let mut claimed = std::collections::HashSet::new();
        for outcome in outcomes {
            for id in outcome.expect("concurrent claim should serialize") {
                assert!(claimed.insert(id), "a command was claimed more than once");
            }
        }
        assert_eq!(claimed.len(), 16);

        state.db.close().await;
        for path in [
            database_path.clone(),
            database_path.with_extension("db-wal"),
            database_path.with_extension("db-shm"),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[tokio::test]
    async fn existing_watch_rule_webui_delivery_suppresses_automatic_duplicate() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db.clone());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let worker_id = "watch-rule-delivery-dedup-test";
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, last_heartbeat_at)
             VALUES (?, 'WATCHDEDUP01', ?, 'test', 'deep_research', 'dedup task', 'failed',
                     'finalizing', ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("task fixture");
        let outbox_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json, status, claimed_by, claimed_at)
             VALUES (?, ?, ?, ?, 'task.failed', 1, 'owner', '{}', 'claimed', ?,
                     CURRENT_TIMESTAMP)",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(&task_id)
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("outbox fixture")
        .last_insert_rowid();
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_notification_deliveries
               (id, tenant_id, outbox_id, task_id, user_id, platform, idempotency_key, payload_json)
             VALUES (?, ?, ?, ?, ?, 'webui', ?, '{}')",
        )
        .bind(format!("agtdel-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(outbox_id)
        .bind(&task_id)
        .bind(&user_id)
        .bind(delivery_idempotency_key(
            u64::try_from(outbox_id).expect("positive outbox id"),
            "watch-rule",
            "webui",
            &user_id,
        ))
        .execute(&db)
        .await
        .expect("watch-rule delivery fixture");

        materialize_outbox_deliveries(
            &state,
            worker_id,
            u64::try_from(outbox_id).expect("positive outbox id"),
            true,
        )
        .await
        .expect("materialize outbox");
        let delivery_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND user_id = ? AND platform = 'webui'",
        )
        .bind(&tenant_id)
        .bind(outbox_id)
        .bind(&user_id)
        .fetch_one(&db)
        .await
        .expect("count webui deliveries");
        assert_eq!(delivery_count, 1);
        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_standard_bot_event_config_is_bound_and_deduplicated() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db.clone());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let agent_id = uuid::Uuid::new_v4().to_string();
        let channel_id = uuid::Uuid::new_v4().to_string();
        let subscription_id = uuid::Uuid::new_v4().to_string();
        let worker_id = "notification-materialize-test";

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO users
               (id, email, name, password_hash, role, tenant_id, is_active)
             VALUES (?, ?, 'Notification user', 'not-used', 'developer', ?, 1)",
        )
        .bind(&user_id)
        .bind(format!(
            "notification-{}@example.invalid",
            uuid::Uuid::new_v4()
        ))
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("insert notification user fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO bot_agents
               (id, tenant_id, name, enabled, default_capability, created_by)
             VALUES (?, ?, 'Test Bot', 1, 'ai_chat', ?)",
        )
        .bind(&agent_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .execute(&db)
        .await
        .expect("insert Bot agent fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO bot_agent_channels
               (id, tenant_id, agent_id, platform, name, enabled, config_json)
             VALUES (?, ?, ?, 'slack', 'Test channel', 1, ?)",
        )
        .bind(&channel_id)
        .bind(&tenant_id)
        .bind(&agent_id)
        .bind(json!({ "notifyOnEvents": ["task.failed"] }).to_string())
        .execute(&db)
        .await
        .expect("insert Bot channel fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_external_identity_links
               (id, tenant_id, user_id, platform, external_user_id, channel_id,
                external_conversation_id, status, verified_at)
             VALUES (?, ?, ?, 'slack', 'external-user', ?, 'conversation-a', 'active',
                     CURRENT_TIMESTAMP)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("insert identity binding fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, last_heartbeat_at)
             VALUES (?, 'NOTIFYTEST01', ?, 'test', 'ai_chat', 'failed task', 'failed',
                     'finalizing', ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert notification task fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_subscriptions
               (id, tenant_id, task_id, task_key, user_id, event_types_json,
                destination_type, destination_ref, destination_key)
             VALUES (?, ?, ?, ?, ?, ?, 'bot', ?, ?)",
        )
        .bind(&subscription_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(&task_id)
        .bind(&user_id)
        .bind(json!(["task.failed"]).to_string())
        .bind(&channel_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("insert explicit Bot subscription fixture");
        let outbox_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json, status, claimed_by, claimed_at)
             VALUES (?, ?, ?, ?, 'task.failed', 1, 'owner', ?, 'claimed', ?,
                     CURRENT_TIMESTAMP)",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(&task_id)
        .bind(json!({ "message": "failed for test" }).to_string())
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("insert claimed outbox fixture")
        .last_insert_rowid();

        materialize_outbox_deliveries(
            &state,
            worker_id,
            u64::try_from(outbox_id).expect("positive outbox id"),
            true,
        )
        .await
        .expect("materialize standard task notification");
        let bot_deliveries: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT channel_id, external_conversation_id
             FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND platform = 'bot'",
        )
        .bind(&tenant_id)
        .bind(outbox_id)
        .fetch_all(&db)
        .await
        .expect("load Bot deliveries");
        assert_eq!(
            bot_deliveries,
            vec![(channel_id.clone(), Some("conversation-a".to_string()))]
        );

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE bot_agent_channels SET config_json = ? WHERE tenant_id = ? AND id = ?",
        )
        .bind(json!({ "notifyOnEvents": ["task.completed"] }).to_string())
        .bind(&tenant_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("enable completion notification fixture");
        let completed_bot_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, external_channel_id,
                external_conversation_id, last_heartbeat_at)
             VALUES (?, 'NOTIFYDONE01', ?, 'bot', 'ai_chat', 'directly replied task',
                     'completed', 'finalizing', ?, ?, ?, ?, 'conversation-a', CURRENT_TIMESTAMP)",
        )
        .bind(&completed_bot_task_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&completed_bot_task_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("insert completed Bot task fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_events
               (id, tenant_id, task_id, event_type, phase, status, severity, message,
                metadata_json)
             VALUES (?, ?, ?, 'bot.outbound', 'replying', 'running', 'info',
                     'direct reply sent', ?)",
        )
        .bind(format!("agte-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&completed_bot_task_id)
        .bind(json!({ "channelId": &channel_id }).to_string())
        .execute(&db)
        .await
        .expect("insert direct Bot reply event fixture");
        let completed_outbox_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json, status, claimed_by, claimed_at)
             VALUES (?, ?, ?, ?, 'task.completed', 1, 'owner', '{}', 'claimed', ?,
                     CURRENT_TIMESTAMP)",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&completed_bot_task_id)
        .bind(&completed_bot_task_id)
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("insert completed Bot outbox fixture")
        .last_insert_rowid();
        materialize_outbox_deliveries(
            &state,
            worker_id,
            u64::try_from(completed_outbox_id).expect("positive completed outbox id"),
            true,
        )
        .await
        .expect("materialize completed Bot outbox");
        let duplicate_completion_deliveries: i64 = sqlx::query_scalar(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND platform = 'bot'",
        )
        .bind(&tenant_id)
        .bind(completed_outbox_id)
        .fetch_one(&db)
        .await
        .expect("count duplicate completion deliveries");
        assert_eq!(duplicate_completion_deliveries, 0);

        // Automatic replies for a task created in a group must still use the
        // identity-linked private conversation, not the task's group thread.
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE bot_agent_channels SET config_json = '{}' WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("disable channel-level notification fixture");
        let original_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, external_platform,
                external_channel_id, external_conversation_id, last_heartbeat_at)
             VALUES (?, 'NOTIFY0002', ?, 'bot', 'ai_chat', 'group task', 'failed',
                     'finalizing', ?, ?, ?, 'slack', ?, 'group-conversation', CURRENT_TIMESTAMP)",
        )
        .bind(&original_task_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&original_task_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("insert original-channel notification task fixture");
        let original_outbox_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json, status, claimed_by, claimed_at)
             VALUES (?, ?, ?, ?, 'task.failed', 1, 'owner', ?, 'claimed', ?,
                     CURRENT_TIMESTAMP)",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&original_task_id)
        .bind(&original_task_id)
        .bind(json!({ "message": "failed in a group" }).to_string())
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("insert original-channel outbox fixture")
        .last_insert_rowid();

        materialize_outbox_deliveries(
            &state,
            worker_id,
            u64::try_from(original_outbox_id).expect("positive original outbox id"),
            true,
        )
        .await
        .expect("materialize original-channel task notification");
        let original_delivery: (String, Option<String>) = sqlx::query_as(
            "SELECT channel_id, external_conversation_id
             FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND platform = 'bot'",
        )
        .bind(&tenant_id)
        .bind(original_outbox_id)
        .fetch_one(&db)
        .await
        .expect("load original-channel delivery");
        assert_eq!(
            original_delivery,
            (channel_id.clone(), Some("conversation-a".to_string()))
        );

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_external_identity_links
             SET status = 'revoked', revoked_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND channel_id = ?",
        )
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("revoke original-channel identity fixture");
        let revoked_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, external_platform,
                external_channel_id, external_conversation_id, last_heartbeat_at)
             VALUES (?, 'NOTIFY0003', ?, 'bot', 'ai_chat', 'revoked group task', 'failed',
                     'finalizing', ?, ?, ?, 'slack', ?, 'group-conversation', CURRENT_TIMESTAMP)",
        )
        .bind(&revoked_task_id)
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&revoked_task_id)
        .bind(&channel_id)
        .execute(&db)
        .await
        .expect("insert revoked-channel notification task fixture");
        let revoked_outbox_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json, status, claimed_by, claimed_at)
             VALUES (?, ?, ?, ?, 'task.failed', 1, 'owner', ?, 'claimed', ?,
                     CURRENT_TIMESTAMP)",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&revoked_task_id)
        .bind(&revoked_task_id)
        .bind(json!({ "message": "failed after unpairing" }).to_string())
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("insert revoked-channel outbox fixture")
        .last_insert_rowid();

        materialize_outbox_deliveries(
            &state,
            worker_id,
            u64::try_from(revoked_outbox_id).expect("positive revoked outbox id"),
            true,
        )
        .await
        .expect("revoked Bot target must not block other notification materialization");
        let revoked_bot_deliveries: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_notification_deliveries
             WHERE tenant_id = ? AND outbox_id = ? AND platform = 'bot'",
        )
        .bind(&tenant_id)
        .bind(revoked_outbox_id)
        .fetch_one(&db)
        .await
        .expect("count revoked-channel Bot deliveries");
        assert_eq!(revoked_bot_deliveries, 0);
        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_finish_command_updates_linked_watch_rule_atomically() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db.clone());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let rule_id = format!("agtrule-{}", uuid::Uuid::new_v4());
        let command_id = format!("agtcmd-{}", uuid::Uuid::new_v4());
        let worker_id = "test-command-worker";

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, capability_key, title, root_task_id, last_heartbeat_at)
             VALUES (?, 'TESTTASK0001', ?, 'test', 'command fixture', ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert task fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_watch_rules
               (id, tenant_id, user_id, name, condition_json, action_json, created_by)
             VALUES (?, ?, 'test-user', 'test rule', '{}', '{}', 'test-user')",
        )
        .bind(&rule_id)
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("insert watch rule fixture");
        let run_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_watch_rule_runs
               (tenant_id, rule_id, task_id, matched, action_status)
             VALUES (?, ?, ?, 1, 'action_queued')",
        )
        .bind(&tenant_id)
        .bind(&rule_id)
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert watch rule run fixture")
        .last_insert_rowid();
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_command_requests
               (id, tenant_id, task_id, actor_user_id, command_type, status, idempotency_key,
                input_json, claimed_by, claimed_at)
             VALUES (?, ?, ?, 'test-user', 'approve', 'claimed', 'test-idempotency', ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&command_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(json!({ "watchRuleRunId": run_id }).to_string())
        .bind(worker_id)
        .execute(&db)
        .await
        .expect("insert command fixture");

        finish_command(
            &state,
            &command_id,
            worker_id,
            true,
            json!({ "approved": true }),
            None,
        )
        .await
        .expect("finish linked watch rule command");

        let command_status: String =
            sqlx::query_scalar("SELECT status FROM agent_task_command_requests WHERE id = ?")
                .bind(&command_id)
                .fetch_one(&db)
                .await
                .expect("load finished command");
        let task_version: i64 =
            sqlx::query_scalar("SELECT state_version FROM agent_tasks WHERE id = ?")
                .bind(&task_id)
                .fetch_one(&db)
                .await
                .expect("load updated task");
        let run_status: (String, Option<String>) = sqlx::query_as(
            "SELECT action_status, reason_code FROM agent_watch_rule_runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_one(&db)
        .await
        .expect("load updated watch rule run");
        let outbox_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_task_outbox
             WHERE task_id = ? AND event_type = 'task.command_succeeded'",
        )
        .bind(&task_id)
        .fetch_one(&db)
        .await
        .expect("count command outbox events");

        assert_eq!(command_status, "succeeded");
        assert_eq!(task_version, 1);
        assert_eq!(
            run_status,
            (
                "executed".to_string(),
                Some("command_succeeded".to_string())
            )
        );
        assert_eq!(outbox_count, 1);
        state.db.close().await;
    }
}
