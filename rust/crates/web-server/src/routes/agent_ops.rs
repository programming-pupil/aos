//! AgentOps / WatchDog control plane.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get as routing_get, post as routing_post},
    Json, Router,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, Transaction};

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::agent_ops_adapters;
#[allow(unused_imports)]
pub use crate::routes::agent_ops_types::{
    AgentTaskEventInfo, AgentTaskInfo, AgentTaskQueueInfo, AgentTaskRuntimeSummary,
    AgentTraceEventInfo, CreateAgentTaskInput, CreateAgentTaskOutcome, ListResponse,
    QueueListQuery, QueueRecoverRequest, TaskListQuery, WatchDogAskRequest,
    PHASE_CAPABILITY_MATCHING, PHASE_CONTEXT_LOADING, PHASE_DEBATING, PHASE_EXECUTING,
    PHASE_FINALIZING, PHASE_INTAKE, PHASE_MODEL_CALLING, PHASE_PLANNING, PHASE_REPLYING,
    PHASE_RETRIEVING, PHASE_VALIDATING, STATUS_CANCELLED, STATUS_CANCELLING, STATUS_COMPLETED,
    STATUS_CREATED, STATUS_FAILED, STATUS_RUNNING, STATUS_STALE, STATUS_WAITING_INPUT,
};
use crate::routes::agent_ops_types::{
    AGENT_TASK_EVENT_SELECT, AGENT_TASK_RUNTIME_JOIN, AGENT_TASK_SELECT, AGENT_TRACE_EVENT_SELECT,
};
use crate::routes::agent_ops_watchdog_intent::{
    detect_capability_filter, detect_queue_intent, detect_stale_minutes, normalize_queue_intent,
    normalize_watchdog_capability, normalize_watchdog_scope, normalized_status_filter,
    parse_watchdog_intent, WatchDogQueueIntent,
};
use crate::routes::users::parse_permissions_json;
use crate::routes::PaginationParams;
use crate::state::AppState;

pub fn routes(state: AppState) -> Router<AppState> {
    let auth_state = state.clone();
    Router::new()
        .route("/summary", routing_get(summary))
        .route("/agents", routing_get(agents))
        .route("/tasks", routing_get(tasks))
        .route("/tasks/{id}", routing_get(task_detail))
        .route("/tasks/{id}/events", routing_get(task_events))
        .route("/tasks/{id}/trace", routing_get(task_trace_events))
        .route("/tasks/{id}/cancel", routing_post(cancel_task))
        .route("/tasks/{id}/retry", routing_post(retry_task))
        .route("/queue", routing_get(queue_items))
        .route("/queue/recover", routing_post(queue_recover))
        .route("/watchdog/ask", routing_post(watchdog_ask))
        .route("/capabilities", routing_get(capabilities))
        .layer(axum::middleware::from_fn_with_state(
            auth_state,
            crate::auth_middleware::require_auth,
        ))
        .with_state(state)
}

fn task_progress_json(status: &str, phase: &str, message: &str) -> String {
    let completed = matches!(status, STATUS_COMPLETED | STATUS_CANCELLED);
    let blocking = matches!(
        status,
        STATUS_WAITING_INPUT | "waiting_approval" | "blocked"
    );
    json!({
        "phaseCode": phase,
        "phaseLabel": phase,
        "activityCode": status,
        "activityText": message,
        "progressKind": if completed { "percent" } else { "unknown" },
        "current": completed.then_some(1),
        "total": completed.then_some(1),
        "unit": completed.then_some("task"),
        "confidence": if completed { Some(1.0) } else { None },
        "blockingReasonCode": blocking.then_some(status),
        "blockingReasonText": blocking.then_some(message),
    })
    .to_string()
}

pub fn capability_contracts() -> Vec<agent_ops_adapters::CapabilityContract> {
    agent_ops_adapters::capability_contracts()
}

pub fn bot_platform_contracts() -> Vec<agent_ops_adapters::BotPlatformContract> {
    agent_ops_adapters::bot_platform_contracts()
}

pub fn runtime_contracts() -> Vec<agent_ops_adapters::RuntimeContract> {
    agent_ops_adapters::runtime_contracts()
}

pub fn watchdog_action_contracts() -> Vec<agent_ops_adapters::WatchDogActionContract> {
    agent_ops_adapters::watchdog_action_contracts()
}

fn role_has_watchdog_permission(role: &str, permission: &str) -> bool {
    match permission {
        "watchdog:read" => matches!(role, "viewer" | "developer" | "admin" | "superadmin"),
        "watchdog:write" => matches!(role, "developer" | "admin" | "superadmin"),
        "watchdog:admin" => matches!(role, "admin" | "superadmin"),
        _ => false,
    }
}

fn menu_permissions_grant_watchdog(
    role: &str,
    menu_permissions: &[String],
    permission: &str,
) -> bool {
    let can_read_tasks = menu_permissions
        .iter()
        .any(|item| matches!(item.as_str(), "watchdog:read" | "tasks:read"));
    match permission {
        "watchdog:read" => can_read_tasks,
        "watchdog:write" => role_has_watchdog_permission(role, "watchdog:write") && can_read_tasks,
        "watchdog:admin" => role_has_watchdog_permission(role, "watchdog:admin"),
        _ => false,
    }
}

async fn require_watchdog_permission(
    state: &AppState,
    claims: &Claims,
    permission: &str,
) -> Result<()> {
    if has_watchdog_permission(state, claims, permission).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

async fn has_watchdog_permission(
    state: &AppState,
    claims: &Claims,
    permission: &str,
) -> Result<bool> {
    if role_has_watchdog_permission(&claims.role, "watchdog:admin") {
        return Ok(true);
    }
    let row: Option<(String, Option<String>)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT role, CAST(menu_permissions_json AS TEXT) FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(state.control_db())
    .await?;
    let Some((role, menu_permissions_json)) = row else {
        return Ok(false);
    };
    Ok(if let Some(raw) = menu_permissions_json {
        let menu_permissions = parse_permissions_json(Some(raw));
        menu_permissions_grant_watchdog(&role, &menu_permissions, permission)
    } else {
        role_has_watchdog_permission(&role, permission)
    })
}

pub async fn create_task(state: &AppState, input: CreateAgentTaskInput) -> Result<String> {
    Ok(create_task_with_outcome(state, input).await?.id)
}

pub async fn create_task_with_outcome(
    state: &AppState,
    input: CreateAgentTaskInput,
) -> Result<CreateAgentTaskOutcome> {
    let idempotency_key = input
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| trim_to(value.to_string(), 128));
    if let Some(idempotency_key) = idempotency_key.as_deref() {
        if let Some(existing_id) =
            existing_task_for_idempotency_key(state, &input.tenant_id, idempotency_key).await?
        {
            record_idempotency_reuse(state, &input.tenant_id, &existing_id, idempotency_key)
                .await?;
            return Ok(CreateAgentTaskOutcome {
                id: existing_id,
                reused: true,
            });
        }
    }
    let id = format!("agt-{}", uuid::Uuid::new_v4());
    let short_code = crate::routes::task_control::short_code_for_task_id(&id);
    let input_json = json_to_string(input.input_json.as_ref())?;
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let root_task_id = if let Some(parent_task_id) = input.parent_task_id.as_deref() {
        sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT COALESCE(root_task_id, id) FROM agent_tasks
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&input.tenant_id)
        .bind(parent_task_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound("parent agent task not found".to_string()))?
    } else {
        id.clone()
    };
    let insert_result = sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_tasks
            (id, short_code, tenant_id, source, source_ref, source_label, capability_key,
             agent_id, agent_name, status, phase, progress_percent, progress_json, title, summary,
             owner_user_id, initiator_user_id, correlation_id, parent_task_id, root_task_id,
             external_platform,
             external_channel_id, external_conversation_id, external_message_id,
             input_json, queue_status, available_at, max_attempts, idempotency_key, priority,
             last_event, last_heartbeat_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                'queued', CURRENT_TIMESTAMP, 3, ?, 100, '任务已创建', CURRENT_TIMESTAMP)
        ",
    )
    .bind(&id)
    .bind(&short_code)
    .bind(&input.tenant_id)
    .bind(&input.source)
    .bind(&input.source_ref)
    .bind(&input.source_label)
    .bind(&input.capability_key)
    .bind(&input.agent_id)
    .bind(&input.agent_name)
    .bind(STATUS_CREATED)
    .bind(PHASE_INTAKE)
    .bind(task_progress_json(
        STATUS_CREATED,
        PHASE_INTAKE,
        "任务已创建",
    ))
    .bind(trim_to(input.title, 512))
    .bind(&input.summary)
    .bind(&input.owner_user_id)
    .bind(&input.owner_user_id)
    .bind(&input.correlation_id)
    .bind(&input.parent_task_id)
    .bind(&root_task_id)
    .bind(&input.external_platform)
    .bind(&input.external_channel_id)
    .bind(&input.external_conversation_id)
    .bind(&input.external_message_id)
    .bind(input_json)
    .bind(&idempotency_key)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert_result {
        tx.rollback().await?;
        if let Some(idempotency_key) = idempotency_key.as_deref() {
            if matches!(error, sqlx::Error::Database(ref db) if db.is_unique_violation()) {
                if let Some(existing_id) =
                    existing_task_for_idempotency_key(state, &input.tenant_id, idempotency_key)
                        .await?
                {
                    record_idempotency_reuse(
                        state,
                        &input.tenant_id,
                        &existing_id,
                        idempotency_key,
                    )
                    .await?;
                    return Ok(CreateAgentTaskOutcome {
                        id: existing_id,
                        reused: true,
                    });
                }
            }
        }
        return Err(AppError::Database(error));
    }
    let trigger_type = if input.source == "watchdog" && input.parent_task_id.is_some() {
        "watchdog_retry"
    } else {
        "initial"
    };
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_task_attempts
           (id, tenant_id, task_id, attempt_no, trigger_type, trigger_ref, status, metadata_json)
         VALUES (?, ?, ?, 1, ?, ?, 'created', ?)",
    )
    .bind(format!("agtattempt-{}", uuid::Uuid::new_v4()))
    .bind(&input.tenant_id)
    .bind(&id)
    .bind(trigger_type)
    .bind(input.parent_task_id.as_deref())
    .bind(json!({ "source": input.source, "idempotencyKey": idempotency_key }).to_string())
    .execute(&mut *tx)
    .await?;
    add_event_tx(
        &mut tx,
        &input.tenant_id,
        &id,
        "created",
        Some(PHASE_INTAKE),
        Some(STATUS_CREATED),
        "info",
        "任务已创建",
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(CreateAgentTaskOutcome { id, reused: false })
}

async fn existing_task_for_idempotency_key(
    state: &AppState,
    tenant_id: &str,
    idempotency_key: &str,
) -> Result<Option<String>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT id FROM agent_tasks WHERE tenant_id = ? AND idempotency_key = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(idempotency_key)
    .fetch_optional(state.control_db())
    .await?;
    Ok(row.map(|row| row.get("id")))
}

async fn record_idempotency_reuse(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    idempotency_key: &str,
) -> Result<()> {
    add_event(
        state,
        tenant_id,
        task_id,
        "task.idempotency_reused",
        Some(PHASE_INTAKE),
        Some(STATUS_CREATED),
        "info",
        "重复创建请求已复用既有 Agent task",
        Some(json!({ "idempotencyKey": idempotency_key })),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn add_event(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    event_type: &str,
    phase: Option<&str>,
    status: Option<&str>,
    severity: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<()> {
    let mut tx = state.control_db().begin().await?;
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        event_type,
        phase,
        status,
        severity,
        message,
        metadata_json,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_event_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    event_type: &str,
    phase: Option<&str>,
    status: Option<&str>,
    severity: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<()> {
    let id = format!("agte-{}", uuid::Uuid::new_v4());
    let metadata_json = json_to_string(metadata_json.as_ref())?;
    let updated = sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks SET state_version = state_version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "agent task '{task_id}' not found"
        )));
    }
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_task_events
            (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(event_type)
    .bind(phase)
    .bind(status)
    .bind(severity)
    .bind(message)
    .bind(&metadata_json)
    .execute(&mut **tx)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_trace_events
            (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(format!("agtr-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(task_id)
    .bind(event_type)
    .bind(phase)
    .bind(status)
    .bind(severity)
    .bind(message)
    .bind(metadata_json.as_deref())
    .execute(&mut **tx)
    .await?;
    crate::routes::task_control::insert_outbox_tx(
        tx,
        tenant_id,
        task_id,
        &normalize_outbox_event_type(event_type),
        "owner",
        json!({
            "schemaVersion": 1,
            "actorType": "system",
            "phase": phase,
            "status": status,
            "severity": severity,
            "message": message,
            "metadata": metadata_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
        }),
    )
    .await?;
    Ok(())
}

fn normalize_outbox_event_type(event_type: &str) -> String {
    if event_type.starts_with("task.") {
        event_type.to_string()
    } else {
        format!("task.{event_type}")
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_trace_event_full(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    event_type: &str,
    phase: Option<&str>,
    status: Option<&str>,
    severity: &str,
    message: &str,
    metadata_json: Option<&str>,
    artifact_id: Option<&str>,
    runtime_session_id: Option<&str>,
    runtime_process_id: Option<&str>,
    token_input: Option<u64>,
    token_output: Option<u64>,
    cost_usd: Option<f64>,
    duration_ms: Option<u64>,
) -> Result<()> {
    let id = format!("agtr-{}", uuid::Uuid::new_v4());
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_trace_events
            (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json,
             artifact_id, runtime_session_id, runtime_process_id, token_input, token_output,
             cost_usd, duration_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(event_type)
    .bind(phase)
    .bind(status)
    .bind(severity)
    .bind(message)
    .bind(metadata_json)
    .bind(artifact_id)
    .bind(runtime_session_id)
    .bind(runtime_process_id)
    .bind(token_input.and_then(|value| i64::try_from(value).ok()))
    .bind(token_output.and_then(|value| i64::try_from(value).ok()))
    .bind(cost_usd)
    .bind(duration_ms.and_then(|value| i64::try_from(value).ok()))
    .execute(state.control_db())
    .await?;
    Ok(())
}

pub async fn mark_task_running(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    phase: &str,
    message: &str,
    progress_percent: i32,
) -> Result<()> {
    transition_task_with_event(
        state,
        tenant_id,
        task_id,
        STATUS_RUNNING,
        phase,
        message,
        progress_percent,
        "progress",
        "info",
        None,
    )
    .await
    .map(|_| ())
}

#[allow(dead_code)]
pub async fn mark_task_waiting_input(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    phase: &str,
    message: &str,
    progress_percent: i32,
    metadata_json: Option<Value>,
) -> Result<()> {
    transition_task_with_event(
        state,
        tenant_id,
        task_id,
        STATUS_WAITING_INPUT,
        phase,
        message,
        progress_percent,
        "waiting_input",
        "warn",
        metadata_json,
    )
    .await
    .map(|_| ())
}

pub async fn mark_task_waiting_approval(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    phase: &str,
    message: &str,
    progress_percent: i32,
    metadata_json: Option<Value>,
) -> Result<()> {
    transition_task_with_event(
        state,
        tenant_id,
        task_id,
        "waiting_approval",
        phase,
        message,
        progress_percent,
        "waiting_approval",
        "warn",
        metadata_json,
    )
    .await
    .map(|_| ())
}

pub async fn complete_task(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    message: &str,
    output_json: Option<Value>,
) -> Result<()> {
    let output_json = json_to_string(output_json.as_ref())?;
    let mut tx = state.control_db().begin().await?;
    let updated = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE agent_tasks
        SET status = ?, phase = ?, progress_percent = 100, progress_json = ?, output_json = ?,
            result_summary = ?,
            queue_status = 'succeeded',
            claimed_by = NULL,
            claimed_at = NULL,
            lease_expires_at = NULL,
            last_error = NULL,
            finished_at = CURRENT_TIMESTAMP,
            last_event = ?, last_heartbeat_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
          AND status NOT IN ('completed','failed','cancelled','timed_out','stale')
        ",
    )
    .bind(STATUS_COMPLETED)
    .bind(PHASE_FINALIZING)
    .bind(task_progress_json(
        STATUS_COMPLETED,
        PHASE_FINALIZING,
        message,
    ))
    .bind(output_json)
    .bind(message)
    .bind(message)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(());
    }
    finish_latest_attempt_tx(&mut tx, tenant_id, task_id, STATUS_COMPLETED, None, None).await?;
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        "completed",
        Some(PHASE_FINALIZING),
        Some(STATUS_COMPLETED),
        "info",
        message,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn fail_task(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    error_code: &str,
    error_message: &str,
) -> Result<()> {
    let mut tx = state.control_db().begin().await?;
    let updated = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE agent_tasks
        SET status = ?, phase = ?, progress_json = ?, error_code = ?, error_message = ?, last_event = ?,
            result_summary = ?,
            queue_status = 'dead',
            claimed_by = NULL,
            claimed_at = NULL,
            lease_expires_at = NULL,
            last_error = ?,
            finished_at = CURRENT_TIMESTAMP,
            dead_reason = ?,
            last_heartbeat_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
          AND status NOT IN ('completed','failed','cancelled','timed_out','stale')
        ",
    )
    .bind(STATUS_FAILED)
    .bind(PHASE_FINALIZING)
    .bind(task_progress_json(
        STATUS_FAILED,
        PHASE_FINALIZING,
        error_message,
    ))
    .bind(error_code)
    .bind(error_message)
    .bind(error_message)
    .bind(error_message)
    .bind(error_message)
    .bind(error_message)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(());
    }
    finish_latest_attempt_tx(
        &mut tx,
        tenant_id,
        task_id,
        STATUS_FAILED,
        Some(error_code),
        Some(error_message),
    )
    .await?;
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        "failed",
        None,
        Some(STATUS_FAILED),
        "error",
        error_message,
        Some(json!({ "errorCode": error_code })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn link_task_resource(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    resource_type: &str,
    resource_id: &str,
) -> Result<()> {
    let mut tx = state.control_db().begin().await?;
    let updated = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE agent_tasks
        SET linked_resource_type = ?, linked_resource_id = ?, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(resource_type)
    .bind(resource_id)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(AppError::NotFound(format!(
            "agent task '{task_id}' not found"
        )));
    }
    let root_task_id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COALESCE(root_task_id, id) FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT OR IGNORE INTO agent_task_resource_links
            (id, tenant_id, task_id, root_task_id, resource_type, resource_id, relation_type)
        VALUES (?, ?, ?, ?, ?, ?, 'primary')
        ",
    )
    .bind(format!("agtres-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(task_id)
    .bind(root_task_id)
    .bind(resource_type)
    .bind(resource_id)
    .execute(&mut *tx)
    .await?;
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        "linked_resource",
        None,
        None,
        "info",
        "已关联真实能力任务",
        Some(json!({ "resourceType": resource_type, "resourceId": resource_id })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn link_task_secondary_resource(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    resource_type: &str,
    resource_id: &str,
) -> Result<()> {
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let root_task_id = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT COALESCE(root_task_id, id) FROM agent_tasks
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("agent task '{task_id}' not found")))?;
    let inserted = sqlx::query::<sqlx::Sqlite>(
        "INSERT OR IGNORE INTO agent_task_resource_links
           (id, tenant_id, task_id, root_task_id, resource_type, resource_id, relation_type)
         VALUES (?, ?, ?, ?, ?, ?, 'secondary')",
    )
    .bind(format!("agtres-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(task_id)
    .bind(root_task_id)
    .bind(resource_type)
    .bind(resource_id)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 1 {
        add_event_tx(
            &mut tx,
            tenant_id,
            task_id,
            "linked_secondary_resource",
            None,
            None,
            "info",
            "已关联任务来源上下文",
            Some(json!({
                "resourceType": resource_type,
                "resourceId": resource_id,
                "relationType": "secondary",
            })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn sync_linked_resource_status(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT linked_resource_type, linked_resource_id
        FROM agent_tasks
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let resource_type: Option<String> = row.get("linked_resource_type");
    let resource_id: Option<String> = row.get("linked_resource_id");
    sync_linked_resource_status_inner(
        state,
        tenant_id,
        task_id,
        resource_type.as_deref(),
        resource_id.as_deref(),
    )
    .await
}

pub(crate) async fn sync_linked_resource_status_inner(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
) -> Result<()> {
    let (Some(resource_type), Some(resource_id)) = (resource_type, resource_id) else {
        return Ok(());
    };
    match resource_type {
        "rd_task" => sync_rd_task_status(state, tenant_id, task_id, resource_id).await,
        "chat_adversarial_run" => {
            sync_chat_adversarial_status(state, tenant_id, task_id, resource_id).await
        }
        "pm_research_task" => {
            sync_pm_research_task_status(state, tenant_id, task_id, resource_id).await
        }
        "nl2sql_agent_query" => {
            sync_nl2sql_agent_query_status(state, tenant_id, task_id, resource_id).await
        }
        "nl2sql_async_query" => {
            #[cfg(feature = "nl2sql")]
            {
                sync_nl2sql_async_query_status(state, tenant_id, task_id, resource_id).await
            }
            #[cfg(not(feature = "nl2sql"))]
            {
                Ok(())
            }
        }
        "nl2sql_attribution_task" => {
            #[cfg(feature = "nl2sql")]
            {
                sync_nl2sql_attribution_status(state, tenant_id, task_id, resource_id).await
            }
            #[cfg(not(feature = "nl2sql"))]
            {
                Ok(())
            }
        }
        "super_assistant_turn" => {
            sync_super_assistant_turn_status(state, tenant_id, task_id, resource_id).await
        }
        "pm_material_job" => {
            sync_pm_material_job_status(state, tenant_id, task_id, resource_id).await
        }
        "bot_inbound_message" => {
            sync_bot_inbound_message_status(state, tenant_id, task_id, resource_id).await
        }
        _ => Ok(()),
    }
}

#[cfg(feature = "nl2sql")]
async fn sync_nl2sql_async_query_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    query_task_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        "SELECT owner_user_id, status,
                CAST((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400 AS INTEGER) AS age_seconds
         FROM agent_tasks WHERE tenant_id = ? AND id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(agent_task_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    if matches!(
        status.as_str(),
        STATUS_COMPLETED
            | STATUS_FAILED
            | STATUS_CANCELLED
            | "timed_out"
            | STATUS_STALE
            | STATUS_WAITING_INPUT
            | "waiting_approval"
    ) {
        return Ok(());
    }
    let owner_user_id: Option<String> = row.get("owner_user_id");
    let Some(owner_user_id) = owner_user_id.filter(|value| !value.trim().is_empty()) else {
        return fail_task(
            state,
            tenant_id,
            agent_task_id,
            "nl2sql_async_owner_missing",
            "NL2SQL async task has no active owner and cannot be recovered safely",
        )
        .await;
    };
    if super::nl2sql::query_async::query_task_worker_registered(
        query_task_id,
        tenant_id,
        &owner_user_id,
    )
    .await
    {
        return Ok(());
    }
    let age_seconds: i64 = row.get("age_seconds");
    if age_seconds < 15 {
        return Ok(());
    }
    fail_task(
        state,
        tenant_id,
        agent_task_id,
        "nl2sql_async_worker_lost",
        "NL2SQL worker state was lost during service restart; retry the task safely",
    )
    .await
}

#[cfg(feature = "nl2sql")]
async fn sync_nl2sql_attribution_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    attribution_task_id: &str,
) -> Result<()> {
    let Some(mut row) = sqlx::query::<sqlx::Sqlite>(
        "SELECT attribution.status, attribution.summary, attribution.error,
                CAST((julianday(CURRENT_TIMESTAMP) - julianday(attribution.updated_at)) * 86400 AS INTEGER) AS heartbeat_age_seconds
         FROM nl2sql_attribution_tasks attribution
         INNER JOIN agent_tasks task
           ON task.tenant_id = attribution.tenant_id
          AND task.owner_user_id = attribution.user_id
          AND task.id = ?
         WHERE attribution.tenant_id = ? AND attribution.task_id = ? LIMIT 1",
    )
    .bind(agent_task_id)
    .bind(tenant_id)
    .bind(attribution_task_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let source_status: String = row.get("status");
    let heartbeat_age_seconds: i64 = row.get("heartbeat_age_seconds");
    if matches!(source_status.as_str(), "queued" | "running") && heartbeat_age_seconds >= 90 {
        let error = "data-attribution worker heartbeat was lost during service restart";
        let updated = sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_attribution_tasks
             SET status = 'failed', error = ?, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND task_id = ?
               AND status IN ('queued','running') AND cancel_requested = 0
               AND updated_at < datetime(CURRENT_TIMESTAMP, '-90 seconds')",
        )
        .bind(error)
        .bind(tenant_id)
        .bind(attribution_task_id)
        .execute(state.control_db())
        .await?;
        if updated.rows_affected() == 1 {
            return fail_task(
                state,
                tenant_id,
                agent_task_id,
                "attribution_worker_lost",
                error,
            )
            .await;
        }
        row = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, summary, error,
                    CAST((julianday(CURRENT_TIMESTAMP) - julianday(updated_at)) * 86400 AS INTEGER) AS heartbeat_age_seconds
             FROM nl2sql_attribution_tasks
             WHERE tenant_id = ? AND task_id = ? LIMIT 1",
        )
        .bind(tenant_id)
        .bind(attribution_task_id)
        .fetch_one(state.control_db())
        .await?;
    }
    let source_status: String = row.get("status");
    let summary: Option<String> = row.get("summary");
    let error: Option<String> = row.get("error");
    let (status, phase, progress, terminal) = match source_status.as_str() {
        "queued" => ("queued", PHASE_INTAKE, 5, false),
        "running" => (STATUS_RUNNING, PHASE_EXECUTING, 50, false),
        "clarification_needed" => (STATUS_WAITING_INPUT, PHASE_PLANNING, 60, false),
        "completed" | "partial" | "no_data" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        _ => (STATUS_RUNNING, PHASE_EXECUTING, 50, false),
    };
    let message = summary.unwrap_or_else(|| match status {
        STATUS_COMPLETED => "数据归因已完成".to_string(),
        STATUS_CANCELLED => "数据归因已取消".to_string(),
        STATUS_FAILED => error.clone().unwrap_or_else(|| "数据归因失败".to_string()),
        STATUS_WAITING_INPUT => "数据归因需要补充信息".to_string(),
        "queued" => "数据归因已排队".to_string(),
        _ => "数据归因正在执行 SQL 下钻".to_string(),
    });
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        status,
        phase,
        progress,
        &message,
        error.as_deref(),
        terminal,
    )
    .await
}

fn bot_inbound_message_projection(
    queue_status: &str,
    delivery_status: &str,
) -> (&'static str, &'static str, i32, bool) {
    match queue_status {
        "queued" => ("queued", PHASE_INTAKE, 5, false),
        "claimed" => (STATUS_RUNNING, PHASE_INTAKE, 10, false),
        "cancelling" => (STATUS_CANCELLING, PHASE_FINALIZING, 95, false),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "dead" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        "succeeded" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        _ => match delivery_status {
            "queued" => ("queued", PHASE_INTAKE, 5, false),
            "processing" => (STATUS_RUNNING, PHASE_EXECUTING, 50, false),
            "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
            "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
            "replied" | "ignored" | "duplicate" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
            _ => (STATUS_RUNNING, PHASE_INTAKE, 10, false),
        },
    }
}

async fn sync_bot_inbound_message_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    inbound_log_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        "SELECT queue_status, status, error_message
         FROM bot_message_logs
         WHERE tenant_id = ? AND id = ? AND direction = 'inbound' LIMIT 1",
    )
    .bind(tenant_id)
    .bind(inbound_log_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let queue_status: String = row.get("queue_status");
    let delivery_status: String = row.get("status");
    let error_message: Option<String> = row.get("error_message");
    let (status, phase, progress, terminal) =
        bot_inbound_message_projection(&queue_status, &delivery_status);
    let message = match status {
        STATUS_CANCELLED => "Bot 入站任务已取消".to_string(),
        STATUS_CANCELLING => "正在停止 Bot 入站任务".to_string(),
        STATUS_FAILED => error_message
            .clone()
            .unwrap_or_else(|| "Bot 入站任务执行失败".to_string()),
        STATUS_COMPLETED => "Bot 入站队列执行完成".to_string(),
        "queued" => "Bot 入站任务已排队".to_string(),
        _ => "Bot 入站任务正在执行".to_string(),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        status,
        phase,
        progress,
        &message,
        error_message.as_deref(),
        terminal,
    )
    .await
}

fn pm_material_job_projection(status: &str) -> (&'static str, &'static str, i32, bool) {
    match status {
        "queued" => ("queued", PHASE_INTAKE, 5, false),
        "running" => (STATUS_RUNNING, PHASE_MODEL_CALLING, 50, false),
        "cancelling" => (STATUS_CANCELLING, PHASE_FINALIZING, 95, false),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "completed" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        _ => (STATUS_RUNNING, PHASE_EXECUTING, 25, false),
    }
}

fn pm_material_asset_mime_type(asset_type: &str) -> &'static str {
    match asset_type {
        "image" => "image/*",
        "music" => "audio/*",
        "ppt" => "text/html",
        _ => "text/markdown",
    }
}

async fn sync_pm_material_job_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    resource_id: &str,
) -> Result<()> {
    let job_id = resource_id.parse::<u64>().map_err(|_| {
        AppError::ValidationError("invalid PM material job resource id".to_string())
    })?;
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        "SELECT jobs.status, jobs.asset_type, jobs.model, jobs.result_count,
                jobs.error_message, jobs.created_by, tasks.owner_user_id
         FROM pm_material_jobs jobs
         INNER JOIN agent_tasks tasks
           ON tasks.tenant_id = jobs.tenant_id AND tasks.id = ?
         WHERE jobs.tenant_id = ? AND jobs.id = ? LIMIT 1",
    )
    .bind(agent_task_id)
    .bind(tenant_id)
    .bind(crate::sqlite_i64(job_id))
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let owner_user_id: String = row
        .get::<Option<String>, _>("owner_user_id")
        .ok_or_else(|| AppError::ValidationError("material task has no owner".to_string()))?;
    if row.get::<Option<String>, _>("created_by").as_deref() != Some(owner_user_id.as_str()) {
        return Err(AppError::Forbidden);
    }

    let source_status: String = row.get("status");
    let asset_type: String = row.get("asset_type");
    let model: Option<String> = row.get("model");
    let result_count: i32 = row.get("result_count");
    let error: Option<String> = row.get("error_message");
    let (status, phase, progress, terminal) = pm_material_job_projection(&source_status);
    let message = match status {
        STATUS_COMPLETED => format!("{asset_type} 素材生成已完成，共 {result_count} 个结果"),
        STATUS_CANCELLED => "素材生成已取消".to_string(),
        STATUS_CANCELLING => "正在取消素材生成".to_string(),
        STATUS_FAILED => error.clone().unwrap_or_else(|| "素材生成失败".to_string()),
        "queued" => "素材生成已排队".to_string(),
        _ => format!(
            "正在使用 {} 生成 {asset_type} 素材",
            model.as_deref().unwrap_or("模型")
        ),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        status,
        phase,
        progress,
        &message,
        error.as_deref(),
        terminal,
    )
    .await?;

    if status != STATUS_COMPLETED {
        return Ok(());
    }

    let assets = sqlx::query::<sqlx::Sqlite>(
        "SELECT CAST(id AS INTEGER) AS id, asset_type, url, content_text,
                CAST(meta_json AS TEXT) AS meta_json
         FROM pm_material_assets
         WHERE tenant_id = ? AND job_id = ? ORDER BY id ASC",
    )
    .bind(tenant_id)
    .bind(crate::sqlite_i64(job_id))
    .fetch_all(state.control_db())
    .await?;
    let mut first_artifact_ref = None;
    for asset in assets {
        let asset_id: u64 = asset.get("id");
        let projected_asset_type: String = asset.get("asset_type");
        let url: Option<String> = asset.get("url");
        let content_text: Option<String> = asset.get("content_text");
        let metadata_json: Option<String> = asset.get("meta_json");
        let artifact_ref = format!("pm-material-asset:{asset_id}");
        first_artifact_ref.get_or_insert_with(|| artifact_ref.clone());
        let name = url
            .as_deref()
            .and_then(|value| value.split(['?', '#']).next())
            .and_then(|value| value.rsplit('/').next())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{projected_asset_type}-material-{asset_id}"));
        let size_bytes = content_text
            .as_deref()
            .map(str::len)
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0);
        sqlx::query::<sqlx::Sqlite>(
            "INSERT OR IGNORE INTO agent_task_artifacts
               (id, tenant_id, task_id, owner_user_id, artifact_type, name,
                artifact_ref, mime_type, size_bytes, sensitivity_label, metadata_json)
             VALUES (?, ?, ?, ?, 'pm_material_asset', ?, ?, ?, ?, 'internal', ?)",
        )
        .bind(format!("agtart-{}", uuid::Uuid::new_v4()))
        .bind(tenant_id)
        .bind(agent_task_id)
        .bind(&owner_user_id)
        .bind(trim_to(name, 255))
        .bind(&artifact_ref)
        .bind(pm_material_asset_mime_type(&projected_asset_type))
        .bind(crate::sqlite_i64(size_bytes))
        .bind(
            json!({
                "materialJobId": job_id,
                "materialAssetId": asset_id,
                "assetType": projected_asset_type,
                "url": url,
                "sourceMetadata": metadata_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
            })
            .to_string(),
        )
        .execute(state.control_db())
        .await?;
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks
         SET result_summary = ?, result_artifact_ref = COALESCE(?, result_artifact_ref),
             output_json = ?, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ? AND status = 'completed'",
    )
    .bind(&message)
    .bind(first_artifact_ref)
    .bind(
        json!({
            "materialJobId": job_id,
            "assetType": asset_type,
            "model": model,
            "resultCount": result_count,
        })
        .to_string(),
    )
    .bind(tenant_id)
    .bind(agent_task_id)
    .execute(state.control_db())
    .await?;
    Ok(())
}

async fn sync_super_assistant_turn_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    turn_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        "SELECT status, route_capability, final_text, error, session_id, user_id
         FROM super_assistant_turns
         WHERE tenant_id = ? AND turn_id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(turn_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let parent_status: String = row.get("status");
    let (status, phase, progress, completed) = match parent_status.as_str() {
        "queued" => ("queued", PHASE_INTAKE, 5, false),
        "running_model" => (STATUS_RUNNING, PHASE_MODEL_CALLING, 25, false),
        "waiting_subagent" => (STATUS_RUNNING, PHASE_EXECUTING, 55, false),
        "resuming_model" => (STATUS_RUNNING, PHASE_MODEL_CALLING, 70, false),
        "verifying" => (STATUS_RUNNING, PHASE_VALIDATING, 88, false),
        "completed" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        _ => (STATUS_RUNNING, PHASE_EXECUTING, 40, false),
    };
    let route: Option<String> = row.get("route_capability");
    let owner_user_id: String = row.get("user_id");
    let error: Option<String> = row.get("error");
    let message = match status {
        STATUS_COMPLETED => "超级助手回答已完成".to_string(),
        STATUS_CANCELLED => "超级助手回答已取消".to_string(),
        STATUS_FAILED => error
            .clone()
            .unwrap_or_else(|| "超级助手回答失败".to_string()),
        _ => route
            .as_deref()
            .map(|value| format!("正在执行 {value}"))
            .unwrap_or_else(|| "超级助手正在处理".to_string()),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        status,
        phase,
        progress,
        &message,
        error.as_deref(),
        completed,
    )
    .await?;

    let root_task_id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COALESCE(root_task_id, id) FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(agent_task_id)
    .fetch_one(state.control_db())
    .await?;
    let subtasks = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, engine, external_task_id, status, error_message, artifact_ref
         FROM super_assistant_subtasks
         WHERE tenant_id = ? AND parent_turn_id = ?
         ORDER BY created_at ASC",
    )
    .bind(tenant_id)
    .bind(turn_id)
    .fetch_all(state.control_db())
    .await?;
    for subtask in subtasks {
        let subtask_id: String = subtask.get("id");
        let engine: String = subtask.get("engine");
        let subtask_status: String = subtask.get("status");
        let subtask_error: Option<String> = subtask.get("error_message");
        let external_task_id: Option<String> = subtask.get("external_task_id");
        let artifact_ref: Option<String> = subtask.get("artifact_ref");
        let resource_id = external_task_id.as_deref().unwrap_or(&subtask_id);
        let child_task_id = ensure_super_assistant_subtask_task(
            state,
            tenant_id,
            agent_task_id,
            turn_id,
            &subtask_id,
            &engine,
        )
        .await?;
        let (child_status, child_phase, child_progress, child_terminal) =
            super_assistant_subtask_projection(&subtask_status);
        let child_message = match child_status {
            STATUS_COMPLETED => format!("{}子任务已完成", super_assistant_subtask_label(&engine)),
            STATUS_CANCELLED => format!("{}子任务已取消", super_assistant_subtask_label(&engine)),
            STATUS_FAILED => subtask_error
                .clone()
                .unwrap_or_else(|| format!("{}子任务失败", super_assistant_subtask_label(&engine))),
            _ => format!("{}子任务正在执行", super_assistant_subtask_label(&engine)),
        };
        sync_agent_task_from_resource(
            state,
            tenant_id,
            &child_task_id,
            child_status,
            child_phase,
            child_progress,
            &child_message,
            subtask_error.as_deref(),
            child_terminal,
        )
        .await?;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT OR IGNORE INTO agent_task_resource_links
               (id, tenant_id, task_id, root_task_id, resource_type, resource_id,
                relation_type, metadata_json)
             VALUES (?, ?, ?, ?, ?, ?, 'primary', ?)",
        )
        .bind(format!("agtres-{}", uuid::Uuid::new_v4()))
        .bind(tenant_id)
        .bind(&child_task_id)
        .bind(&root_task_id)
        .bind(format!("super_assistant_subtask:{engine}"))
        .bind(resource_id)
        .bind(
            json!({
                "subtaskId": subtask_id,
                "engine": engine,
                "status": subtask_status,
                "artifactRef": artifact_ref,
            })
            .to_string(),
        )
        .execute(state.control_db())
        .await?;
        if let Some(artifact_ref) = artifact_ref
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT OR IGNORE INTO agent_task_artifacts
                   (id, tenant_id, task_id, owner_user_id, artifact_type, name,
                    artifact_ref, sensitivity_label, metadata_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'internal', ?)",
            )
            .bind(format!("agtart-{}", uuid::Uuid::new_v4()))
            .bind(tenant_id)
            .bind(&child_task_id)
            .bind(&owner_user_id)
            .bind(&engine)
            .bind(
                artifact_ref
                    .rsplit('/')
                    .next()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("result"),
            )
            .bind(artifact_ref)
            .bind(
                json!({
                    "subtaskId": subtask_id,
                    "engine": engine,
                })
                .to_string(),
            )
            .execute(state.control_db())
            .await?;
        }
    }
    Ok(())
}

fn super_assistant_subtask_label(engine: &str) -> &'static str {
    match engine {
        "deep_research" => "深度研究",
        "nl2sql" => "NL2SQL",
        "data_attribution" => "数据归因",
        "super_adversarial" => "超级对抗",
        _ => "专业能力",
    }
}

fn super_assistant_subtask_projection(status: &str) -> (&'static str, &'static str, i32, bool) {
    match status {
        "queued" => ("queued", PHASE_INTAKE, 5, false),
        "running" => (STATUS_RUNNING, PHASE_EXECUTING, 55, false),
        "completed" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "timed_out" => ("timed_out", PHASE_FINALIZING, 100, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        _ => (STATUS_RUNNING, PHASE_EXECUTING, 40, false),
    }
}

async fn ensure_super_assistant_subtask_task(
    state: &AppState,
    tenant_id: &str,
    parent_task_id: &str,
    turn_id: &str,
    subtask_id: &str,
    engine: &str,
) -> Result<String> {
    let idempotency_key = format!("super-assistant-subtask:{tenant_id}:{subtask_id}");
    if let Some(existing_id) =
        existing_task_for_idempotency_key(state, tenant_id, &idempotency_key).await?
    {
        return Ok(existing_id);
    }
    let parent = sqlx::query::<sqlx::Sqlite>(
        "SELECT owner_user_id, origin_session_id, origin_turn_id, external_platform,
                external_channel_id, external_conversation_id, sensitivity_label
         FROM agent_tasks WHERE tenant_id = ? AND id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(parent_task_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("parent agent task not found".to_string()))?;
    let label = super_assistant_subtask_label(engine);
    let outcome = create_task_with_outcome(
        state,
        CreateAgentTaskInput {
            tenant_id: tenant_id.to_string(),
            source: "super_assistant".to_string(),
            source_ref: Some(subtask_id.to_string()),
            source_label: Some("Super Assistant subtask".to_string()),
            capability_key: engine.to_string(),
            agent_id: None,
            agent_name: Some(label.to_string()),
            title: format!("{label}子任务"),
            summary: None,
            owner_user_id: parent.get("owner_user_id"),
            correlation_id: Some(turn_id.to_string()),
            parent_task_id: Some(parent_task_id.to_string()),
            external_platform: parent.get("external_platform"),
            external_channel_id: parent.get("external_channel_id"),
            external_conversation_id: parent.get("external_conversation_id"),
            external_message_id: None,
            idempotency_key: Some(idempotency_key),
            input_json: Some(json!({
                "parentTurnId": turn_id,
                "subtaskId": subtask_id,
                "engine": engine,
            })),
        },
    )
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks
         SET origin_session_id = COALESCE(origin_session_id, ?),
             origin_turn_id = COALESCE(origin_turn_id, ?), sensitivity_label = ?
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(parent.get::<Option<String>, _>("origin_session_id"))
    .bind(
        parent
            .get::<Option<String>, _>("origin_turn_id")
            .or_else(|| Some(turn_id.to_string())),
    )
    .bind(parent.get::<String, _>("sensitivity_label"))
    .bind(tenant_id)
    .bind(&outcome.id)
    .execute(state.control_db())
    .await?;
    Ok(outcome.id)
}

async fn sync_rd_task_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    rd_task_id: &str,
) -> Result<()> {
    sync_rd_task_events_to_agent_ops(state, tenant_id, agent_task_id, rd_task_id, 80).await?;
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, error_message, CAST(updated_at AS TEXT) AS updated_at,
               CAST(completed_at AS TEXT) AS completed_at
        FROM rd_tasks
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(rd_task_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    let error_message: Option<String> = row.get("error_message");
    let (agent_status, phase, progress, completed) = match status.as_str() {
        "completed" => (STATUS_COMPLETED, PHASE_FINALIZING, 100, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, 100, true),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, 100, true),
        "waiting_approval" => ("waiting_approval", "validating", 85, false),
        "queued" => ("queued", PHASE_INTAKE, 15, false),
        _ => (STATUS_RUNNING, PHASE_EXECUTING, 70, false),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        agent_status,
        phase,
        progress,
        &format!("RD 任务状态同步：{status}"),
        error_message.as_deref(),
        completed,
    )
    .await
}

async fn sync_rd_task_events_to_agent_ops(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    rd_task_id: &str,
    limit: i64,
) -> Result<()> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, stage, status, message, CAST(detail_json AS TEXT) AS detail_json,
               CAST(created_at AS TEXT) AS created_at
        FROM rd_task_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY id DESC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(rd_task_id)
    .bind(limit.clamp(1, 200))
    .fetch_all(state.control_db())
    .await?;
    for row in rows.into_iter().rev() {
        let rd_event_id: u64 = row.get("id");
        let stage: String = row.get("stage");
        let status: String = row.get("status");
        let message: Option<String> = row.get("message");
        let detail_json: Option<String> = row.get("detail_json");
        let created_at: Option<String> = row.get("created_at");
        let event_id = format!("agte-rd-{rd_event_id}");
        let metadata = json_to_string(Some(&json!({
            "source": "rd_task_events",
            "rdTaskId": rd_task_id,
            "rdEventId": rd_event_id,
            "detail": detail_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok()),
        })))?;
        sqlx::query::<sqlx::Sqlite>(
            r"
            INSERT OR IGNORE INTO agent_task_events
                (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))
            ",
        )
        .bind(event_id)
        .bind(tenant_id)
        .bind(agent_task_id)
        .bind(format!("rd.{stage}"))
        .bind(rd_stage_to_agent_phase(&stage))
        .bind(rd_status_to_agent_status(&status))
        .bind(rd_status_to_severity(&status))
        .bind(message.unwrap_or_else(|| format!("RD {stage} / {status}")))
        .bind(metadata)
        .bind(created_at)
        .execute(state.control_db())
        .await?;
    }
    Ok(())
}

fn rd_stage_to_agent_phase(stage: &str) -> &'static str {
    let normalized = stage.to_ascii_lowercase();
    if normalized.contains("context")
        || normalized.contains("retrieve")
        || normalized.contains("search")
        || normalized.contains("index")
        || normalized.contains("cache_usage")
        || normalized.contains("evidence")
    {
        PHASE_RETRIEVING
    } else if normalized.contains("plan") || normalized.contains("workflow_stage") {
        PHASE_PLANNING
    } else if normalized.contains("model") || normalized.contains("llm") {
        PHASE_MODEL_CALLING
    } else if normalized.contains("tool")
        || normalized.contains("command")
        || normalized.contains("test")
        || normalized.contains("runtime")
        || normalized.contains("candidate_worktree")
    {
        PHASE_EXECUTING
    } else if normalized.contains("approval")
        || normalized.contains("review")
        || normalized.contains("diff")
        || normalized.contains("validate")
        || normalized.contains("repair")
        || normalized.contains("workflow_post_stage")
    {
        PHASE_VALIDATING
    } else if normalized.contains("summary") || normalized.contains("final") {
        PHASE_FINALIZING
    } else {
        PHASE_EXECUTING
    }
}

fn rd_status_to_agent_status(status: &str) -> &'static str {
    match status {
        "completed" => STATUS_COMPLETED,
        "failed" => STATUS_FAILED,
        "cancelled" => STATUS_CANCELLED,
        "waiting_approval" => "waiting_approval",
        "queued" => "queued",
        _ => STATUS_RUNNING,
    }
}

fn rd_status_to_severity(status: &str) -> &'static str {
    match status {
        "failed" => "error",
        "cancelled" | "waiting_approval" => "warn",
        _ => "info",
    }
}

async fn sync_chat_adversarial_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    run_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, current_round, max_rounds, error_message,
               CAST(completed_at AS TEXT) AS completed_at
        FROM chat_adversarial_runs
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    let current_round: i32 = row.get("current_round");
    let max_rounds: i32 = row.get("max_rounds");
    let error_message: Option<String> = row.get("error_message");
    let progress = if max_rounds > 0 {
        20 + ((current_round.clamp(0, max_rounds) * 65) / max_rounds)
    } else {
        50
    };
    let (agent_status, phase, completed) = match status.as_str() {
        "completed" => (STATUS_COMPLETED, PHASE_FINALIZING, true),
        "failed" => (STATUS_FAILED, PHASE_FINALIZING, true),
        "cancelled" => (STATUS_CANCELLED, PHASE_FINALIZING, true),
        "queued" => ("queued", PHASE_INTAKE, false),
        _ => (STATUS_RUNNING, PHASE_DEBATING, false),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        agent_status,
        phase,
        if completed { 100 } else { progress },
        &format!("超级对抗状态同步：{status}，轮次 {current_round}/{max_rounds}"),
        error_message.as_deref(),
        completed,
    )
    .await
}

async fn sync_pm_research_task_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    pm_task_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, stage, error_message, CAST(completed_at AS TEXT) AS completed_at
        FROM pm_research_tasks
        WHERE tenant_id = ? AND task_id = ?
        ",
    )
    .bind(tenant_id)
    .bind(pm_task_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    let stage: Option<String> = row.get("stage");
    let error_message: Option<String> = row.get("error_message");
    let phase = match stage.as_deref().unwrap_or(status.as_str()) {
        "queued" => PHASE_INTAKE,
        "planning" | "plan" => PHASE_PLANNING,
        "retrieve" | "retrieving" => PHASE_RETRIEVING,
        "model_calling" | "synthesize" | "synthesizing" => PHASE_MODEL_CALLING,
        "validating" | "quality_gate" => PHASE_VALIDATING,
        "completed" | "failed" | "cancelled" | "cancelling" => PHASE_FINALIZING,
        _ => PHASE_EXECUTING,
    };
    let (agent_status, progress, completed) = match status.as_str() {
        "completed" => (STATUS_COMPLETED, 100, true),
        "failed" => (STATUS_FAILED, 100, true),
        "cancelled" => (STATUS_CANCELLED, 100, true),
        "cancelling" => (STATUS_CANCELLING, 95, false),
        "queued" => ("queued", 15, false),
        _ => (STATUS_RUNNING, 70, false),
    };
    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        agent_status,
        phase,
        progress,
        &format!(
            "产运深度分析状态同步：{}{}",
            status,
            stage
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!(" / {value}"))
                .unwrap_or_default()
        ),
        error_message.as_deref(),
        completed,
    )
    .await
}

async fn sync_nl2sql_agent_query_status(
    state: &AppState,
    tenant_id: &str,
    agent_task_id: &str,
    query_id: &str,
) -> Result<()> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT generated_sql, executed, CAST(rows_returned AS INTEGER) AS rows_returned,
               CAST(planning_ms AS INTEGER) AS planning_ms,
               CAST(execution_ms AS INTEGER) AS execution_ms,
               error_message
        FROM nl2sql_queries
        WHERE tenant_id = ? AND id = ? AND deleted_at IS NULL
        ",
    )
    .bind(tenant_id)
    .bind(query_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(());
    };

    let generated_sql: Option<String> = row.get("generated_sql");
    let executed = row.get::<i8, _>("executed") == 1;
    let rows_returned: Option<i64> = row.get("rows_returned");
    let planning_ms: Option<i64> = row.get("planning_ms");
    let execution_ms: Option<i64> = row.get("execution_ms");
    let error_message: Option<String> = row.get("error_message");
    let has_sql = generated_sql
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_error = error_message
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    let (agent_status, phase, progress, message, completed) = if has_error {
        (
            STATUS_FAILED,
            PHASE_FINALIZING,
            100,
            format!(
                "数据探索状态同步失败：{}",
                error_message.as_deref().unwrap_or("未知错误")
            ),
            true,
        )
    } else if executed {
        (
            STATUS_COMPLETED,
            PHASE_FINALIZING,
            100,
            format!(
                "数据探索状态同步：已执行，返回 {} 行",
                rows_returned.unwrap_or(0)
            ),
            true,
        )
    } else if has_sql {
        (
            STATUS_COMPLETED,
            PHASE_FINALIZING,
            100,
            "数据探索状态同步：SQL 已生成，等待用户确认执行".to_string(),
            true,
        )
    } else {
        (
            STATUS_RUNNING,
            PHASE_MODEL_CALLING,
            65,
            "数据探索状态同步：正在生成 SQL".to_string(),
            false,
        )
    };

    let metadata = json!({
        "source": "nl2sql_queries",
        "queryId": query_id,
        "executed": executed,
        "rowsReturned": rows_returned.unwrap_or(0),
        "planningMs": planning_ms.unwrap_or(0),
        "executionMs": execution_ms.unwrap_or(0),
        "hasGeneratedSql": has_sql,
    });
    let event_id = format!("agte-nl2sql-{agent_task_id}-{query_id}");
    let metadata_json = json_to_string(Some(&metadata))?;
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_task_events
            (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json, created_at)
        VALUES (?, ?, ?, 'nl2sql.status_synced', ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT DO UPDATE SET
            phase = excluded.phase,
            status = excluded.status,
            severity = excluded.severity,
            message = excluded.message,
            metadata_json = excluded.metadata_json,
            created_at = excluded.created_at
        ",
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(agent_task_id)
    .bind(phase)
    .bind(agent_status)
    .bind(if has_error { "error" } else { "info" })
    .bind(&message)
    .bind(metadata_json)
    .execute(state.control_db())
    .await?;
    let trace_id = format!("agtr-nl2sql-{agent_task_id}-{query_id}");
    let metadata_json = json_to_string(Some(&metadata))?;
    if let Err(error) = sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO agent_trace_events
            (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json, created_at)
        VALUES (?, ?, ?, 'nl2sql.status_synced', ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT DO UPDATE SET
            phase = excluded.phase,
            status = excluded.status,
            severity = excluded.severity,
            message = excluded.message,
            metadata_json = excluded.metadata_json,
            created_at = excluded.created_at
        ",
    )
    .bind(trace_id)
    .bind(tenant_id)
    .bind(agent_task_id)
    .bind(phase)
    .bind(agent_status)
    .bind(if has_error { "error" } else { "info" })
    .bind(&message)
    .bind(metadata_json)
    .execute(state.control_db())
    .await
    {
        tracing::warn!(
            tenant_id,
            task_id = agent_task_id,
            query_id,
            "failed to mirror NL2SQL status sync into trace events: {}",
            error
        );
    }

    sync_agent_task_from_resource(
        state,
        tenant_id,
        agent_task_id,
        agent_status,
        phase,
        progress,
        &message,
        error_message.as_deref(),
        completed,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn sync_agent_task_from_resource(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    status: &str,
    phase: &str,
    progress_percent: i32,
    message: &str,
    error_message: Option<&str>,
    completed: bool,
) -> Result<()> {
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let current = sqlx::query::<sqlx::Sqlite>(
        "SELECT status, phase, progress_percent, last_event
         FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        tx.rollback().await?;
        return Ok(());
    };
    let previous_status: String = current.get("status");
    if matches!(
        previous_status.as_str(),
        "completed" | "failed" | "cancelled" | "timed_out" | "stale"
    ) && previous_status != status
    {
        tx.rollback().await?;
        return Ok(());
    }
    if previous_status == STATUS_CANCELLING
        && !matches!(
            status,
            STATUS_COMPLETED | STATUS_FAILED | STATUS_CANCELLED | "timed_out" | STATUS_STALE
        )
    {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks SET last_heartbeat_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let changed = previous_status != status
        || current.get::<String, _>("phase") != phase
        || current.get::<i32, _>("progress_percent") != progress_percent.clamp(0, 100)
        || current.get::<Option<String>, _>("last_event").as_deref() != Some(message);
    if !changed {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks SET last_heartbeat_at = CURRENT_TIMESTAMP, updated_at = updated_at
             WHERE tenant_id = ? AND id = ? AND status NOT IN ('completed','failed','cancelled','timed_out','stale')",
        )
        .bind(tenant_id)
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE agent_tasks
        SET status = ?, phase = ?, progress_percent = ?, last_event = ?,
            progress_json = ?,
            error_message = COALESCE(?, error_message),
            queue_status = CASE
                WHEN ? = 'created' THEN 'queued'
                WHEN ? = 'queued' THEN 'queued'
                WHEN ? = 'claimed' THEN 'claimed'
                WHEN ? = 'retrying' THEN 'running'
                WHEN ? = 'running' THEN 'running'
                WHEN ? IN ('waiting_input','waiting_approval','blocked') THEN 'waiting_input'
                WHEN ? = 'cancelling' THEN 'cancelling'
                WHEN ? = 'cancelled' THEN 'cancelled'
                WHEN ? = 'completed' THEN 'succeeded'
                WHEN ? IN ('failed', 'timed_out', 'stale') THEN 'dead'
                ELSE queue_status
            END,
            claimed_by = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE claimed_by END,
            claimed_at = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE claimed_at END,
            lease_expires_at = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE lease_expires_at END,
            finished_at = CASE WHEN ? IN ('cancelled','completed','failed','timed_out','stale') THEN COALESCE(finished_at, CURRENT_TIMESTAMP) ELSE finished_at END,
            last_heartbeat_at = CURRENT_TIMESTAMP, last_progress_at = CURRENT_TIMESTAMP,
            completed_at = IIF(?, COALESCE(completed_at, CURRENT_TIMESTAMP), completed_at),
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(status)
    .bind(phase)
    .bind(progress_percent.clamp(0, 100))
    .bind(message)
    .bind(task_progress_json(status, phase, message))
    .bind(error_message)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(completed)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    if matches!(
        status,
        STATUS_COMPLETED | STATUS_FAILED | STATUS_CANCELLED | "timed_out" | STATUS_STALE
    ) {
        finish_latest_attempt_tx(&mut tx, tenant_id, task_id, status, None, error_message).await?;
    }
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        match status {
            STATUS_COMPLETED => "completed",
            STATUS_FAILED => "failed",
            STATUS_CANCELLED => "cancelled",
            STATUS_WAITING_INPUT => "waiting_input",
            "waiting_approval" => "waiting_approval",
            "blocked" => "blocked",
            "retrying" => "retrying",
            _ => "progress",
        },
        Some(phase),
        Some(status),
        if status == STATUS_FAILED {
            "error"
        } else if matches!(
            status,
            STATUS_CANCELLED | STATUS_WAITING_INPUT | "waiting_approval" | "blocked"
        ) {
            "warn"
        } else {
            "info"
        },
        message,
        error_message.map(|error| json!({ "error": error })),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn ask_watchdog_text_with_actions_and_audit(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    scope: Option<&str>,
    external_platform: Option<&str>,
    external_channel_id: Option<&str>,
    external_conversation_id: Option<&str>,
    allowed_actions: &[&str],
    action_audit: Option<WatchDogActionAudit>,
) -> Result<String> {
    let scope = scope.unwrap_or("conversation");
    let q = question.trim();
    let intent = parse_watchdog_intent(state, tenant_id, q, scope).await;
    let capability_filter = intent
        .capability
        .as_deref()
        .and_then(normalize_watchdog_capability)
        .or_else(|| detect_capability_filter(q));
    let normalized_scope = intent
        .scope
        .as_deref()
        .and_then(normalize_watchdog_scope)
        .unwrap_or(scope);
    let include_all = normalized_scope == "tenant"
        || q.contains("全部")
        || q.to_ascii_lowercase().contains("all")
        || scope == "tenant";
    let status_filter = normalized_status_filter(intent.status.as_deref());
    let stale_minutes = intent.stale_minutes.or_else(|| detect_stale_minutes(q));
    let limit = intent.limit.unwrap_or(20).clamp(1, 50);
    let tasks = query_watchdog_tasks(
        state,
        tenant_id,
        if include_all { None } else { external_platform },
        if include_all {
            None
        } else {
            external_channel_id
        },
        if include_all {
            None
        } else {
            external_conversation_id
        },
        capability_filter,
        stale_minutes,
        status_filter.as_deref(),
        limit,
    )
    .await?;

    if let Some(action) = parse_watchdog_action(q).or_else(|| watchdog_action_from_intent(&intent))
    {
        if tasks.is_empty() {
            return Ok("WatchDog 没查到匹配的 Agent 任务。可以回复“全部任务”扩大范围，或确认 Bot 是否已绑定对应能力。".to_string());
        }
        let Some(task) = tasks.get(action.index.saturating_sub(1)) else {
            return Ok(format!(
                "没有第 {} 个任务。请先回复“全部任务”或重新查询任务列表。",
                action.index
            ));
        };
        if !watchdog_action_allowed(action.kind, allowed_actions) {
            return Ok(format!(
                "当前看门狗 Bot 未开启“{}”动作权限。请到 Bot 绑定能力的高级配置中添加 allowActions 后再试。",
                watchdog_action_config_key(action.kind)
            ));
        }
        let audit = action_audit.unwrap_or_else(|| {
            WatchDogActionAudit::bot(
                "watchdog-bot",
                external_platform,
                external_channel_id,
                external_conversation_id,
            )
        });
        return run_watchdog_action(state, tenant_id, task, action.kind, &audit).await;
    }

    let queue_intent = intent
        .queue_intent
        .as_deref()
        .and_then(normalize_queue_intent)
        .or_else(|| detect_queue_intent(q));
    let mut answer = if let Some(intent) = queue_intent {
        build_watchdog_queue_answer(&tasks, intent)
    } else if tasks.is_empty() {
        "WatchDog 没查到匹配的 Agent 任务。可以回复“全部任务”扩大范围，或确认 Bot 是否已绑定对应能力。".to_string()
    } else {
        build_watchdog_rule_answer(&tasks)
    };
    if let Some(code_studio_answer) =
        build_watchdog_code_studio_answer(state, tenant_id, q, &tasks, include_all).await?
    {
        answer.push_str("\n\n");
        answer.push_str(&code_studio_answer);
    }
    if intent.needs_llm_summary.unwrap_or(true) {
        if let Some(summary) =
            summarize_watchdog_answer_with_llm(state, tenant_id, q, &answer).await
        {
            return Ok(summary);
        }
    }
    Ok(answer)
}

fn build_watchdog_queue_answer(tasks: &[AgentTaskInfo], intent: WatchDogQueueIntent) -> String {
    let mut queue_tasks = tasks
        .iter()
        .filter(|task| match intent {
            WatchDogQueueIntent::All => task.queue.status != "none",
            WatchDogQueueIntent::Dead => task.queue.status == "dead",
            WatchDogQueueIntent::StaleLease => {
                matches!(
                    task.queue.status.as_str(),
                    "claimed" | "running" | "cancelling"
                ) && task.queue.lease_expires_at.is_some()
            }
        })
        .collect::<Vec<_>>();
    queue_tasks.sort_by_key(|task| {
        (
            queue_priority(&task.queue.status),
            task.queue.priority,
            task.queue.available_at.clone().unwrap_or_default(),
        )
    });
    let title = match intent {
        WatchDogQueueIntent::All => "当前队列概览",
        WatchDogQueueIntent::Dead => "当前死信任务",
        WatchDogQueueIntent::StaleLease => "当前可能租约超时任务",
    };
    if queue_tasks.is_empty() {
        return format!("{title}：未发现匹配任务。");
    }
    let mut lines = vec![format!("{title}：{} 个", queue_tasks.len())];
    for (idx, task) in queue_tasks.into_iter().take(8).enumerate() {
        lines.push(String::new());
        lines.push(format!(
            "{}. {} · {} · {}",
            idx + 1,
            capability_display(&task.capability_key),
            task.queue.status,
            task.status
        ));
        lines.push(format!("   任务：{}", task.title));
        lines.push(format!(
            "   尝试：{}/{} · 优先级：{}",
            task.queue.attempt_count, task.queue.max_attempts, task.queue.priority
        ));
        if let Some(worker) = task.queue.claimed_by.as_deref() {
            lines.push(format!("   Worker：{worker}"));
        }
        if let Some(lease) = task.queue.lease_expires_at.as_deref() {
            lines.push(format!("   Lease 到期：{lease}"));
        }
        if let Some(reason) = task
            .queue
            .dead_reason
            .as_deref()
            .or(task.queue.last_error.as_deref())
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("   队列原因：{}", truncate_chars(reason, 100)));
        }
    }
    lines.push(String::new());
    lines.push("可回复：详情 1 / 取消 1 / 重试 1，或在 WebUI 点击“恢复队列”。".to_string());
    lines.join("\n")
}

fn queue_priority(status: &str) -> u8 {
    match status {
        "dead" => 0,
        "cancelling" => 1,
        "running" => 2,
        "claimed" => 3,
        "queued" => 4,
        "waiting_input" => 5,
        "succeeded" => 6,
        "cancelled" => 7,
        _ => 8,
    }
}

fn watchdog_action_config_key(kind: WatchDogActionKind) -> &'static str {
    agent_ops_adapters::WatchDogActionRegistry::with_builtins()
        .contract_for_kind(kind.as_contract_kind())
        .map(|contract| contract.audit_action)
        .unwrap_or("open_task")
}

fn watchdog_action_allowed(kind: WatchDogActionKind, allowed_actions: &[&str]) -> bool {
    let registry = agent_ops_adapters::WatchDogActionRegistry::with_builtins();
    let Some(contract) = registry.contract_for_kind(kind.as_contract_kind()) else {
        return false;
    };
    let key = contract.audit_action;
    let _required_permission = contract.permission;
    allowed_actions
        .iter()
        .any(|action| action.trim().eq_ignore_ascii_case(key))
}

fn build_watchdog_rule_answer(tasks: &[AgentTaskInfo]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("当前查到 {} 个 Agent 任务：", tasks.len()));
    for (idx, task) in tasks.iter().take(8).enumerate() {
        lines.push(String::new());
        lines.push(format!(
            "{}. {} · {} · {}",
            idx + 1,
            capability_display(&task.capability_key),
            task.status,
            task.phase
        ));
        lines.push(format!("   任务：{}", task.title));
        lines.push(format!(
            "   队列：{} · {}/{}",
            task.queue.status, task.queue.attempt_count, task.queue.max_attempts
        ));
        lines.push(format!(
            "   已运行：{}",
            elapsed_label(task.started_at.as_ref().or(Some(&task.created_at)))
        ));
        if let Some(event) = task.last_event.as_deref().filter(|v| !v.trim().is_empty()) {
            lines.push(format!("   最近事件：{}", truncate_chars(event, 80)));
        }
        if let Some(error) = task
            .error_message
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            lines.push(format!("   原因：{}", truncate_chars(error, 80)));
        }
        if let Some(reason) = task
            .queue
            .dead_reason
            .as_deref()
            .or(task.queue.last_error.as_deref())
            .filter(|v| !v.trim().is_empty())
        {
            lines.push(format!("   队列原因：{}", truncate_chars(reason, 80)));
        }
        if let Some(runtime) = &task.runtime_session {
            lines.push(format!(
                "   Runtime：{} · {}",
                runtime.status, runtime.isolation_mode
            ));
            if let Some(command) = runtime.current_command.as_deref() {
                lines.push(format!("   当前命令：{}", truncate_chars(command, 80)));
            }
        }
    }
    lines.push(String::new());
    lines.push("可回复：详情 1 / 取消 1 / 重试 1 / 全部任务".to_string());
    lines.join("\n")
}

async fn build_watchdog_code_studio_answer(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    tasks: &[AgentTaskInfo],
    allow_tenant_scan: bool,
) -> Result<Option<String>> {
    if !watchdog_question_mentions_code_studio(question) {
        return Ok(None);
    }
    let preview_rows =
        query_watchdog_preview_sessions(state, tenant_id, tasks, allow_tenant_scan, 5).await?;
    let code_intel_rows =
        query_watchdog_code_intel_sessions(state, tenant_id, tasks, allow_tenant_scan, 6).await?;
    if preview_rows.is_empty() && code_intel_rows.is_empty() {
        return Ok(Some(
            "Code Studio 证据：未发现最近的预览会话或 LSP 会话。".to_string(),
        ));
    }
    let mut lines = vec!["Code Studio 证据：".to_string()];
    if !preview_rows.is_empty() {
        lines.push("- 预览 / dev server：".to_string());
        for row in preview_rows {
            let status: String = row.get("status");
            let command: String = row.get("command");
            let port = row
                .try_get::<Option<u64>, _>("port")
                .ok()
                .flatten()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let session_id: String = row.get("id");
            let last_error: Option<String> = row.get("last_error");
            let event_type: Option<String> = row.get("event_type");
            let event_severity: Option<String> = row.get("event_severity");
            let event_message: Option<String> = row.get("event_message");
            lines.push(format!(
                "  · {} · :{} · {}",
                status,
                port,
                truncate_chars(&command, 90)
            ));
            lines.push(format!("    session：{session_id}"));
            if let Some(error) = last_error.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("    最近错误：{}", truncate_chars(&error, 120)));
            }
            if let (Some(event_type), Some(event_message)) = (event_type, event_message) {
                let severity = event_severity.unwrap_or_else(|| "info".to_string());
                lines.push(format!(
                    "    最近事件：{} {} · {}",
                    severity,
                    event_type,
                    truncate_chars(&event_message, 120)
                ));
            }
        }
    }
    if !code_intel_rows.is_empty() {
        lines.push("- LSP / 代码跳转：".to_string());
        for row in code_intel_rows {
            let language: String = row.get("language");
            let status: String = row.get("status");
            let server_command: Option<String> = row.get("server_command");
            let last_error: Option<String> = row.get("last_error");
            lines.push(format!(
                "  · {} · {} · {}",
                language,
                status,
                server_command.unwrap_or_else(|| "无 server command".to_string())
            ));
            if let Some(error) = last_error.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("    原因：{}", truncate_chars(&error, 120)));
            }
        }
    }
    lines.push(
        "可继续问：预览页面有什么 console error / LSP 为什么不能跳转 / RD 卡在哪。".to_string(),
    );
    Ok(Some(lines.join("\n")))
}

fn watchdog_question_mentions_code_studio(question: &str) -> bool {
    let lower = question.to_ascii_lowercase();
    question.contains("代码开发")
        || question.contains("预览")
        || question.contains("截图")
        || question.contains("跳转")
        || question.contains("控制台")
        || lower.contains("preview")
        || lower.contains("dev server")
        || lower.contains("console")
        || lower.contains("network")
        || lower.contains("lsp")
        || lower.contains("code intel")
}

async fn query_watchdog_preview_sessions(
    state: &AppState,
    tenant_id: &str,
    tasks: &[AgentTaskInfo],
    allow_tenant_scan: bool,
    limit: i64,
) -> Result<Vec<sqlx::sqlite::SqliteRow>> {
    let rd_task_ids = tasks
        .iter()
        .filter(|task| task.linked_resource_type.as_deref() == Some("rd_task"))
        .filter_map(|task| task.linked_resource_id.clone())
        .take(20)
        .collect::<Vec<_>>();
    if rd_task_ids.is_empty() {
        if !allow_tenant_scan {
            return Ok(Vec::new());
        }
        return sqlx::query::<sqlx::Sqlite>(
            r"
            SELECT ps.id, ps.repository_id, ps.task_id, ps.runtime_session_id, ps.process_id,
                   ps.command, ps.port, ps.url, ps.proxied_url, ps.status, ps.last_error,
                   pe.event_type, pe.severity AS event_severity, pe.message AS event_message,
                   CAST(ps.updated_at AS TEXT) AS updated_at
            FROM rd_preview_sessions ps
            LEFT JOIN rd_preview_events pe
              ON pe.tenant_id = ps.tenant_id
             AND pe.session_id = ps.id
             AND pe.created_at = (
               SELECT MAX(pe2.created_at)
               FROM rd_preview_events pe2
               WHERE pe2.tenant_id = ps.tenant_id AND pe2.session_id = ps.id
             )
            WHERE ps.tenant_id = ?
            ORDER BY ps.updated_at DESC
            LIMIT ?
            ",
        )
        .bind(tenant_id)
        .bind(limit.clamp(1, 10))
        .fetch_all(state.control_db())
        .await
        .map_err(AppError::Database);
    }
    let placeholders = vec!["?"; rd_task_ids.len()].join(",");
    let sql = format!(
        r"
        SELECT ps.id, ps.repository_id, ps.task_id, ps.runtime_session_id, ps.process_id,
               ps.command, ps.port, ps.url, ps.proxied_url, ps.status, ps.last_error,
               pe.event_type, pe.severity AS event_severity, pe.message AS event_message,
               CAST(ps.updated_at AS TEXT) AS updated_at
        FROM rd_preview_sessions ps
        LEFT JOIN rd_preview_events pe
          ON pe.tenant_id = ps.tenant_id
         AND pe.session_id = ps.id
         AND pe.created_at = (
           SELECT MAX(pe2.created_at)
           FROM rd_preview_events pe2
           WHERE pe2.tenant_id = ps.tenant_id AND pe2.session_id = ps.id
         )
        WHERE ps.tenant_id = ? AND ps.task_id IN ({placeholders})
        ORDER BY ps.updated_at DESC
        LIMIT ?
        "
    );
    let mut query = sqlx::query::<sqlx::Sqlite>(&sql).bind(tenant_id);
    for task_id in rd_task_ids {
        query = query.bind(task_id);
    }
    query
        .bind(limit.clamp(1, 10))
        .fetch_all(state.control_db())
        .await
        .map_err(AppError::Database)
}

async fn query_watchdog_code_intel_sessions(
    state: &AppState,
    tenant_id: &str,
    tasks: &[AgentTaskInfo],
    allow_tenant_scan: bool,
    limit: i64,
) -> Result<Vec<sqlx::sqlite::SqliteRow>> {
    let rd_task_ids = tasks
        .iter()
        .filter(|task| task.linked_resource_type.as_deref() == Some("rd_task"))
        .filter_map(|task| task.linked_resource_id.as_deref())
        .collect::<Vec<_>>();
    if rd_task_ids.is_empty() {
        if !allow_tenant_scan {
            return Ok(Vec::new());
        }
        return sqlx::query::<sqlx::Sqlite>(
            r"
            SELECT language, status, server_command, last_error,
                   CAST(updated_at AS TEXT) AS updated_at
            FROM rd_code_intel_sessions
            WHERE tenant_id = ?
            ORDER BY
              CASE WHEN status IN ('error','disconnected') THEN 0 ELSE 1 END,
              updated_at DESC
            LIMIT ?
            ",
        )
        .bind(tenant_id)
        .bind(limit.clamp(1, 12))
        .fetch_all(state.control_db())
        .await
        .map_err(AppError::Database);
    }
    let placeholders = vec!["?"; rd_task_ids.len()].join(",");
    let sql = format!(
        r"
        SELECT cis.language, cis.status, cis.server_command, cis.last_error,
               CAST(cis.updated_at AS TEXT) AS updated_at
        FROM rd_code_intel_sessions cis
        INNER JOIN rd_tasks rt
          ON rt.tenant_id = cis.tenant_id AND rt.repository_id = cis.repository_id
        WHERE cis.tenant_id = ? AND rt.id IN ({placeholders})
        ORDER BY
          CASE WHEN cis.status IN ('error','disconnected') THEN 0 ELSE 1 END,
          cis.updated_at DESC
        LIMIT ?
        "
    );
    let mut query = sqlx::query::<sqlx::Sqlite>(&sql).bind(tenant_id);
    for task_id in rd_task_ids {
        query = query.bind(task_id);
    }
    query
        .bind(limit.clamp(1, 12))
        .fetch_all(state.control_db())
        .await
        .map_err(AppError::Database)
}

async fn summarize_watchdog_answer_with_llm(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    evidence: &str,
) -> Option<String> {
    #[cfg(not(feature = "pm"))]
    {
        let _ = (state, tenant_id, question, evidence);
        return None;
    }
    #[cfg(feature = "pm")]
    {
        if std::env::var("AOS_WATCHDOG_LLM_SUMMARY")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "0" | "false" | "FALSE" | "off" | "OFF"))
        {
            return None;
        }
        let system_prompt = "你是 AOS 看门狗，只能根据给定 AgentOps 证据回答，不要编造不存在的任务、状态或原因。要求：中文，移动端友好，先给结论，再列 1-5 个关键任务；保留可执行建议，例如“详情 1 / 取消 1 / 重试 1”。";
        let prompt = format!("用户问题：\n{question}\n\nAgentOps 证据：\n{evidence}");
        match crate::routes::pm::run_chat_completion_with_any_chat_key(
            state,
            tenant_id,
            state.default_model.clone(),
            vec![crate::routes::chat::ChatMessage {
                role: "user".to_string(),
                content: Value::String(prompt),
            }],
            system_prompt,
            2048,
        )
        .await
        {
            Ok(result) => {
                let answer = result.answer.trim();
                if answer.is_empty() {
                    None
                } else {
                    Some(answer.to_string())
                }
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id,
                    error = %error,
                    "watchdog llm summary failed; using rule answer"
                );
                None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchDogActionKind {
    Detail,
    Cancel,
    Retry,
}

impl WatchDogActionKind {
    fn as_contract_kind(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Cancel => "cancel",
            Self::Retry => "retry",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WatchDogAction {
    kind: WatchDogActionKind,
    index: usize,
}

#[derive(Debug, Clone)]
pub struct WatchDogActionAudit {
    pub actor: String,
    pub source: String,
    pub external_platform: Option<String>,
    pub external_channel_id: Option<String>,
    pub external_conversation_id: Option<String>,
}

impl WatchDogActionAudit {
    fn webui(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: "webui".to_string(),
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
        }
    }

    pub(crate) fn bot(
        actor: impl Into<String>,
        external_platform: Option<&str>,
        external_channel_id: Option<&str>,
        external_conversation_id: Option<&str>,
    ) -> Self {
        Self {
            actor: actor.into(),
            source: "bot".to_string(),
            external_platform: external_platform.map(ToOwned::to_owned),
            external_channel_id: external_channel_id.map(ToOwned::to_owned),
            external_conversation_id: external_conversation_id.map(ToOwned::to_owned),
        }
    }

    pub(crate) fn command(actor: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: source.into(),
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
        }
    }

    fn metadata(&self, action: &str) -> Value {
        json!({
            "actor": self.actor,
            "source": self.source,
            "action": action,
            "externalPlatform": self.external_platform,
            "externalChannelId": self.external_channel_id,
            "externalConversationId": self.external_conversation_id,
        })
    }
}

fn parse_watchdog_action(question: &str) -> Option<WatchDogAction> {
    let compact = question.trim();
    let kind = if compact.starts_with("详情") || compact.to_ascii_lowercase().starts_with("detail")
    {
        WatchDogActionKind::Detail
    } else if compact.starts_with("取消") || compact.to_ascii_lowercase().starts_with("cancel") {
        WatchDogActionKind::Cancel
    } else if compact.starts_with("重试") || compact.to_ascii_lowercase().starts_with("retry") {
        WatchDogActionKind::Retry
    } else {
        return None;
    };
    let index = compact
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1);
    Some(WatchDogAction { kind, index })
}

fn watchdog_action_from_intent(
    intent: &crate::routes::agent_ops_watchdog_intent::WatchDogIntent,
) -> Option<WatchDogAction> {
    let (kind, index) = intent.action_contract_kind_index()?;
    let kind = match kind {
        "detail" => WatchDogActionKind::Detail,
        "cancel" => WatchDogActionKind::Cancel,
        "retry" => WatchDogActionKind::Retry,
        _ => return None,
    };
    Some(WatchDogAction { kind, index })
}

async fn run_watchdog_action(
    state: &AppState,
    tenant_id: &str,
    task: &AgentTaskInfo,
    kind: WatchDogActionKind,
    audit: &WatchDogActionAudit,
) -> Result<String> {
    let action_contract = agent_ops_adapters::WatchDogActionRegistry::with_builtins()
        .contract_for_kind(kind.as_contract_kind())
        .ok_or_else(|| AppError::ValidationError("unknown watchdog action".to_string()))?;
    match kind {
        WatchDogActionKind::Detail => {
            let events = list_task_events_for_watchdog(state, tenant_id, &task.id, 5).await?;
            let resource_events =
                list_linked_resource_events_for_watchdog(state, tenant_id, task, 5).await?;
            let code_studio_events =
                list_code_studio_events_for_watchdog(state, tenant_id, task, 5).await?;
            let mut lines = vec![
                format!("任务详情：{}", task.title),
                format!("能力：{}", capability_display(&task.capability_key)),
                format!("状态：{} / {}", task.status, task.phase),
                format!(
                    "队列：{}，尝试：{}/{}",
                    task.queue.status, task.queue.attempt_count, task.queue.max_attempts
                ),
                format!("任务ID：{}", task.id),
            ];
            if let Some(claimed_by) = task.queue.claimed_by.as_deref() {
                lines.push(format!("Worker：{claimed_by}"));
            }
            if let Some(lease) = task.queue.lease_expires_at.as_deref() {
                lines.push(format!("Lease 到期：{lease}"));
            }
            if let (Some(resource_type), Some(resource_id)) = (
                task.linked_resource_type.as_deref(),
                task.linked_resource_id.as_deref(),
            ) {
                lines.push(format!("关联资源：{resource_type} / {resource_id}"));
            }
            if let Some(reason) = task
                .queue
                .dead_reason
                .as_deref()
                .or(task.queue.last_error.as_deref())
                .filter(|v| !v.trim().is_empty())
            {
                lines.push(format!("队列原因：{}", truncate_chars(reason, 180)));
            }
            if let Some(error) = task
                .error_message
                .as_deref()
                .filter(|v| !v.trim().is_empty())
            {
                lines.push(format!("错误：{}", truncate_chars(error, 160)));
            }
            if !events.is_empty() {
                lines.push(String::new());
                lines.push("最近事件：".to_string());
                for event in events {
                    lines.push(format!(
                        "- {} · {}",
                        event.event_type,
                        truncate_chars(&event.message, 100)
                    ));
                }
            }
            if !resource_events.is_empty() {
                lines.push(String::new());
                lines.push("真实能力事件：".to_string());
                for event in resource_events {
                    lines.push(format!("- {}", truncate_chars(&event, 120)));
                }
            }
            if !code_studio_events.is_empty() {
                lines.push(String::new());
                lines.push("Code Studio 证据：".to_string());
                for event in code_studio_events {
                    lines.push(format!("- {}", truncate_chars(&event, 140)));
                }
            }
            Ok(lines.join("\n"))
        }
        WatchDogActionKind::Cancel => {
            cancel_agent_task_by_id(state, tenant_id, &task.id, audit).await?;
            Ok(format!(
                "{}：{}\n任务ID：{}",
                action_contract.display_name, task.title, task.id
            ))
        }
        WatchDogActionKind::Retry => {
            let result = request_retry_agent_task_by_id(state, tenant_id, &task.id, audit).await?;
            let retry_task_line = result
                .get("agentTaskId")
                .and_then(Value::as_str)
                .map(|id| format!("\n新任务ID：{id}"))
                .unwrap_or_default();
            let retry_resource_line = match (
                result.get("linkedResourceType").and_then(Value::as_str),
                result.get("linkedResourceId").and_then(Value::as_str),
            ) {
                (Some(resource_type), Some(resource_id)) => {
                    format!("\n新关联资源：{resource_type} / {resource_id}")
                }
                _ => String::new(),
            };
            Ok(format!(
                "已请求重试：{}\n原任务ID：{}{}{}\n结果：{}",
                task.title,
                task.id,
                retry_task_line,
                retry_resource_line,
                result
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("重试请求已处理")
            ))
        }
    }
}

async fn list_linked_resource_events_for_watchdog(
    state: &AppState,
    tenant_id: &str,
    task: &AgentTaskInfo,
    limit: i64,
) -> Result<Vec<String>> {
    match (
        task.linked_resource_type.as_deref(),
        task.linked_resource_id.as_deref(),
    ) {
        (Some("rd_task"), Some(task_id)) => {
            let rows = sqlx::query::<sqlx::Sqlite>(
                r"
                SELECT stage, status, message, CAST(created_at AS TEXT) AS created_at
                FROM rd_task_events
                WHERE tenant_id = ? AND task_id = ?
                ORDER BY id DESC
                LIMIT ?
                ",
            )
            .bind(tenant_id)
            .bind(task_id)
            .bind(limit.clamp(1, 20))
            .fetch_all(state.control_db())
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let stage: String = row.get("stage");
                    let status: String = row.get("status");
                    let message: Option<String> = row.get("message");
                    format!("{stage}/{status} · {}", message.unwrap_or_default())
                })
                .collect())
        }
        (Some("chat_adversarial_run"), Some(run_id)) => {
            let Some(row) = sqlx::query::<sqlx::Sqlite>(
                r"
                SELECT status, current_round, max_rounds, winner_model, winner_reason, error_message
                FROM chat_adversarial_runs
                WHERE tenant_id = ? AND id = ?
                ",
            )
            .bind(tenant_id)
            .bind(run_id)
            .fetch_optional(state.control_db())
            .await?
            else {
                return Ok(Vec::new());
            };
            let status: String = row.get("status");
            let current_round: i32 = row.get("current_round");
            let max_rounds: i32 = row.get("max_rounds");
            let winner_model: Option<String> = row.get("winner_model");
            let winner_reason: Option<String> = row.get("winner_reason");
            let error_message: Option<String> = row.get("error_message");
            let mut items = vec![format!("状态 {status} · 轮次 {current_round}/{max_rounds}")];
            if let Some(winner_model) = winner_model {
                items.push(format!("胜出模型：{winner_model}"));
            }
            if let Some(reason) = winner_reason.filter(|v| !v.trim().is_empty()) {
                items.push(format!("裁判结论：{reason}"));
            }
            if let Some(error) = error_message.filter(|v| !v.trim().is_empty()) {
                items.push(format!("错误：{error}"));
            }
            Ok(items)
        }
        (Some("pm_research_task"), Some(task_id)) => {
            let rows = sqlx::query::<sqlx::Sqlite>(
                r"
                SELECT status, stage, message, error_message, CAST(created_at AS TEXT) AS created_at
                FROM pm_research_task_events
                WHERE tenant_id = ? AND task_id = ?
                ORDER BY seq DESC
                LIMIT ?
                ",
            )
            .bind(tenant_id)
            .bind(task_id)
            .bind(limit.clamp(1, 20))
            .fetch_all(state.control_db())
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let status: String = row.get("status");
                    let stage: Option<String> = row.get("stage");
                    let message: Option<String> = row.get("message");
                    let error: Option<String> = row.get("error_message");
                    let mut text = format!(
                        "{}/{}",
                        stage
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or("pm"),
                        status
                    );
                    if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
                        text.push_str(" · ");
                        text.push_str(&message);
                    }
                    if let Some(error) = error.filter(|value| !value.trim().is_empty()) {
                        text.push_str(" · 错误：");
                        text.push_str(&error);
                    }
                    text
                })
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

async fn list_task_events_for_watchdog(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    limit: i64,
) -> Result<Vec<AgentTaskEventInfo>> {
    let rows = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        SELECT {AGENT_TASK_EVENT_SELECT} FROM agent_task_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY created_at DESC
        LIMIT ?
        "
    ))
    .bind(tenant_id)
    .bind(task_id)
    .bind(limit.clamp(1, 20))
    .fetch_all(state.control_db())
    .await?;
    Ok(rows.into_iter().map(event_from_row).collect())
}

async fn list_code_studio_events_for_watchdog(
    state: &AppState,
    tenant_id: &str,
    task: &AgentTaskInfo,
    limit: i64,
) -> Result<Vec<String>> {
    let Some(rd_task_id) = task
        .linked_resource_type
        .as_deref()
        .filter(|value| *value == "rd_task")
        .and(task.linked_resource_id.as_deref())
    else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT ps.id, ps.status, ps.command, ps.port, ps.last_error,
               pe.event_type, pe.severity, pe.message
        FROM rd_preview_sessions ps
        LEFT JOIN rd_preview_events pe
          ON pe.tenant_id = ps.tenant_id
         AND pe.session_id = ps.id
         AND pe.created_at = (
           SELECT MAX(pe2.created_at)
           FROM rd_preview_events pe2
           WHERE pe2.tenant_id = ps.tenant_id AND pe2.session_id = ps.id
         )
        WHERE ps.tenant_id = ? AND ps.task_id = ?
        ORDER BY ps.updated_at DESC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(rd_task_id)
    .bind(limit.clamp(1, 10))
    .fetch_all(state.control_db())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let id: String = row.get("id");
            let status: String = row.get("status");
            let command: String = row.get("command");
            let port = row
                .try_get::<Option<u64>, _>("port")
                .ok()
                .flatten()
                .map(|value| format!(":{value}"))
                .unwrap_or_default();
            let event = row
                .try_get::<Option<String>, _>("message")
                .ok()
                .flatten()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    row.get::<Option<String>, _>("last_error")
                        .unwrap_or_default()
                });
            let event_type: Option<String> = row.get("event_type");
            let severity: Option<String> = row.get("severity");
            format!(
                "Preview {id} · {status}{port} · {}{}{}",
                truncate_chars(&command, 80),
                event_type
                    .as_deref()
                    .map(|value| format!(" · {value}"))
                    .unwrap_or_default(),
                if event.trim().is_empty() {
                    String::new()
                } else {
                    format!(
                        " · {}{}",
                        severity
                            .as_deref()
                            .map(|value| format!("{value}: "))
                            .unwrap_or_default(),
                        truncate_chars(&event, 100)
                    )
                }
            )
        })
        .collect())
}

pub(crate) async fn cancel_agent_task_by_id(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    audit: &WatchDogActionAudit,
) -> Result<()> {
    let linked = sqlx::query::<sqlx::Sqlite>(
        "SELECT linked_resource_type, linked_resource_id, origin_turn_id, owner_user_id,
                (SELECT links.resource_id FROM agent_task_resource_links links
                 WHERE links.tenant_id = agent_tasks.tenant_id
                   AND links.task_id = agent_tasks.id
                   AND links.resource_type = 'bot_inbound_message'
                 ORDER BY links.created_at ASC, links.id ASC LIMIT 1) AS bot_inbound_message_id
         FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(linked) = linked else {
        return Err(AppError::NotFound(format!(
            "agent task '{task_id}' not found"
        )));
    };
    let resource_type: Option<String> = linked.get("linked_resource_type");
    let resource_id: Option<String> = linked.get("linked_resource_id");
    let origin_turn_id: Option<String> = linked.get("origin_turn_id");
    let owner_user_id: Option<String> = linked.get("owner_user_id");
    let bot_inbound_message_id: Option<String> = linked.get("bot_inbound_message_id");
    if let Some(resource_type) = resource_type.as_deref() {
        ensure_linked_resource_cancel_supported(resource_type)?;
    }
    let has_execution = resource_type.is_some() && resource_id.is_some()
        || origin_turn_id.is_some()
        || bot_inbound_message_id.is_some();
    let status = if has_execution {
        STATUS_CANCELLING
    } else {
        STATUS_CANCELLED
    };
    let transitioned = transition_task_with_event(
        state,
        tenant_id,
        task_id,
        status,
        PHASE_FINALIZING,
        "用户请求取消任务",
        if has_execution { 95 } else { 100 },
        if has_execution {
            "cancel_requested"
        } else {
            "cancelled"
        },
        "warn",
        Some(audit.metadata("cancel_task")),
    )
    .await?;
    if !transitioned {
        return Ok(());
    }
    if let Some(inbound_log_id) = bot_inbound_message_id.as_deref() {
        let is_primary_bot_resource = resource_type.as_deref() == Some("bot_inbound_message")
            && resource_id.as_deref() == Some(inbound_log_id);
        if !is_primary_bot_resource {
            if let Err(error) = cancel_linked_resource(
                state,
                tenant_id,
                owner_user_id.as_deref(),
                "bot_inbound_message",
                inbound_log_id,
            )
            .await
            {
                add_event(
                    state,
                    tenant_id,
                    task_id,
                    "cancel_dispatch_failed",
                    Some(PHASE_FINALIZING),
                    Some(STATUS_CANCELLING),
                    "error",
                    "取消请求未能送达 Bot 入站执行队列",
                    Some(json!({
                        "resourceType": "bot_inbound_message",
                        "resourceId": inbound_log_id,
                        "error": error.to_string(),
                    })),
                )
                .await?;
                return Err(error);
            }
        }
    }
    if let (Some(resource_type), Some(resource_id)) =
        (resource_type.as_deref(), resource_id.as_deref())
    {
        if let Err(error) = cancel_linked_resource(
            state,
            tenant_id,
            owner_user_id.as_deref(),
            resource_type,
            resource_id,
        )
        .await
        {
            add_event(
                state,
                tenant_id,
                task_id,
                "cancel_dispatch_failed",
                Some(PHASE_FINALIZING),
                Some(STATUS_CANCELLING),
                "error",
                "取消请求未能送达真实执行器",
                Some(json!({
                    "resourceType": resource_type,
                    "resourceId": resource_id,
                    "error": error.to_string(),
                })),
            )
            .await?;
            return Err(error);
        }
        if let Err(error) = sync_linked_resource_status_inner(
            state,
            tenant_id,
            task_id,
            Some(resource_type),
            Some(resource_id),
        )
        .await
        {
            tracing::error!(
                tenant_id,
                task_id,
                resource_type,
                resource_id,
                error = %error,
                error_debug = ?error,
                "cancel reached the executor but immediate AgentOps projection failed"
            );
        }
    }
    #[cfg(feature = "bot-agents")]
    if resource_type.as_deref() != Some("super_assistant_turn")
        && !resource_type
            .as_deref()
            .is_some_and(|value| value.starts_with("super_assistant_subtask:"))
    {
        if let Some(turn_id) = origin_turn_id.as_deref() {
            let claims = claims_for_task_owner(state, tenant_id, owner_user_id.as_deref()).await?;
            if let Err(error) = super::super_assistant_parent::cancel_parent_turn(
                axum::extract::State(state.clone()),
                axum::extract::Extension(claims),
                axum::extract::Path(turn_id.to_string()),
            )
            .await
            {
                add_event(
                    state,
                    tenant_id,
                    task_id,
                    "cancel_dispatch_failed",
                    Some(PHASE_FINALIZING),
                    Some(STATUS_CANCELLING),
                    "error",
                    "取消请求未能送达超级助手父任务",
                    Some(json!({ "turnId": turn_id, "error": error.to_string() })),
                )
                .await?;
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) async fn cancel_linked_resource(
    state: &AppState,
    tenant_id: &str,
    owner_user_id: Option<&str>,
    resource_type: &str,
    resource_id: &str,
) -> Result<()> {
    #[cfg(not(any(feature = "bot-agents", feature = "nl2sql")))]
    let _ = owner_user_id;
    ensure_linked_resource_cancel_supported(resource_type)?;
    match resource_type {
        "rd_task" => {
            if let Some(runtime) = find_runtime_session_for_linked_resource(
                state,
                tenant_id,
                resource_type,
                resource_id,
            )
            .await?
            {
                crate::routes::agent_runtime::request_cancel_runtime_session(
                    state, tenant_id, &runtime,
                )
                .await?;
            }
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE rd_tasks SET status = 'cancelled', completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP) WHERE tenant_id = ? AND id = ? AND status IN ('queued','running','waiting_approval')",
            )
            .bind(tenant_id)
            .bind(resource_id)
            .execute(state.control_db())
            .await?;
        }
        "chat_adversarial_run" => {
            #[cfg(feature = "agent")]
            crate::routes::agent::request_chat_adversarial_cancel_from_agent_ops(
                state,
                tenant_id,
                resource_id,
            )
            .await?;
            #[cfg(not(feature = "agent"))]
            return Err(AppError::ValidationError(
                "super adversarial cancel requires the web-server agent feature to be enabled"
                    .to_string(),
            ));
        }
        "pm_research_task" => {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE pm_research_tasks
                 SET cancel_requested = 1,
                     status = CASE
                         WHEN status IN ('completed','failed','cancelled') THEN status
                         ELSE 'cancelling'
                     END,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND task_id = ?
                   AND (? IS NULL OR user_id = ?)
                   AND status NOT IN ('completed','failed','cancelled')",
            )
            .bind(tenant_id)
            .bind(resource_id)
            .bind(owner_user_id)
            .bind(owner_user_id)
            .execute(state.control_db())
            .await?;
        }
        "pm_material_job" => {
            let job_id = resource_id.parse::<u64>().map_err(|_| {
                AppError::ValidationError("invalid PM material job resource id".to_string())
            })?;
            let status = sqlx::query_scalar::<sqlx::Sqlite, String>(
                "SELECT status FROM pm_material_jobs
                 WHERE tenant_id = ? AND id = ? AND (? IS NULL OR created_by = ?) LIMIT 1",
            )
            .bind(tenant_id)
            .bind(crate::sqlite_i64(job_id))
            .bind(owner_user_id)
            .bind(owner_user_id)
            .fetch_optional(state.control_db())
            .await?
            .ok_or_else(|| AppError::NotFound("PM material job not found".to_string()))?;
            if !matches!(status.as_str(), "completed" | "failed" | "cancelled") {
                sqlx::query::<sqlx::Sqlite>(
                    "UPDATE pm_material_jobs SET status = 'cancelling', updated_at = CURRENT_TIMESTAMP
                     WHERE tenant_id = ? AND id = ? AND (? IS NULL OR created_by = ?)
                       AND status IN ('queued','running','cancelling')",
                )
                .bind(tenant_id)
                .bind(crate::sqlite_i64(job_id))
                .bind(owner_user_id)
                .bind(owner_user_id)
                .execute(state.control_db())
                .await?;
            }
        }
        "bot_inbound_message" => {
            let updated = sqlx::query::<sqlx::Sqlite>(
                "UPDATE bot_message_logs
                 SET status = CASE
                       WHEN queue_status = 'claimed' THEN 'cancelling'
                       ELSE 'cancelled'
                     END,
                     finished_at = CASE
                       WHEN queue_status = 'queued' THEN COALESCE(finished_at, CURRENT_TIMESTAMP)
                       ELSE finished_at
                     END,
                     queue_status = CASE
                       WHEN queue_status = 'claimed' THEN 'cancelling'
                       ELSE 'cancelled'
                     END,
                     claimed_by = NULL, claimed_at = NULL,
                     last_error = 'cancelled by user', error_message = NULL,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND id = ? AND direction = 'inbound'
                   AND queue_status IN ('queued','claimed')",
            )
            .bind(tenant_id)
            .bind(resource_id)
            .execute(state.control_db())
            .await?;
            if updated.rows_affected() == 0 {
                let exists = sqlx::query_scalar::<sqlx::Sqlite, i64>(
                    "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_message_logs
                     WHERE tenant_id = ? AND id = ? AND direction = 'inbound'",
                )
                .bind(tenant_id)
                .bind(resource_id)
                .fetch_one(state.control_db())
                .await?;
                if exists == 0 {
                    return Err(AppError::NotFound(
                        "Bot inbound message resource not found".to_string(),
                    ));
                }
            }
        }
        "super_assistant_turn" => {
            #[cfg(feature = "bot-agents")]
            {
                let owner_user_id = sqlx::query_scalar::<sqlx::Sqlite, Option<String>>(
                    "SELECT owner_user_id FROM agent_tasks
                     WHERE tenant_id = ? AND (
                       (linked_resource_type = 'super_assistant_turn' AND linked_resource_id = ?)
                       OR id IN (
                         SELECT task_id FROM agent_task_resource_links
                         WHERE tenant_id = ? AND resource_type = 'super_assistant_turn'
                           AND resource_id = ?
                       )
                     )
                     ORDER BY updated_at DESC LIMIT 1",
                )
                .bind(tenant_id)
                .bind(resource_id)
                .bind(tenant_id)
                .bind(resource_id)
                .fetch_optional(state.control_db())
                .await?
                .flatten();
                let claims =
                    claims_for_task_owner(state, tenant_id, owner_user_id.as_deref()).await?;
                let _ = super::super_assistant_parent::cancel_parent_turn(
                    axum::extract::State(state.clone()),
                    axum::extract::Extension(claims),
                    axum::extract::Path(resource_id.to_string()),
                )
                .await?;
            }
            #[cfg(not(feature = "bot-agents"))]
            return Err(AppError::ValidationError(
                "super assistant cancellation requires bot-agents".to_string(),
            ));
        }
        "nl2sql_attribution_task" => {
            #[cfg(feature = "nl2sql")]
            {
                let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
                let _ = super::nl2sql::attribution::cancel_attribution_task(
                    axum::extract::State(state.clone()),
                    axum::extract::Extension(claims),
                    axum::extract::Path(resource_id.to_string()),
                )
                .await?;
            }
            #[cfg(not(feature = "nl2sql"))]
            return Err(AppError::ValidationError(
                "data-attribution cancellation requires nl2sql".to_string(),
            ));
        }
        value if value.starts_with("super_assistant_subtask:") => {
            #[cfg(feature = "bot-agents")]
            {
                let engine = value
                    .strip_prefix("super_assistant_subtask:")
                    .unwrap_or_default();
                let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
                super::super_assistant_parent::cancel_subtask_from_agent_ops(
                    state,
                    &claims,
                    engine,
                    resource_id,
                )
                .await?;
            }
            #[cfg(not(feature = "bot-agents"))]
            return Err(AppError::ValidationError(
                "super assistant subtask cancellation requires bot-agents".to_string(),
            ));
        }
        _ => unreachable!("unsupported linked resource type was rejected"),
    }
    Ok(())
}

pub(crate) fn linked_resource_cancel_supported(resource_type: &str) -> bool {
    matches!(
        resource_type,
        "rd_task"
            | "chat_adversarial_run"
            | "pm_research_task"
            | "pm_material_job"
            | "bot_inbound_message"
            | "super_assistant_turn"
    ) || (resource_type == "nl2sql_attribution_task" && cfg!(feature = "nl2sql"))
        || matches!(
            resource_type,
            "super_assistant_subtask:deep_research"
                | "super_assistant_subtask:nl2sql"
                | "super_assistant_subtask:data_attribution"
                | "super_assistant_subtask:super_adversarial"
        )
}

pub(crate) fn linked_resource_retry_supported(resource_type: &str) -> bool {
    matches!(
        resource_type,
        "rd_task" | "pm_research_task" | "chat_adversarial_run" | "nl2sql_agent_query"
    )
}

fn ensure_linked_resource_cancel_supported(resource_type: &str) -> Result<()> {
    if linked_resource_cancel_supported(resource_type) {
        Ok(())
    } else {
        Err(AppError::ValidationError(format!(
            "linked resource type '{resource_type}' does not support cancellation"
        )))
    }
}

pub(crate) async fn find_runtime_session_for_linked_resource(
    state: &AppState,
    tenant_id: &str,
    resource_type: &str,
    resource_id: &str,
) -> Result<Option<String>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT ars.id
        FROM agent_runtime_sessions ars
        JOIN agent_tasks at ON at.tenant_id = ars.tenant_id AND at.id = ars.agent_task_id
        WHERE ars.tenant_id = ?
          AND at.linked_resource_type = ?
          AND at.linked_resource_id = ?
        ORDER BY ars.created_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(state.control_db())
    .await?;
    Ok(row.map(|row| row.get("id")))
}

pub(crate) async fn request_retry_agent_task_by_id(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    audit: &WatchDogActionAudit,
) -> Result<Value> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT source, source_label, capability_key, agent_id, agent_name, title, summary,
               owner_user_id, correlation_id, external_platform, external_channel_id,
               external_conversation_id, external_message_id,
               linked_resource_type, linked_resource_id
        FROM agent_tasks
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("agent task '{task_id}' not found")))?;
    let source: String = row.get("source");
    let source_label: Option<String> = row.get("source_label");
    let capability_key: String = row.get("capability_key");
    let agent_id: Option<String> = row.get("agent_id");
    let agent_name: Option<String> = row.get("agent_name");
    let title: String = row.get("title");
    let summary: Option<String> = row.get("summary");
    let owner_user_id: Option<String> = row.get("owner_user_id");
    let correlation_id: Option<String> = row.get("correlation_id");
    let external_platform: Option<String> = row.get("external_platform");
    let external_channel_id: Option<String> = row.get("external_channel_id");
    let external_conversation_id: Option<String> = row.get("external_conversation_id");
    let external_message_id: Option<String> = row.get("external_message_id");
    let resource_type: Option<String> = row.get("linked_resource_type");
    let resource_id: Option<String> = row.get("linked_resource_id");

    let retry_title = format!("重试：{title}");
    let retry_outcome = create_task_with_outcome(
        state,
        CreateAgentTaskInput {
            tenant_id: tenant_id.to_string(),
            source: "watchdog".to_string(),
            source_ref: Some(task_id.to_string()),
            source_label: Some(format!(
                "WatchDog retry{}",
                source_label
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!(" · {value}"))
                    .unwrap_or_default()
            )),
            capability_key: capability_key.clone(),
            agent_id,
            agent_name,
            title: retry_title,
            summary,
            owner_user_id: owner_user_id.clone(),
            correlation_id,
            parent_task_id: Some(task_id.to_string()),
            external_platform,
            external_channel_id,
            external_conversation_id,
            external_message_id,
            idempotency_key: audit
                .source
                .strip_prefix("command:")
                .map(|command_id| format!("task-retry:{task_id}:{command_id}")),
            input_json: Some(json!({
                "action": "retry_task",
                "audit": audit.metadata("retry_task"),
                "sourceAgentTaskId": task_id,
                "source": source,
                "previousLinkedResourceType": resource_type,
                "previousLinkedResourceId": resource_id,
            })),
        },
    )
    .await?;
    let child_task_id = retry_outcome.id;
    if retry_outcome.reused {
        let existing = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, linked_resource_type, linked_resource_id, last_event
             FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(&child_task_id)
        .fetch_one(state.control_db())
        .await?;
        let existing_status: String = existing.get("status");
        let existing_resource_type: Option<String> = existing.get("linked_resource_type");
        let existing_resource_id: Option<String> = existing.get("linked_resource_id");
        if existing_resource_type.is_some() && existing_resource_id.is_some() {
            let _ = sync_linked_resource_status(state, tenant_id, &child_task_id).await;
            return Ok(json!({
                "status": existing_status,
                "message": existing.get::<Option<String>, _>("last_event"),
                "linkedResourceType": existing_resource_type,
                "linkedResourceId": existing_resource_id,
                "agentTaskId": child_task_id,
                "parentAgentTaskId": task_id,
                "reused": true,
            }));
        }
        if existing_status == STATUS_COMPLETED {
            return Ok(json!({
                "status": existing_status,
                "message": existing.get::<Option<String>, _>("last_event"),
                "agentTaskId": child_task_id,
                "parentAgentTaskId": task_id,
                "reused": true,
            }));
        }
        if matches!(
            existing_status.as_str(),
            STATUS_CREATED | "retrying" | STATUS_RUNNING
        ) {
            let error = "retry worker restarted before the execution resource was durably linked; automatic replay was blocked to avoid duplicate side effects";
            fail_task(
                state,
                tenant_id,
                &child_task_id,
                "retry_recovery_ambiguous",
                error,
            )
            .await?;
            add_event(
                state,
                tenant_id,
                task_id,
                "retry_recovery_blocked",
                Some(PHASE_EXECUTING),
                Some(STATUS_FAILED),
                "error",
                "重试恢复缺少真实执行资源引用，已阻止盲目重放",
                Some(json!({
                    "audit": audit.metadata("retry_task"),
                    "retryAgentTaskId": child_task_id,
                    "previousLinkedResourceType": resource_type,
                    "previousLinkedResourceId": resource_id,
                })),
            )
            .await?;
            return Err(AppError::Conflict(error.to_string()));
        }
        return Err(AppError::Conflict(format!(
            "retry attempt '{child_task_id}' already ended with status '{existing_status}'"
        )));
    }
    transition_task_with_event(
        state,
        tenant_id,
        &child_task_id,
        "retrying",
        PHASE_EXECUTING,
        "WatchDog 已登记重试任务，正在创建真实能力任务",
        5,
        "retrying",
        "info",
        Some(json!({ "parentTaskId": task_id })),
    )
    .await?;

    let mut retry_result = match retry_linked_resource(
        state,
        tenant_id,
        owner_user_id.as_deref(),
        resource_type.as_deref(),
        resource_id.as_deref(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let error_message = error.to_string();
            fail_task(
                state,
                tenant_id,
                &child_task_id,
                "retry_failed",
                &error_message,
            )
            .await?;
            add_event(
                state,
                tenant_id,
                task_id,
                "retry_failed",
                Some(PHASE_EXECUTING),
                Some(STATUS_FAILED),
                "error",
                "WatchDog 重试失败",
                Some(json!({
                    "audit": audit.metadata("retry_task"),
                    "capabilityKey": capability_key,
                    "retryAgentTaskId": child_task_id,
                    "previousLinkedResourceType": resource_type,
                    "previousLinkedResourceId": resource_id,
                    "error": error_message,
                })),
            )
            .await?;
            return Err(error);
        }
    };
    let message = retry_result
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("用户请求重试任务")
        .to_string();

    let new_resource_type = retry_result
        .get("linkedResourceType")
        .and_then(Value::as_str)
        .map(str::to_string);
    let new_resource_id = retry_result
        .get("linkedResourceId")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(resource_type), Some(resource_id)) =
        (new_resource_type.as_deref(), new_resource_id.as_deref())
    {
        link_task_resource(state, tenant_id, &child_task_id, resource_type, resource_id).await?;
    }
    transition_task_with_event(
        state,
        tenant_id,
        &child_task_id,
        STATUS_RUNNING,
        PHASE_EXECUTING,
        &message,
        10,
        "progress",
        "info",
        Some(json!({
            "parentTaskId": task_id,
            "linkedResourceType": new_resource_type,
            "linkedResourceId": new_resource_id,
        })),
    )
    .await?;
    if let Err(error) = sync_linked_resource_status(state, tenant_id, &child_task_id).await {
        tracing::warn!(
            tenant_id,
            parent_task_id = task_id,
            child_task_id,
            "failed to sync retry child linked resource status: {}",
            error
        );
    }
    add_event(
        state,
        tenant_id,
        task_id,
        "retry_requested",
        Some(PHASE_EXECUTING),
        Some(STATUS_RUNNING),
        "warn",
        &message,
        Some(json!({
            "audit": audit.metadata("retry_task"),
            "capabilityKey": capability_key,
            "retryAgentTaskId": child_task_id,
            "previousLinkedResourceType": resource_type,
            "previousLinkedResourceId": resource_id,
            "retry": retry_result,
        })),
    )
    .await?;
    if let Some(map) = retry_result.as_object_mut() {
        map.insert("agentTaskId".to_string(), json!(child_task_id));
        map.insert("parentAgentTaskId".to_string(), json!(task_id));
    }
    Ok(retry_result)
}

#[allow(dead_code)]
pub(crate) async fn claims_for_task_owner(
    state: &AppState,
    tenant_id: &str,
    owner_user_id: Option<&str>,
) -> Result<Claims> {
    let Some(owner_user_id) = owner_user_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(AppError::ValidationError(
            "task owner is required for safe retry".to_string(),
        ));
    };
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT email, role FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(tenant_id)
    .bind(owner_user_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::ValidationError("task owner is inactive or missing".to_string()))?;
    let email: String = row.get("email");
    let role: String = row.get("role");
    Ok(Claims::new(owner_user_id, &email, &role, tenant_id))
}

async fn retry_linked_resource(
    state: &AppState,
    tenant_id: &str,
    owner_user_id: Option<&str>,
    resource_type: Option<&str>,
    resource_id: Option<&str>,
) -> Result<Value> {
    match (resource_type, resource_id) {
        (Some("rd_task"), Some(resource_id)) => {
            #[cfg(feature = "rd")]
            {
                let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
                let result = crate::routes::rd::retry_task_from_agent_ops(
                    state.clone(),
                    claims,
                    resource_id,
                )
                .await?;
                Ok(json!({
                    "status": "retry_started",
                    "message": "已创建 RD 重试任务",
                    "linkedResourceType": "rd_task",
                    "linkedResourceId": result.id,
                    "title": result.title,
                    "rdStatus": result.status,
                    "mode": result.mode,
                }))
            }
            #[cfg(not(feature = "rd"))]
            {
                let _ = (state, tenant_id, owner_user_id, resource_id);
                Err(AppError::ValidationError(
                    "rd retry requires the web-server rd feature to be enabled".to_string(),
                ))
            }
        }
        (Some("pm_research_task"), Some(resource_id)) => {
            #[cfg(feature = "agent")]
            {
                let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
                let result = crate::routes::agent::resume_pm_research_task_from_agent_ops(
                    state,
                    claims,
                    resource_id,
                )
                .await?;
                Ok(json!({
                    "status": "retry_started",
                    "message": "已恢复产运深度分析任务",
                    "linkedResourceType": "pm_research_task",
                    "linkedResourceId": result.task_id,
                    "sessionId": result.session_id,
                    "pmStatus": result.status,
                    "restarted": result.restarted,
                }))
            }
            #[cfg(not(feature = "agent"))]
            {
                let _ = (state, tenant_id, owner_user_id, resource_id);
                Err(AppError::ValidationError(
                    "pm research retry requires the web-server agent feature to be enabled"
                        .to_string(),
                ))
            }
        }
        (Some("chat_adversarial_run"), Some(resource_id)) => {
            #[cfg(feature = "agent")]
            {
                retry_chat_adversarial_run(state, tenant_id, owner_user_id, resource_id).await
            }
            #[cfg(not(feature = "agent"))]
            {
                let _ = (state, tenant_id, owner_user_id, resource_id);
                Err(AppError::ValidationError(
                    "super adversarial retry requires the web-server agent feature to be enabled"
                        .to_string(),
                ))
            }
        }
        (Some("nl2sql_agent_query"), Some(resource_id)) => {
            retry_nl2sql_agent_query(state, tenant_id, owner_user_id, resource_id).await
        }
        (Some(other), Some(_)) => Err(AppError::ValidationError(format!(
            "retry is not supported for linked resource type '{other}'"
        ))),
        _ => Err(AppError::ValidationError(
            "task has no linked resource to retry safely".to_string(),
        )),
    }
}

#[cfg(feature = "agent")]
async fn retry_chat_adversarial_run(
    state: &AppState,
    tenant_id: &str,
    owner_user_id: Option<&str>,
    run_id: &str,
) -> Result<Value> {
    let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT question, CAST(models_json AS TEXT) AS models_json,
               CAST(max_rounds AS INTEGER) AS max_rounds
        FROM chat_adversarial_runs
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("chat adversarial run '{run_id}' not found")))?;

    let question: String = row.get("question");
    let models_json: Option<String> = row.get("models_json");
    let models = models_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| Vec::new());
    let max_rounds = row
        .try_get::<Option<i64>, _>("max_rounds")
        .ok()
        .flatten()
        .and_then(|value| u32::try_from(value).ok());
    let result = crate::routes::agent::start_chat_adversarial_run_from_bot(
        state.clone(),
        claims,
        crate::routes::agent::ChatAdversarialBotRunInput {
            question,
            models,
            max_rounds,
            parent_run_id: Some(run_id.to_string()),
            session_id: None,
            evidence_search_required: false,
            evidence_search_query: None,
        },
    )
    .await?;
    Ok(json!({
        "status": "retry_started",
        "message": "已创建超级对抗重试任务",
        "linkedResourceType": "chat_adversarial_run",
        "linkedResourceId": result.id,
        "threadId": result.thread_id,
        "adversarialStatus": result.status,
        "currentRound": result.current_round,
        "maxRounds": result.max_rounds,
    }))
}

#[cfg(not(feature = "nl2sql"))]
async fn retry_nl2sql_agent_query(
    _state: &AppState,
    _tenant_id: &str,
    _owner_user_id: Option<&str>,
    _query_id: &str,
) -> Result<Value> {
    Err(AppError::ValidationError(
        "nl2sql retry requires the web-server nl2sql feature to be enabled".to_string(),
    ))
}

#[cfg(feature = "nl2sql")]
async fn retry_nl2sql_agent_query(
    state: &AppState,
    tenant_id: &str,
    owner_user_id: Option<&str>,
    query_id: &str,
) -> Result<Value> {
    let claims = claims_for_task_owner(state, tenant_id, owner_user_id).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT question, data_source_id, conversation_id, routing_method
        FROM nl2sql_queries
        WHERE tenant_id = ? AND id = ? AND deleted_at IS NULL
        ",
    )
    .bind(tenant_id)
    .bind(query_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("NL2SQL query '{query_id}' not found")))?;

    let question: String = row.get("question");
    let data_source_id: Option<String> = row.get("data_source_id");
    let conversation_id: Option<String> = row.get("conversation_id");
    let routing_method: Option<String> = row.get("routing_method");
    let conversation_id = conversation_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("retry-{query_id}"));

    if let Some(data_source_id) = data_source_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let resp = crate::routes::nl2sql::query_request(
            State(state.clone()),
            Extension(claims),
            Json(crate::routes::nl2sql::QueryRequest {
                data_source_id: data_source_id.to_string(),
                question,
                conversation_id: Some(conversation_id.clone()),
                route_confidence: None,
                routing_method: Some(
                    routing_method
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| "watchdog_retry".to_string()),
                ),
                semantic_context: Some(json!({
                    "retryOfQueryId": query_id,
                    "source": "watchdog_retry",
                })),
                reference_bindings: None,
            }),
        )
        .await?
        .0;
        return Ok(json!({
            "status": "retry_started",
            "message": "已创建数据探索重试查询",
            "linkedResourceType": "nl2sql_agent_query",
            "linkedResourceId": resp.query_id,
            "conversationId": resp.conversation_id,
            "hasSql": resp.sql.is_some(),
            "error": resp.error,
        }));
    }

    let resp = crate::routes::nl2sql::execute_agent_request(
        state,
        &claims,
        crate::routes::nl2sql::AgentExecuteRequest {
            question,
            retrieval_question: None,
            preferred_model: None,
            shared_context: None,
            datasource_ids: Vec::new(),
            conversation_id: Some(conversation_id.clone()),
            max_steps: None,
            bounded: false,
        },
    )
    .await?;
    let new_query_id = resp
        .query_id
        .clone()
        .ok_or_else(|| AppError::Internal("NL2SQL retry did not return a query id".to_string()))?;
    Ok(json!({
        "status": "retry_started",
        "message": "已创建数据探索 Agent 重试查询",
        "linkedResourceType": "nl2sql_agent_query",
        "linkedResourceId": new_query_id,
        "conversationId": resp.conversation_id,
        "totalSteps": resp.total_steps,
        "totalExecutionMs": resp.total_execution_ms,
        "error": resp.error,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn transition_task_with_event(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    status: &str,
    phase: &str,
    message: &str,
    progress_percent: i32,
    event_type: &str,
    severity: &str,
    metadata_json: Option<Value>,
) -> Result<bool> {
    let mut tx = state.control_db().begin().await?;
    if !update_task_state_tx(
        &mut tx,
        tenant_id,
        task_id,
        status,
        phase,
        message,
        progress_percent,
    )
    .await?
    {
        tx.rollback().await?;
        return Ok(false);
    }
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        event_type,
        Some(phase),
        Some(status),
        severity,
        message,
        metadata_json,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn update_task_state_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    status: &str,
    phase: &str,
    message: &str,
    progress_percent: i32,
) -> Result<bool> {
    let updated = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE agent_tasks
        SET status = ?, phase = ?, progress_percent = ?, last_event = ?,
            progress_json = ?, last_progress_at = CURRENT_TIMESTAMP,
            desired_state = CASE
                WHEN ? IN ('cancelling','cancelled') THEN 'cancelled'
                ELSE desired_state
            END,
            queue_status = CASE
                WHEN ? = 'created' THEN 'queued'
                WHEN ? = 'queued' THEN 'queued'
                WHEN ? = 'claimed' THEN 'claimed'
                WHEN ? = 'retrying' THEN 'running'
                WHEN ? = 'running' THEN 'running'
                WHEN ? IN ('waiting_input','waiting_approval','blocked') THEN 'waiting_input'
                WHEN ? = 'cancelling' THEN 'cancelling'
                WHEN ? = 'cancelled' THEN 'cancelled'
                WHEN ? = 'completed' THEN 'succeeded'
                WHEN ? IN ('failed', 'timed_out', 'stale') THEN 'dead'
                ELSE queue_status
            END,
            claimed_by = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE claimed_by END,
            claimed_at = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE claimed_at END,
            lease_expires_at = CASE WHEN ? IN ('waiting_input','waiting_approval','blocked','cancelled','completed','failed','timed_out','stale') THEN NULL ELSE lease_expires_at END,
            finished_at = CASE WHEN ? IN ('cancelled','completed','failed','timed_out','stale') THEN COALESCE(finished_at, CURRENT_TIMESTAMP) ELSE finished_at END,
            last_heartbeat_at = CURRENT_TIMESTAMP,
            started_at = COALESCE(started_at, CURRENT_TIMESTAMP),
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
          AND (status NOT IN ('completed','failed','cancelled','timed_out','stale') OR status = ?)
        ",
    )
    .bind(status)
    .bind(phase)
    .bind(progress_percent.clamp(0, 100))
    .bind(message)
    .bind(task_progress_json(status, phase, message))
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(status)
    .bind(tenant_id)
    .bind(task_id)
    .bind(status)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        > 0;
    if !updated {
        return Ok(false);
    }
    if matches!(status, "claimed" | "running" | "retrying") {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_attempts
             SET status = 'running', started_at = COALESCE(started_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND task_id = ? AND status IN ('created','retrying')",
        )
        .bind(tenant_id)
        .bind(task_id)
        .execute(&mut **tx)
        .await?;
    } else if matches!(status, "cancelled" | "timed_out" | "stale") {
        finish_latest_attempt_tx(tx, tenant_id, task_id, status, None, None).await?;
    }
    Ok(true)
}

async fn finish_latest_attempt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    status: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_task_attempts
         SET status = ?, completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
             error_code = COALESCE(?, error_code), error_message = COALESCE(?, error_message),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND task_id = ? AND status NOT IN ('completed','failed','cancelled','timed_out','stale')",
    )
    .bind(status)
    .bind(error_code)
    .bind(error_message)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg_attr(not(feature = "rd"), allow(dead_code))]
pub async fn mark_task_cancelled(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<()> {
    transition_task_with_event(
        state,
        tenant_id,
        task_id,
        STATUS_CANCELLED,
        PHASE_FINALIZING,
        message,
        100,
        "cancelled",
        "warn",
        metadata_json,
    )
    .await
    .map(|_| ())
}

#[cfg(feature = "agent")]
pub async fn mark_task_cancelling(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    message: &str,
    metadata_json: Option<Value>,
) -> Result<()> {
    transition_task_with_event(
        state,
        tenant_id,
        task_id,
        STATUS_CANCELLING,
        PHASE_FINALIZING,
        message,
        95,
        "cancel_requested",
        "warn",
        metadata_json,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn mark_task_stale_if_heartbeat_expired(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    stale_seconds: i64,
) -> Result<bool> {
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    // Decode the integer sentinel explicitly instead of treating it as a boolean.
    let eligible: Option<i64> = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT 1 FROM agent_tasks
         WHERE tenant_id = ? AND id = ?
           AND status IN ('created','queued','claimed','running','retrying','cancelling')
           AND last_heartbeat_at < datetime(CURRENT_TIMESTAMP, printf('%+d seconds', -?))",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(stale_seconds.max(60))
    .fetch_optional(&mut *tx)
    .await?;
    if eligible.is_none() {
        tx.rollback().await?;
        return Ok(false);
    }
    if !update_task_state_tx(
        &mut tx,
        tenant_id,
        task_id,
        STATUS_STALE,
        PHASE_FINALIZING,
        "执行器心跳已失联，任务停止自动恢复",
        100,
    )
    .await?
    {
        tx.rollback().await?;
        return Ok(false);
    }
    add_event_tx(
        &mut tx,
        tenant_id,
        task_id,
        "stale",
        Some(PHASE_FINALIZING),
        Some(STATUS_STALE),
        "error",
        "执行器心跳已失联，任务停止自动恢复",
        Some(json!({ "staleAfterSeconds": stale_seconds.max(60) })),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

async fn capabilities() -> Json<Value> {
    Json(json!({
        "items": capability_contracts(),
        "botPlatforms": bot_platform_contracts(),
        "runtimes": runtime_contracts(),
        "watchdogActions": watchdog_action_contracts(),
    }))
}

fn log_agent_ops_error(endpoint: &str, tenant_id: &str, error: &AppError) {
    tracing::error!(
        endpoint,
        tenant_id,
        error = %error,
        app_error = ?error,
        "agent ops endpoint failed"
    );
}

async fn summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    match summary_inner(&state, &claims).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("summary", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn summary_inner(state: &AppState, claims: &Claims) -> Result<Json<Value>> {
    tracing::debug!(tenant_id = %claims.tenant_id, "agent ops summary request started");
    require_watchdog_permission(state, claims, "watchdog:read").await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, CAST(COUNT(*) AS INTEGER) AS count
        FROM agent_tasks
        WHERE tenant_id = ?
        GROUP BY status
        ",
    )
    .bind(&claims.tenant_id)
    .fetch_all(state.control_db())
    .await?;
    let by_status = rows
        .into_iter()
        .map(|row| json!({ "status": row.get::<String, _>("status"), "count": row.get::<i64, _>("count") }))
        .collect::<Vec<_>>();
    let running: i64 = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks WHERE tenant_id = ? AND status IN ('queued','claimed','running','retrying','cancelling')",
    )
    .bind(&claims.tenant_id)
    .fetch_one(state.control_db())
    .await?;
    let stale: i64 = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        r"
        SELECT CAST(COUNT(*) AS INTEGER)
        FROM agent_tasks
        WHERE tenant_id = ?
          AND (
            status = 'stale'
            OR (
              status IN ('queued','claimed','running','retrying','cancelling')
              AND COALESCE(last_heartbeat_at, updated_at) < datetime(CURRENT_TIMESTAMP, '-10 minutes')
            )
          )
        ",
    )
    .bind(&claims.tenant_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(Json(json!({
        "running": running,
        "stale": stale,
        "byStatus": by_status,
        "capabilities": capability_contracts(),
    })))
}

async fn agents(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    match agents_inner(&state, &claims).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("agents", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn agents_inner(state: &AppState, claims: &Claims) -> Result<Json<Value>> {
    tracing::debug!(tenant_id = %claims.tenant_id, "agent ops agents request started");
    require_watchdog_permission(state, claims, "watchdog:read").await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT capability_key,
               CAST(COUNT(*) AS INTEGER) AS total,
               CAST(SUM(CASE WHEN status IN ('queued','claimed','running','waiting_input','retrying') THEN 1 ELSE 0 END) AS INTEGER) AS active,
               CAST(SUM(CASE WHEN status IN ('failed','timed_out') AND updated_at >= datetime(CURRENT_TIMESTAMP, '-24 hours') THEN 1 ELSE 0 END) AS INTEGER) AS failed_24h,
               MAX(updated_at) AS last_updated_at
        FROM agent_tasks
        WHERE tenant_id = ?
        GROUP BY capability_key
        ORDER BY active DESC, total DESC, capability_key ASC
        ",
    )
    .bind(&claims.tenant_id)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let capability_key: String = row.get("capability_key");
            json!({
                "capabilityKey": capability_key,
                "displayName": capability_display(&capability_key),
                "total": row.get::<i64, _>("total"),
                "active": row.get::<Option<i64>, _>("active").unwrap_or(0),
                "failed24h": row.get::<Option<i64>, _>("failed_24h").unwrap_or(0),
                "lastUpdatedAt": row_datetime_opt(&row, "last_updated_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<ListResponse<AgentTaskInfo>>> {
    match tasks_inner(&state, &claims, query).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("tasks", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn tasks_inner(
    state: &AppState,
    claims: &Claims,
    query: TaskListQuery,
) -> Result<Json<ListResponse<AgentTaskInfo>>> {
    tracing::debug!(tenant_id = %claims.tenant_id, "agent ops tasks request started");
    require_watchdog_permission(state, claims, "watchdog:read").await?;
    let pagination = PaginationParams {
        page: query.page,
        per_page: query.per_page,
    };
    let where_sql = task_filter_sql(&query);
    let count_sql = format!(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks at WHERE at.tenant_id = ?{where_sql}"
    );
    let mut count_query =
        sqlx::query_scalar::<sqlx::Sqlite, i64>(&count_sql).bind(&claims.tenant_id);
    if let Some(value) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .source
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .external_conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .linked_resource_type
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .linked_resource_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count_query = count_query.bind(value);
    }
    let total = count_query.fetch_one(state.control_db()).await?;

    let list_sql = format!(
        "SELECT {AGENT_TASK_SELECT} FROM agent_tasks at {AGENT_TASK_RUNTIME_JOIN} WHERE at.tenant_id = ?{where_sql} ORDER BY at.updated_at DESC, at.created_at DESC LIMIT ? OFFSET ?"
    );
    let mut list_query = sqlx::query::<sqlx::Sqlite>(&list_sql).bind(&claims.tenant_id);
    if let Some(value) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .source
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .external_conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .linked_resource_type
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .linked_resource_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        list_query = list_query.bind(value);
    }
    let rows = list_query
        .bind(pagination.limit())
        .bind(pagination.offset())
        .fetch_all(state.control_db())
        .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(task_from_row).collect(),
        total,
    }))
}

async fn task_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<AgentTaskInfo>> {
    match task_detail_inner(&state, &claims, &id).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("task_detail", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn task_detail_inner(
    state: &AppState,
    claims: &Claims,
    id: &str,
) -> Result<Json<AgentTaskInfo>> {
    require_watchdog_permission(&state, &claims, "watchdog:read").await?;
    let row = sqlx::query::<sqlx::Sqlite>(&format!(
        "SELECT {AGENT_TASK_SELECT} FROM agent_tasks at {AGENT_TASK_RUNTIME_JOIN} WHERE at.tenant_id = ? AND at.id = ?"
    ))
        .bind(&claims.tenant_id)
        .bind(id)
        .fetch_optional(state.control_db())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent task '{id}' not found")))?;
    Ok(Json(task_from_row(row)))
}

async fn task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<ListResponse<AgentTaskEventInfo>>> {
    match task_events_inner(&state, &claims, &id, pagination).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("task_events", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn task_events_inner(
    state: &AppState,
    claims: &Claims,
    id: &str,
    pagination: PaginationParams,
) -> Result<Json<ListResponse<AgentTaskEventInfo>>> {
    require_watchdog_permission(&state, &claims, "watchdog:read").await?;
    let total: i64 = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_events WHERE tenant_id = ? AND task_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(id)
    .fetch_one(state.control_db())
    .await?;
    let rows = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        SELECT {AGENT_TASK_EVENT_SELECT} FROM agent_task_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY created_at DESC
        LIMIT ? OFFSET ?
        "
    ))
    .bind(&claims.tenant_id)
    .bind(id)
    .bind(pagination.limit())
    .bind(pagination.offset())
    .fetch_all(state.control_db())
    .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(event_from_row).collect(),
        total,
    }))
}

async fn ensure_task_visible(state: &AppState, tenant_id: &str, task_id: &str) -> Result<()> {
    let exists = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_one(state.control_db())
    .await?;
    if exists > 0 {
        Ok(())
    } else {
        Err(AppError::NotFound(format!(
            "agent task '{task_id}' not found"
        )))
    }
}

async fn task_trace_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<ListResponse<AgentTraceEventInfo>>> {
    require_watchdog_permission(&state, &claims, "watchdog:read").await?;
    ensure_task_visible(&state, &claims.tenant_id, &id).await?;
    let total = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_trace_events WHERE tenant_id = ? AND task_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_one(state.control_db())
    .await?;
    let rows = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        SELECT {AGENT_TRACE_EVENT_SELECT} FROM agent_trace_events
        WHERE tenant_id = ? AND task_id = ?
        ORDER BY created_at DESC
        LIMIT ? OFFSET ?
        "
    ))
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(pagination.limit())
    .bind(pagination.offset())
    .fetch_all(state.control_db())
    .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(trace_event_from_row).collect(),
        total,
    }))
}

async fn cancel_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    require_watchdog_permission(&state, &claims, "watchdog:admin").await?;
    let audit = WatchDogActionAudit::webui(&claims.sub);
    cancel_agent_task_by_id(&state, &claims.tenant_id, &id, &audit).await?;
    let status = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("agent task '{id}' not found")))?;
    Ok(Json(json!({ "ok": true, "status": status })))
}

async fn retry_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    require_watchdog_permission(&state, &claims, "watchdog:admin").await?;
    let audit = WatchDogActionAudit::webui(&claims.sub);
    let result = request_retry_agent_task_by_id(&state, &claims.tenant_id, &id, &audit).await?;
    Ok(Json(json!({
        "ok": true,
        "status": result.get("status").and_then(Value::as_str).unwrap_or("retry_requested"),
        "message": result.get("message").and_then(Value::as_str).unwrap_or("重试请求已处理"),
        "result": result,
    })))
}

async fn queue_recover(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<QueueRecoverRequest>,
) -> Result<Json<Value>> {
    require_watchdog_permission(&state, &claims, "watchdog:admin").await?;
    let timeout_secs = req.lease_timeout_secs.unwrap_or(600).clamp(30, 86_400);
    let result = recover_agent_task_queue(&state, &claims.tenant_id, timeout_secs).await?;
    Ok(Json(result))
}

async fn queue_items(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<QueueListQuery>,
) -> Result<Json<ListResponse<AgentTaskInfo>>> {
    require_watchdog_permission(&state, &claims, "watchdog:read").await?;
    let pagination = PaginationParams {
        page: query.page,
        per_page: query.per_page,
    };
    let lease_timeout_secs = query.lease_timeout_secs.unwrap_or(600).clamp(30, 86_400);
    let mut where_sql = String::from(" WHERE at.tenant_id = ?");
    if query.dead_only.unwrap_or(false) {
        where_sql.push_str(" AND at.queue_status = 'dead'");
    } else if let Some(value) = query
        .queue_status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let _ = value;
        where_sql.push_str(" AND at.queue_status = ?");
    } else {
        where_sql.push_str(" AND at.queue_status <> 'none'");
    }
    if query
        .capability_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        where_sql.push_str(" AND at.capability_key = ?");
    }
    if query
        .worker_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        where_sql.push_str(" AND at.claimed_by = ?");
    }
    if query.stale_only.unwrap_or(false) {
        where_sql.push_str(&format!(
            " AND at.queue_status IN ('claimed','running','cancelling') AND ((at.lease_expires_at IS NOT NULL AND at.lease_expires_at < CURRENT_TIMESTAMP) OR (at.lease_expires_at IS NULL AND at.claimed_at IS NOT NULL AND at.claimed_at < datetime(CURRENT_TIMESTAMP, '-{lease_timeout_secs} seconds')))"
        ));
    }
    let count_sql = format!("SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks at{where_sql}");
    let mut count_query =
        sqlx::query_scalar::<sqlx::Sqlite, i64>(&count_sql).bind(&claims.tenant_id);
    if !query.dead_only.unwrap_or(false) {
        if let Some(value) = query
            .queue_status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            count_query = count_query.bind(value);
        }
    }
    if let Some(value) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        count_query = count_query.bind(value);
    }
    if let Some(value) = query
        .worker_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        count_query = count_query.bind(value);
    }
    let total = count_query.fetch_one(state.control_db()).await?;
    let list_sql = format!(
        "SELECT {AGENT_TASK_SELECT} FROM agent_tasks at {AGENT_TASK_RUNTIME_JOIN}{where_sql} ORDER BY at.priority ASC, at.available_at ASC, at.updated_at DESC LIMIT ? OFFSET ?"
    );
    let mut list_query = sqlx::query::<sqlx::Sqlite>(&list_sql).bind(&claims.tenant_id);
    if !query.dead_only.unwrap_or(false) {
        if let Some(value) = query
            .queue_status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            list_query = list_query.bind(value);
        }
    }
    if let Some(value) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        list_query = list_query.bind(value);
    }
    if let Some(value) = query
        .worker_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        list_query = list_query.bind(value);
    }
    let rows = list_query
        .bind(pagination.limit())
        .bind(pagination.offset())
        .fetch_all(state.control_db())
        .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(task_from_row).collect(),
        total,
    }))
}

pub async fn recover_agent_task_queue(
    state: &AppState,
    tenant_id: &str,
    timeout_secs: i64,
) -> Result<Value> {
    let timeout_secs = timeout_secs.clamp(30, 86_400);
    let dead_rows = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        SELECT id
        FROM agent_tasks
        WHERE tenant_id = ?
          AND queue_status IN ('claimed','running','cancelling')
          AND (
            (lease_expires_at IS NOT NULL AND lease_expires_at < CURRENT_TIMESTAMP)
            OR (lease_expires_at IS NULL AND claimed_at IS NOT NULL AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds'))
          )
          AND attempt_count >= max_attempts
        ",
        timeout_secs
    ))
    .bind(tenant_id)
    .fetch_all(state.control_db())
    .await?;
    let recovered_rows = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        SELECT id
        FROM agent_tasks
        WHERE tenant_id = ?
          AND queue_status IN ('claimed','running')
          AND (
            (lease_expires_at IS NOT NULL AND lease_expires_at < CURRENT_TIMESTAMP)
            OR (lease_expires_at IS NULL AND claimed_at IS NOT NULL AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds'))
          )
          AND attempt_count < max_attempts
        ",
        timeout_secs
    ))
    .bind(tenant_id)
    .fetch_all(state.control_db())
    .await?;

    let dead = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        UPDATE agent_tasks
        SET status = 'failed',
            queue_status = 'dead',
            claimed_by = NULL,
            claimed_at = NULL,
            lease_expires_at = NULL,
            last_error = 'agent task lease timed out and max attempts were exhausted',
            dead_reason = 'agent task lease timed out and max attempts were exhausted',
            finished_at = CURRENT_TIMESTAMP,
            completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ?
          AND queue_status IN ('claimed','running','cancelling')
          AND (
            (lease_expires_at IS NOT NULL AND lease_expires_at < CURRENT_TIMESTAMP)
            OR (lease_expires_at IS NULL AND claimed_at IS NOT NULL AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds'))
          )
          AND attempt_count >= max_attempts
        ",
        timeout_secs
    ))
    .bind(tenant_id)
    .execute(state.control_db())
    .await?;
    let recovered = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        UPDATE agent_tasks
        SET status = 'queued',
            queue_status = 'queued',
            phase = 'intake',
            claimed_by = NULL,
            claimed_at = NULL,
            lease_expires_at = NULL,
            available_at = CURRENT_TIMESTAMP,
            last_error = 'agent task lease timed out; returned to queue',
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ?
          AND queue_status IN ('claimed','running')
          AND (
            (lease_expires_at IS NOT NULL AND lease_expires_at < CURRENT_TIMESTAMP)
            OR (lease_expires_at IS NULL AND claimed_at IS NOT NULL AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds'))
          )
          AND attempt_count < max_attempts
        ",
        timeout_secs
    ))
    .bind(tenant_id)
    .execute(state.control_db())
    .await?;

    for row in dead_rows {
        let task_id: String = row.get("id");
        let _ = sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_attempts
             SET status = 'failed', completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                 error_code = 'lease_exhausted',
                 error_message = 'execution lease expired and maximum attempts were exhausted'
             WHERE id = (
                 SELECT id FROM agent_task_attempts
                 WHERE tenant_id = ? AND task_id = ?
                   AND status IN ('created','claimed','running')
                 ORDER BY attempt_no DESC LIMIT 1
             )",
        )
        .bind(tenant_id)
        .bind(&task_id)
        .execute(state.control_db())
        .await;
        let _ = add_event(
            state,
            tenant_id,
            &task_id,
            "queue.dead",
            Some(PHASE_FINALIZING),
            Some(STATUS_FAILED),
            "error",
            "Agent task lease 超时且超过最大尝试次数，已进入 dead",
            Some(json!({ "timeoutSecs": timeout_secs })),
        )
        .await;
    }
    for row in recovered_rows {
        let task_id: String = row.get("id");
        let _ = sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_attempts
             SET status = 'failed', completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                 error_code = 'lease_lost',
                 error_message = 'execution lease expired; a new recovery attempt will be created'
             WHERE id = (
                 SELECT id FROM agent_task_attempts
                 WHERE tenant_id = ? AND task_id = ?
                   AND status IN ('created','claimed','running')
                 ORDER BY attempt_no DESC LIMIT 1
             )",
        )
        .bind(tenant_id)
        .bind(&task_id)
        .execute(state.control_db())
        .await;
        let _ = add_event(
            state,
            tenant_id,
            &task_id,
            "queue.recovered",
            Some(PHASE_INTAKE),
            Some("queued"),
            "warn",
            "Agent task lease 超时，已回收到队列",
            Some(json!({ "timeoutSecs": timeout_secs })),
        )
        .await;
    }
    Ok(json!({
        "ok": true,
        "timeoutSecs": timeout_secs,
        "dead": dead.rows_affected(),
        "recovered": recovered.rows_affected(),
    }))
}

#[cfg_attr(not(feature = "rd"), allow(dead_code))]
pub async fn claim_agent_task(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let lease_secs = lease_secs.clamp(30, 86_400);
    let mut tx = state.control_db().begin().await?;
    let result = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        UPDATE agent_tasks
        SET status = 'claimed',
            queue_status = 'claimed',
            claimed_by = ?,
            claimed_at = CURRENT_TIMESTAMP,
            lease_expires_at = datetime(CURRENT_TIMESTAMP, '+{} seconds'),
            attempt_count = attempt_count + 1,
            last_error = NULL,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ?
          AND id = ?
          AND queue_status = 'queued'
          AND available_at <= CURRENT_TIMESTAMP
          AND attempt_count < max_attempts
        ",
        lease_secs
    ))
    .bind(worker_id)
    .bind(tenant_id)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 1 {
        let attempt_no: u32 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT attempt_count FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(tenant_id)
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_attempts
               (id, tenant_id, task_id, attempt_no, trigger_type, status, worker_id, started_at)
             VALUES (?, ?, ?, ?, ?, 'running', ?, CURRENT_TIMESTAMP)
             ON CONFLICT DO UPDATE SET status = 'running', worker_id = excluded.worker_id,
               started_at = COALESCE(started_at, excluded.started_at), completed_at = NULL,
               error_code = NULL, error_message = NULL",
        )
        .bind(format!("agtattempt-{}", uuid::Uuid::new_v4()))
        .bind(tenant_id)
        .bind(task_id)
        .bind(attempt_no)
        .bind(if attempt_no <= 1 {
            "initial"
        } else {
            "lease_recovery"
        })
        .bind(worker_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        add_event(
            state,
            tenant_id,
            task_id,
            "queue.claimed",
            Some(PHASE_INTAKE),
            Some("claimed"),
            "info",
            "Agent task 已被 worker claim",
            Some(json!({ "workerId": worker_id, "leaseSecs": lease_secs })),
        )
        .await?;
        Ok(true)
    } else {
        tx.rollback().await?;
        Ok(false)
    }
}

#[cfg_attr(not(feature = "rd"), allow(dead_code))]
pub async fn acquire_agent_task_execution_lease(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let lease_secs = lease_secs.clamp(30, 86_400);
    let result = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        UPDATE agent_tasks
        SET status = 'running',
            queue_status = 'running',
            attempt_count = CASE
                WHEN claimed_by IS NULL OR claimed_by <> ? THEN attempt_count + 1
                ELSE attempt_count
            END,
            claimed_by = ?,
            claimed_at = CURRENT_TIMESTAMP,
            lease_expires_at = datetime(CURRENT_TIMESTAMP, '+{} seconds'),
            last_error = NULL,
            last_heartbeat_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ?
          AND id = ?
          AND status NOT IN ('completed','failed','cancelled','timed_out','stale')
          AND (
            claimed_by IS NULL
            OR claimed_by = ?
            OR lease_expires_at IS NULL
            OR lease_expires_at < CURRENT_TIMESTAMP
          )
        ",
        lease_secs
    ))
    .bind(worker_id)
    .bind(worker_id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(worker_id)
    .execute(state.control_db())
    .await?;
    if result.rows_affected() == 1 {
        add_event(
            state,
            tenant_id,
            task_id,
            "queue.execution_lease_acquired",
            Some(PHASE_EXECUTING),
            Some(STATUS_RUNNING),
            "info",
            "Agent task 执行 lease 已获取",
            Some(json!({ "workerId": worker_id, "leaseSecs": lease_secs })),
        )
        .await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg_attr(not(feature = "rd"), allow(dead_code))]
pub async fn heartbeat_agent_task_queue(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let lease_secs = lease_secs.clamp(30, 86_400);
    let result = sqlx::query::<sqlx::Sqlite>(&format!(
        r"
        UPDATE agent_tasks
        SET lease_expires_at = datetime(CURRENT_TIMESTAMP, '+{} seconds'),
            last_heartbeat_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ?
          AND id = ?
          AND claimed_by = ?
          AND queue_status IN ('claimed','running','waiting_input','cancelling')
        ",
        lease_secs
    ))
    .bind(tenant_id)
    .bind(task_id)
    .bind(worker_id)
    .execute(state.control_db())
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg_attr(not(feature = "rd"), allow(dead_code))]
pub async fn heartbeat_agent_task_for_linked_resource(
    state: &AppState,
    tenant_id: &str,
    resource_type: &str,
    resource_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id
        FROM agent_tasks
        WHERE tenant_id = ?
          AND linked_resource_type = ?
          AND linked_resource_id = ?
        ORDER BY updated_at DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(resource_type)
    .bind(resource_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let task_id: String = row.get("id");
    heartbeat_agent_task_queue(state, tenant_id, &task_id, worker_id, lease_secs).await
}

async fn watchdog_ask(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<WatchDogAskRequest>,
) -> Result<Json<Value>> {
    match watchdog_ask_inner(&state, &claims, req).await {
        Ok(value) => Ok(value),
        Err(error) => {
            log_agent_ops_error("watchdog_ask", &claims.tenant_id, &error);
            Err(error)
        }
    }
}

async fn watchdog_ask_inner(
    state: &AppState,
    claims: &Claims,
    req: WatchDogAskRequest,
) -> Result<Json<Value>> {
    require_watchdog_permission(&state, &claims, "watchdog:admin").await?;
    let question = req.question.trim().to_string();
    if question.is_empty() {
        return Err(AppError::ValidationError(
            "question is required".to_string(),
        ));
    }
    let allowed_actions: Vec<&'static str> =
        if has_watchdog_permission(state, claims, "watchdog:write").await? {
            vec!["open_task", "cancel_task", "retry_task"]
        } else {
            vec!["open_task"]
        };
    let answer = ask_watchdog_text_with_actions_and_audit(
        state,
        &claims.tenant_id,
        &question,
        req.scope.as_deref(),
        req.external_platform.as_deref(),
        req.external_channel_id.as_deref(),
        req.external_conversation_id.as_deref(),
        &allowed_actions,
        Some(WatchDogActionAudit::webui(&claims.sub)),
    )
    .await?;
    Ok(Json(json!({
        "answer": answer,
        "async": false,
        "asyncModeIgnored": req.async_mode
    })))
}

async fn query_watchdog_tasks(
    state: &AppState,
    tenant_id: &str,
    external_platform: Option<&str>,
    external_channel_id: Option<&str>,
    external_conversation_id: Option<&str>,
    capability_key: Option<&str>,
    stale_minutes: Option<i64>,
    status_filter: Option<&[&str]>,
    limit: i64,
) -> Result<Vec<AgentTaskInfo>> {
    let mut sql =
        format!("SELECT {AGENT_TASK_SELECT} FROM agent_tasks at {AGENT_TASK_RUNTIME_JOIN} WHERE at.tenant_id = ?");
    if external_platform.is_some() {
        sql.push_str(" AND at.external_platform = ?");
    }
    if external_channel_id.is_some() {
        sql.push_str(" AND at.external_channel_id = ?");
    }
    if external_conversation_id.is_some() {
        sql.push_str(" AND at.external_conversation_id = ?");
    }
    if capability_key.is_some() {
        sql.push_str(" AND at.capability_key = ?");
    }
    if stale_minutes.is_some() {
        sql.push_str(" AND (at.status = 'stale' OR (at.status IN ('queued','claimed','running','waiting_input','retrying','cancelling') AND COALESCE(at.last_heartbeat_at, at.updated_at) < datetime(CURRENT_TIMESTAMP, printf('-%d minutes', ?))))");
    }
    if let Some(statuses) = status_filter.filter(|items| !items.is_empty()) {
        sql.push_str(" AND at.status IN (");
        sql.push_str(
            &std::iter::repeat_n("?", statuses.len())
                .collect::<Vec<_>>()
                .join(","),
        );
        sql.push(')');
    }
    sql.push_str(" ORDER BY at.updated_at DESC, at.created_at DESC LIMIT ?");
    let mut query = sqlx::query::<sqlx::Sqlite>(&sql).bind(tenant_id);
    if let Some(value) = external_platform {
        query = query.bind(value);
    }
    if let Some(value) = external_channel_id {
        query = query.bind(value);
    }
    if let Some(value) = external_conversation_id {
        query = query.bind(value);
    }
    if let Some(value) = capability_key {
        query = query.bind(value);
    }
    if let Some(value) = stale_minutes {
        query = query.bind(value);
    }
    if let Some(statuses) = status_filter {
        for status in statuses {
            query = query.bind(*status);
        }
    }
    let rows = query.bind(limit).fetch_all(state.control_db()).await?;
    Ok(rows.into_iter().map(task_from_row).collect())
}

fn task_filter_sql(query: &TaskListQuery) -> String {
    let mut out = String::new();
    if query.attention_only.unwrap_or(false) {
        out.push_str(
            " AND (at.status IN ('waiting_input','waiting_approval','blocked','stale') \
             OR (at.status IN ('queued','claimed','running','retrying','cancelling') \
                 AND COALESCE(at.last_heartbeat_at, at.updated_at) < datetime(CURRENT_TIMESTAMP, '-10 minutes')) \
             OR (at.queue_status IN ('claimed','running','cancelling') \
                 AND at.lease_expires_at IS NOT NULL AND at.lease_expires_at < CURRENT_TIMESTAMP))",
        );
    }
    if query
        .status
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.status = ?");
    }
    if query
        .capability_key
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.capability_key = ?");
    }
    if query
        .source
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.source = ?");
    }
    if query
        .external_conversation_id
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.external_conversation_id = ?");
    }
    if query
        .linked_resource_type
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.linked_resource_type = ?");
    }
    if query
        .linked_resource_id
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        out.push_str(" AND at.linked_resource_id = ?");
    }
    out
}

fn task_from_row(row: sqlx::sqlite::SqliteRow) -> AgentTaskInfo {
    AgentTaskInfo {
        id: row.get("id"),
        short_code: row.get("short_code"),
        tenant_id: row.get("tenant_id"),
        source: row.get("source"),
        source_ref: row.get("source_ref"),
        source_label: row.get("source_label"),
        capability_key: row.get("capability_key"),
        agent_id: row.get("agent_id"),
        agent_name: row.get("agent_name"),
        status: row.get("status"),
        phase: row.get("phase"),
        progress_percent: row.get("progress_percent"),
        title: row.get("title"),
        summary: row.get("summary"),
        owner_user_id: row.get("owner_user_id"),
        initiator_user_id: row.get("initiator_user_id"),
        visibility_scope: row.get("visibility_scope"),
        team_id: row.get("team_id"),
        correlation_id: row.get("correlation_id"),
        parent_task_id: row.get("parent_task_id"),
        root_task_id: row.get("root_task_id"),
        origin_session_id: row.get("origin_session_id"),
        origin_turn_id: row.get("origin_turn_id"),
        external_platform: row.get("external_platform"),
        external_channel_id: row.get("external_channel_id"),
        external_conversation_id: row.get("external_conversation_id"),
        external_message_id: row.get("external_message_id"),
        linked_resource_type: row.get("linked_resource_type"),
        linked_resource_id: row.get("linked_resource_id"),
        input_json: parse_json_opt(row.get("input_json")),
        output_json: parse_json_opt(row.get("output_json")),
        state_version: row.get("state_version"),
        desired_state: row.get("desired_state"),
        progress_json: parse_json_opt(row.get("progress_json")),
        result_summary: row.get("result_summary"),
        result_artifact_ref: row.get("result_artifact_ref"),
        sensitivity_label: row.get("sensitivity_label"),
        archived: row.get("archived"),
        runtime_session: runtime_summary_from_task_row(&row),
        queue: queue_info_from_task_row(&row),
        error_code: row.get("error_code"),
        error_message: row.get("error_message"),
        last_event: row.get("last_event"),
        last_heartbeat_at: row_datetime_opt(&row, "last_heartbeat_at"),
        started_at: row_datetime_opt(&row, "started_at"),
        completed_at: row_datetime_opt(&row, "completed_at"),
        created_at: row_datetime(&row, "created_at"),
        updated_at: row_datetime(&row, "updated_at"),
    }
}

fn queue_info_from_task_row(row: &sqlx::sqlite::SqliteRow) -> AgentTaskQueueInfo {
    AgentTaskQueueInfo {
        status: row
            .try_get::<Option<String>, _>("queue_status")
            .ok()
            .flatten()
            .unwrap_or_else(|| "none".to_string()),
        available_at: row_datetime_opt(row, "available_at"),
        claimed_by: row.try_get("claimed_by").ok().flatten(),
        claimed_at: row_datetime_opt(row, "claimed_at"),
        lease_expires_at: row_datetime_opt(row, "lease_expires_at"),
        attempt_count: row
            .try_get::<Option<i32>, _>("attempt_count")
            .ok()
            .flatten()
            .unwrap_or(0),
        max_attempts: row
            .try_get::<Option<i32>, _>("max_attempts")
            .ok()
            .flatten()
            .unwrap_or(3),
        idempotency_key: row.try_get("idempotency_key").ok().flatten(),
        priority: row
            .try_get::<Option<i32>, _>("priority")
            .ok()
            .flatten()
            .unwrap_or(100),
        last_error: row.try_get("last_error").ok().flatten(),
        finished_at: row_datetime_opt(row, "finished_at"),
        dead_reason: row.try_get("dead_reason").ok().flatten(),
    }
}

fn runtime_summary_from_task_row(row: &sqlx::sqlite::SqliteRow) -> Option<AgentTaskRuntimeSummary> {
    let id: Option<String> = row.try_get("runtime_session_id").ok().flatten();
    id.map(|id| AgentTaskRuntimeSummary {
        id,
        status: row
            .try_get::<Option<String>, _>("runtime_status")
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string()),
        workspace_root: row
            .try_get::<Option<String>, _>("runtime_workspace_root")
            .ok()
            .flatten()
            .unwrap_or_default(),
        isolation_mode: row
            .try_get::<Option<String>, _>("runtime_isolation_mode")
            .ok()
            .flatten()
            .unwrap_or_else(|| "local_process".to_string()),
        cancel_requested: row
            .try_get::<Option<bool>, _>("runtime_cancel_requested")
            .ok()
            .flatten()
            .unwrap_or(false),
        heartbeat_at: row_datetime_opt(row, "runtime_heartbeat_at"),
        current_command: row
            .try_get::<Option<String>, _>("runtime_current_command")
            .ok()
            .flatten(),
        current_process_status: row
            .try_get::<Option<String>, _>("runtime_current_process_status")
            .ok()
            .flatten(),
    })
}

fn event_from_row(row: sqlx::sqlite::SqliteRow) -> AgentTaskEventInfo {
    AgentTaskEventInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        task_id: row.get("task_id"),
        event_type: row.get("event_type"),
        phase: row.get("phase"),
        status: row.get("status"),
        severity: row.get("severity"),
        message: row.get("message"),
        metadata_json: parse_json_opt(row.get("metadata_json")),
        created_at: row_datetime(&row, "created_at"),
    }
}

fn trace_event_from_row(row: sqlx::sqlite::SqliteRow) -> AgentTraceEventInfo {
    AgentTraceEventInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        task_id: row.get("task_id"),
        event_type: row.get("event_type"),
        phase: row.get("phase"),
        status: row.get("status"),
        severity: row.get("severity"),
        message: row.get("message"),
        metadata_json: parse_json_opt(row.get("metadata_json")),
        artifact_id: row.get("artifact_id"),
        runtime_session_id: row.get("runtime_session_id"),
        runtime_process_id: row.get("runtime_process_id"),
        token_input: row.try_get::<Option<u64>, _>("token_input").ok().flatten(),
        token_output: row.try_get::<Option<u64>, _>("token_output").ok().flatten(),
        cost_usd: row.get("cost_usd"),
        duration_ms: row.try_get::<Option<u64>, _>("duration_ms").ok().flatten(),
        created_at: row_datetime(&row, "created_at"),
    }
}

fn json_to_string(value: Option<&Value>) -> Result<Option<String>> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(AppError::Json)
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn row_datetime(row: &sqlx::sqlite::SqliteRow, name: &str) -> String {
    row_datetime_opt(row, name).unwrap_or_default()
}

fn row_datetime_opt(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<String> {
    if let Ok(value) = row.try_get::<Option<NaiveDateTime>, _>(name) {
        return value.map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc).to_rfc3339());
    }
    row.try_get::<Option<String>, _>(name).ok().flatten()
}

fn trim_to(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn capability_display(key: &str) -> &'static str {
    match key {
        "ai_chat" => "AI 对话",
        "super_adversarial" => "超级对抗",
        "watchdog" => "看门狗",
        "pm_assistant" => "产运助手",
        "rd_agent" => "RD Agent",
        "nl2sql" => "数据探索",
        "aos_router" => "智能分发",
        "generic_ai" => "通用 AI",
        _ => "Agent",
    }
}

fn elapsed_label(started_at: Option<&String>) -> String {
    let Some(raw) = started_at else {
        return "未知".to_string();
    };
    let parsed = DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .ok();
    let Some(started) = parsed else {
        return "未知".to_string();
    };
    let secs = Utc::now()
        .signed_duration_since(started)
        .num_seconds()
        .max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn sqlite_concurrent_workers_cannot_claim_the_same_task_twice() {
        let (db, database_path) = crate::test_sqlite_file_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, status, queue_status, title)
             VALUES (?, ?, ?, 'test', 'generic_ai', 'queued', 'queued', 'claim race fixture')",
        )
        .bind(&task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &task_id,
        ))
        .bind(&tenant_id)
        .execute(state.control_db())
        .await
        .expect("insert queued claim fixture");

        let (worker_a, worker_b) = tokio::join!(
            claim_agent_task(&state, &tenant_id, &task_id, "worker-a", 60),
            claim_agent_task(&state, &tenant_id, &task_id, "worker-b", 60),
        );
        let claimed_count = usize::from(worker_a.expect("worker A claim"))
            + usize::from(worker_b.expect("worker B claim"));
        assert_eq!(claimed_count, 1);

        let row = sqlx::query::<sqlx::Sqlite>(
            "SELECT claimed_by, attempt_count FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load claimed task");
        assert!(matches!(
            row.get::<String, _>("claimed_by").as_str(),
            "worker-a" | "worker-b"
        ));
        assert_eq!(row.get::<i64, _>("attempt_count"), 1);
        let attempt_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM agent_task_attempts WHERE tenant_id = ? AND task_id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("count task attempts");
        assert_eq!(attempt_count, 1);

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks
             SET lease_expires_at = datetime(CURRENT_TIMESTAMP, '-1 second')
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(state.control_db())
        .await
        .expect("expire claimed task lease");
        recover_agent_task_queue(&state, &tenant_id, 30)
            .await
            .expect("recover expired task lease");
        let recovered_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT queue_status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load recovered task");
        let attempt_error: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT error_code FROM agent_task_attempts
             WHERE tenant_id = ? AND task_id = ? AND attempt_no = 1",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load recovered task attempt");
        assert_eq!(recovered_status, "queued");
        assert_eq!(attempt_error, "lease_lost");

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
    async fn sqlite_concurrent_resource_projections_serialize_without_busy_errors() {
        let (db, database_path) = crate::test_sqlite_file_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let task_ids = (0..16)
            .map(|index| format!("agt-projection-{index}-{}", uuid::Uuid::new_v4()))
            .collect::<Vec<_>>();
        for task_id in &task_ids {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, source, capability_key, status,
                    phase, progress_percent, queue_status, title)
                 VALUES (?, ?, ?, 'test', 'generic_ai', 'created',
                         'intake', 0, 'queued', 'projection race fixture')",
            )
            .bind(task_id)
            .bind(crate::routes::task_control::short_code_for_task_id(task_id))
            .bind(&tenant_id)
            .execute(state.control_db())
            .await
            .expect("insert projection fixture");
        }

        let outcomes = futures_util::future::join_all(task_ids.iter().map(|task_id| {
            sync_agent_task_from_resource(
                &state,
                &tenant_id,
                task_id,
                STATUS_RUNNING,
                PHASE_EXECUTING,
                50,
                "projection running",
                None,
                false,
            )
        }))
        .await;
        for outcome in outcomes {
            outcome.expect("serialize concurrent SQLite projection");
        }

        let projected = sqlx::query_scalar::<sqlx::Sqlite, i64>(
            "SELECT COUNT(*) FROM agent_tasks
             WHERE tenant_id = ? AND status = 'running' AND progress_percent = 50",
        )
        .bind(&tenant_id)
        .fetch_one(state.control_db())
        .await
        .expect("count projected tasks");
        assert_eq!(projected, 16);

        state.db.close().await;
        for path in [
            database_path.clone(),
            database_path.with_extension("db-wal"),
            database_path.with_extension("db-shm"),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn watchdog_action_permission_defaults_to_read_only() {
        assert!(watchdog_action_allowed(
            WatchDogActionKind::Detail,
            &["open_task"]
        ));
        assert!(!watchdog_action_allowed(
            WatchDogActionKind::Cancel,
            &["open_task"]
        ));
        assert!(!watchdog_action_allowed(
            WatchDogActionKind::Retry,
            &["open_task"]
        ));
    }

    #[test]
    fn watchdog_action_permission_accepts_configured_write_actions() {
        let allowed = ["open_task", "cancel_task", "retry_task"];
        assert!(watchdog_action_allowed(
            WatchDogActionKind::Cancel,
            &allowed
        ));
        assert!(watchdog_action_allowed(WatchDogActionKind::Retry, &allowed));
    }

    #[test]
    fn command_center_permission_grants_agent_ops_read_and_role_scoped_write() {
        let command_center = ["tasks:read".to_string()];
        assert!(menu_permissions_grant_watchdog(
            "viewer",
            &command_center,
            "watchdog:read"
        ));
        assert!(!menu_permissions_grant_watchdog(
            "viewer",
            &command_center,
            "watchdog:write"
        ));
        assert!(menu_permissions_grant_watchdog(
            "developer",
            &command_center,
            "watchdog:write"
        ));
        assert!(!menu_permissions_grant_watchdog(
            "developer",
            &[],
            "watchdog:read"
        ));
    }

    #[test]
    fn watchdog_action_parser_extracts_action_and_index() {
        let detail = parse_watchdog_action("详情 2").expect("detail action");
        assert!(matches!(detail.kind, WatchDogActionKind::Detail));
        assert_eq!(detail.index, 2);

        let cancel = parse_watchdog_action("cancel 12").expect("cancel action");
        assert!(matches!(cancel.kind, WatchDogActionKind::Cancel));
        assert_eq!(cancel.index, 12);
    }

    #[test]
    fn task_filter_sql_supports_linked_resource_filters() {
        let sql = task_filter_sql(&TaskListQuery {
            capability_key: Some("rd_agent".to_string()),
            linked_resource_type: Some("rd_task".to_string()),
            linked_resource_id: Some("rd-1".to_string()),
            ..TaskListQuery::default()
        });
        assert!(sql.contains("capability_key = ?"));
        assert!(sql.contains("linked_resource_type = ?"));
        assert!(sql.contains("linked_resource_id = ?"));
    }

    #[test]
    fn task_filter_sql_attention_scope_excludes_terminal_history() {
        let sql = task_filter_sql(&TaskListQuery {
            attention_only: Some(true),
            ..TaskListQuery::default()
        });
        assert!(sql.contains("waiting_input"));
        assert!(sql.contains("waiting_approval"));
        assert!(sql.contains("lease_expires_at"));
        assert!(!sql.contains("'failed'"));
        assert!(!sql.contains("'timed_out'"));
    }

    #[test]
    fn rd_approval_wait_maps_to_the_canonical_approval_state() {
        assert_eq!(
            rd_status_to_agent_status("waiting_approval"),
            "waiting_approval"
        );
        assert_ne!(
            rd_status_to_agent_status("waiting_approval"),
            STATUS_WAITING_INPUT
        );
    }

    #[test]
    fn super_assistant_subtasks_map_to_canonical_child_states() {
        assert_eq!(
            super_assistant_subtask_projection("queued"),
            ("queued", PHASE_INTAKE, 5, false)
        );
        assert_eq!(
            super_assistant_subtask_projection("running"),
            (STATUS_RUNNING, PHASE_EXECUTING, 55, false)
        );
        assert_eq!(
            super_assistant_subtask_projection("completed"),
            (STATUS_COMPLETED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(
            super_assistant_subtask_projection("failed"),
            (STATUS_FAILED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(super_assistant_subtask_label("deep_research"), "深度研究");
        assert_eq!(super_assistant_subtask_label("nl2sql"), "NL2SQL");
        assert_eq!(
            super_assistant_subtask_label("data_attribution"),
            "数据归因"
        );
        assert_eq!(
            super_assistant_subtask_label("super_adversarial"),
            "超级对抗"
        );
    }

    #[test]
    fn material_jobs_map_to_canonical_states() {
        assert_eq!(
            pm_material_job_projection("queued"),
            ("queued", PHASE_INTAKE, 5, false)
        );
        assert_eq!(
            pm_material_job_projection("running"),
            (STATUS_RUNNING, PHASE_MODEL_CALLING, 50, false)
        );
        assert_eq!(
            pm_material_job_projection("cancelling"),
            (STATUS_CANCELLING, PHASE_FINALIZING, 95, false)
        );
        assert_eq!(
            pm_material_job_projection("cancelled"),
            (STATUS_CANCELLED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(
            pm_material_job_projection("completed"),
            (STATUS_COMPLETED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(
            pm_material_job_projection("failed"),
            (STATUS_FAILED, PHASE_FINALIZING, 100, true)
        );
    }

    #[test]
    fn linked_resource_cancellation_fails_closed() {
        assert!(linked_resource_cancel_supported("pm_material_job"));
        assert!(linked_resource_cancel_supported("bot_inbound_message"));
        assert!(linked_resource_cancel_supported("super_assistant_turn"));
        assert!(linked_resource_cancel_supported(
            "super_assistant_subtask:deep_research"
        ));
        assert!(linked_resource_cancel_supported(
            "super_assistant_subtask:nl2sql"
        ));
        assert!(!linked_resource_cancel_supported(
            "super_assistant_subtask:unknown"
        ));
        assert!(!linked_resource_cancel_supported("nl2sql_agent_query"));
        assert!(!linked_resource_cancel_supported("unknown_executor"));
        assert!(!linked_resource_cancel_supported("demo_scenario"));
        assert!(ensure_linked_resource_cancel_supported("unknown_executor").is_err());
    }

    #[test]
    fn bot_inbound_queue_maps_to_canonical_states() {
        assert_eq!(
            bot_inbound_message_projection("queued", "queued"),
            ("queued", PHASE_INTAKE, 5, false)
        );
        assert_eq!(
            bot_inbound_message_projection("claimed", "processing"),
            (STATUS_RUNNING, PHASE_INTAKE, 10, false)
        );
        assert_eq!(
            bot_inbound_message_projection("cancelling", "cancelling"),
            (STATUS_CANCELLING, PHASE_FINALIZING, 95, false)
        );
        assert_eq!(
            bot_inbound_message_projection("cancelled", "cancelled"),
            (STATUS_CANCELLED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(
            bot_inbound_message_projection("dead", "failed"),
            (STATUS_FAILED, PHASE_FINALIZING, 100, true)
        );
        assert_eq!(
            bot_inbound_message_projection("succeeded", "replied"),
            (STATUS_COMPLETED, PHASE_FINALIZING, 100, true)
        );
    }

    #[tokio::test]
    async fn sqlite_claimed_bot_queue_cancellation_waits_for_worker_confirmation() {
        let db = crate::test_sqlite_pool().await;
        let state = crate::state::AppState {
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
        };
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let owner_user_id = format!("user-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let inbound_log_id = uuid::Uuid::new_v4().to_string();

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO bot_message_logs
               (id, tenant_id, direction, platform, message_type, status, queue_status,
                claimed_by, claimed_at, content_json)
             VALUES (?, ?, 'inbound', 'webhook', 'text', 'processing', 'claimed',
                     'test-worker', CURRENT_TIMESTAMP, JSON_OBJECT('agentTaskId', ?))",
        )
        .bind(&inbound_log_id)
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(state.control_db())
        .await
        .expect("insert claimed Bot queue fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, source_ref, capability_key, title,
                status, phase, progress_percent, owner_user_id, initiator_user_id,
                root_task_id, linked_resource_type, linked_resource_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'bot', ?, 'generic_ai', 'Bot cancellation fixture',
                     'running', 'executing', 50, ?, ?, ?, 'bot_inbound_message', ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &task_id,
        ))
        .bind(&tenant_id)
        .bind(&inbound_log_id)
        .bind(&owner_user_id)
        .bind(&owner_user_id)
        .bind(&task_id)
        .bind(&inbound_log_id)
        .execute(state.control_db())
        .await
        .expect("insert Bot AgentOps fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_resource_links
               (id, tenant_id, task_id, root_task_id, resource_type, resource_id, relation_type)
             VALUES (?, ?, ?, ?, 'bot_inbound_message', ?, 'primary')",
        )
        .bind(format!("agtres-{}", uuid::Uuid::new_v4()))
        .bind(&tenant_id)
        .bind(&task_id)
        .bind(&task_id)
        .bind(&inbound_log_id)
        .execute(state.control_db())
        .await
        .expect("link Bot queue fixture");

        cancel_agent_task_by_id(
            &state,
            &tenant_id,
            &task_id,
            &WatchDogActionAudit::webui(&owner_user_id),
        )
        .await
        .expect("request Bot cancellation");
        let queue_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT queue_status FROM bot_message_logs WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&inbound_log_id)
        .fetch_one(state.control_db())
        .await
        .expect("load cancelling Bot queue");
        let task_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load cancelling AgentOps task");
        assert_eq!(queue_status, "cancelling");
        assert_eq!(task_status, STATUS_CANCELLING);

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE bot_message_logs
             SET queue_status = 'cancelled', status = 'cancelled', finished_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&inbound_log_id)
        .execute(state.control_db())
        .await
        .expect("confirm Bot worker stopped");
        sync_linked_resource_status(&state, &tenant_id, &task_id)
            .await
            .expect("project confirmed cancellation");
        let final_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load cancelled AgentOps task");
        assert_eq!(final_status, STATUS_CANCELLED);

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&task_id)
            .execute(state.control_db())
            .await
            .expect("clean AgentOps fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM bot_message_logs WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&inbound_log_id)
            .execute(state.control_db())
            .await
            .expect("clean Bot queue fixture");
    }

    #[tokio::test]
    async fn sqlite_super_assistant_subtask_projection_is_idempotent() {
        let db = crate::test_sqlite_pool().await;
        let state = crate::state::AppState {
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
        };
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let owner_user_id = format!("user-{}", uuid::Uuid::new_v4());
        let parent_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let turn_id = format!("turn-{}", uuid::Uuid::new_v4());
        let subtask_id = format!("subtask-{}", uuid::Uuid::new_v4());
        let artifact_ref = format!("/generated/{subtask_id}.json");

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, capability_key, title, owner_user_id,
                initiator_user_id, root_task_id, origin_session_id, origin_turn_id,
                last_heartbeat_at)
             VALUES (?, ?, ?, 'super_assistant', 'parent turn', ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&parent_task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &parent_task_id,
        ))
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .bind(&owner_user_id)
        .bind(&parent_task_id)
        .bind(&session_id)
        .bind(&turn_id)
        .execute(state.control_db())
        .await
        .expect("insert parent AgentOps task");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO super_assistant_turns
               (tenant_id, user_id, session_id, turn_id, status, route_capability,
                user_message, final_text)
             VALUES (?, ?, ?, ?, 'completed', 'deep_research', 'research this', 'done')",
        )
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .bind(&session_id)
        .bind(&turn_id)
        .execute(state.control_db())
        .await
        .expect("insert completed Super Assistant turn");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO super_assistant_subtasks
               (id, tenant_id, user_id, parent_turn_id, tool_call_id, engine,
                external_task_id, status, artifact_ref, permission_snapshot_json)
             VALUES (?, ?, ?, ?, ?, 'deep_research', ?, 'completed', ?, JSON_OBJECT())",
        )
        .bind(&subtask_id)
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .bind(&turn_id)
        .bind(format!("call-{subtask_id}"))
        .bind(format!("pm-{subtask_id}"))
        .bind(&artifact_ref)
        .execute(state.control_db())
        .await
        .expect("insert completed specialist subtask");

        sync_super_assistant_turn_status(&state, &tenant_id, &parent_task_id, &turn_id)
            .await
            .expect("project specialist child task");
        sync_super_assistant_turn_status(&state, &tenant_id, &parent_task_id, &turn_id)
            .await
            .expect("replay specialist projection idempotently");

        let children = sqlx::query::<sqlx::Sqlite>(
            "SELECT id, owner_user_id, parent_task_id, root_task_id, status
             FROM agent_tasks
             WHERE tenant_id = ? AND source = 'super_assistant' AND source_ref = ?",
        )
        .bind(&tenant_id)
        .bind(&subtask_id)
        .fetch_all(state.control_db())
        .await
        .expect("load projected child task");
        assert_eq!(children.len(), 1);
        let child_task_id = children[0].get::<String, _>("id");
        assert_eq!(
            children[0]
                .get::<Option<String>, _>("owner_user_id")
                .as_deref(),
            Some(owner_user_id.as_str())
        );
        assert_eq!(
            children[0]
                .get::<Option<String>, _>("parent_task_id")
                .as_deref(),
            Some(parent_task_id.as_str())
        );
        assert_eq!(
            children[0]
                .get::<Option<String>, _>("root_task_id")
                .as_deref(),
            Some(parent_task_id.as_str())
        );
        assert_eq!(children[0].get::<String, _>("status"), STATUS_COMPLETED);

        let reuse_event_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_events
             WHERE tenant_id = ? AND task_id = ? AND event_type = 'task.idempotency_reused'",
        )
        .bind(&tenant_id)
        .bind(&child_task_id)
        .fetch_one(state.control_db())
        .await
        .expect("count child idempotency reuse events");
        assert_eq!(reuse_event_count, 0);

        let artifact_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_artifacts
             WHERE tenant_id = ? AND task_id = ? AND owner_user_id = ? AND artifact_ref = ?",
        )
        .bind(&tenant_id)
        .bind(&child_task_id)
        .bind(&owner_user_id)
        .bind(&artifact_ref)
        .fetch_one(state.control_db())
        .await
        .expect("count child artifacts");
        assert_eq!(artifact_count, 1);

        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM super_assistant_subtasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&subtask_id)
        .execute(state.control_db())
        .await
        .expect("delete specialist fixture");
        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM super_assistant_turns WHERE tenant_id = ? AND turn_id = ?",
        )
        .bind(&tenant_id)
        .bind(&turn_id)
        .execute(state.control_db())
        .await
        .expect("delete turn fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id IN (?, ?)")
            .bind(&tenant_id)
            .bind(&child_task_id)
            .bind(&parent_task_id)
            .execute(state.control_db())
            .await
            .expect("delete AgentOps fixtures");
        state.db.close().await;
    }

    #[cfg(feature = "nl2sql")]
    #[tokio::test]
    async fn sqlite_nl2sql_workers_converge_and_attribution_cancels_durably() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let owner_user_id = uuid::Uuid::new_v4().to_string();
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO users
               (id, email, name, password_hash, role, tenant_id, is_active,
                menu_permissions_json)
             VALUES (?, ?, 'NL2SQL WatchDog Owner', 'not-used', 'developer', ?, 1,
                     JSON_ARRAY('tasks:read','tasks:control'))",
        )
        .bind(&owner_user_id)
        .bind(format!(
            "nl2sql-watchdog-{}@example.invalid",
            uuid::Uuid::new_v4()
        ))
        .bind(&tenant_id)
        .execute(state.control_db())
        .await
        .expect("insert NL2SQL owner fixture");

        let async_resource_id = format!("nl2sql-task-{}", uuid::Uuid::new_v4());
        let async_task = create_task_with_outcome(
            &state,
            CreateAgentTaskInput {
                tenant_id: tenant_id.clone(),
                source: "nl2sql_async".to_string(),
                source_ref: Some(async_resource_id.clone()),
                source_label: Some("NL2SQL async recovery test".to_string()),
                capability_key: "nl2sql".to_string(),
                agent_id: None,
                agent_name: None,
                title: "lost async worker".to_string(),
                summary: None,
                owner_user_id: Some(owner_user_id.clone()),
                correlation_id: Some(async_resource_id.clone()),
                parent_task_id: None,
                external_platform: None,
                external_channel_id: None,
                external_conversation_id: None,
                external_message_id: None,
                idempotency_key: Some(format!("test:{async_resource_id}")),
                input_json: None,
            },
        )
        .await
        .expect("create async AgentOps fixture");
        link_task_resource(
            &state,
            &tenant_id,
            &async_task.id,
            "nl2sql_async_query",
            &async_resource_id,
        )
        .await
        .expect("link async AgentOps fixture");
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks
             SET status = 'running', phase = 'executing',
                 created_at = datetime(CURRENT_TIMESTAMP, '-30 seconds'),
                 last_heartbeat_at = datetime(CURRENT_TIMESTAMP, '-30 seconds')
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&async_task.id)
        .execute(state.control_db())
        .await
        .expect("age async worker fixture");
        sync_linked_resource_status(&state, &tenant_id, &async_task.id)
            .await
            .expect("reconcile lost async worker");
        let async_failure = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, error_code FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&async_task.id)
        .fetch_one(state.control_db())
        .await
        .expect("load async worker recovery result");
        assert_eq!(async_failure.get::<String, _>("status"), STATUS_FAILED);
        assert_eq!(
            async_failure
                .get::<Option<String>, _>("error_code")
                .as_deref(),
            Some("nl2sql_async_worker_lost")
        );

        let attribution_cancel_column: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT COUNT(*) FROM pragma_table_info('nl2sql_attribution_tasks')
             WHERE name = 'cancel_requested'",
        )
        .fetch_one(state.control_db())
        .await
        .expect("inspect attribution cancellation schema");
        assert_eq!(attribution_cancel_column, 1);

        let attribution_resource_id = format!("nl2sql-attribution-task-{}", uuid::Uuid::new_v4());
        let attribution_task = create_task_with_outcome(
            &state,
            CreateAgentTaskInput {
                tenant_id: tenant_id.clone(),
                source: "nl2sql_attribution".to_string(),
                source_ref: Some(attribution_resource_id.clone()),
                source_label: Some("Data attribution recovery test".to_string()),
                capability_key: "data_attribution".to_string(),
                agent_id: None,
                agent_name: None,
                title: "durable attribution cancellation".to_string(),
                summary: None,
                owner_user_id: Some(owner_user_id.clone()),
                correlation_id: Some(attribution_resource_id.clone()),
                parent_task_id: None,
                external_platform: None,
                external_channel_id: None,
                external_conversation_id: None,
                external_message_id: None,
                idempotency_key: Some(format!("test:{attribution_resource_id}")),
                input_json: None,
            },
        )
        .await
        .expect("create attribution AgentOps fixture");
        link_task_resource(
            &state,
            &tenant_id,
            &attribution_task.id,
            "nl2sql_attribution_task",
            &attribution_resource_id,
        )
        .await
        .expect("link attribution AgentOps fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_attribution_tasks
               (task_id, tenant_id, user_id, conversation_id, question, status)
             VALUES (?, ?, ?, ?, 'why did ROI change?', 'running')",
        )
        .bind(&attribution_resource_id)
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .bind(format!("conversation-{}", uuid::Uuid::new_v4()))
        .execute(state.control_db())
        .await
        .expect("insert running attribution fixture");
        sync_linked_resource_status(&state, &tenant_id, &attribution_task.id)
            .await
            .expect("project running attribution task");
        let running_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&attribution_task.id)
        .fetch_one(state.control_db())
        .await
        .expect("load running attribution projection");
        assert_eq!(running_status, STATUS_RUNNING);

        cancel_agent_task_by_id(
            &state,
            &tenant_id,
            &attribution_task.id,
            &WatchDogActionAudit::webui(&owner_user_id),
        )
        .await
        .expect("cancel attribution through its real executor");
        let attribution_source = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, cancel_requested FROM nl2sql_attribution_tasks
             WHERE tenant_id = ? AND task_id = ?",
        )
        .bind(&tenant_id)
        .bind(&attribution_resource_id)
        .fetch_one(state.control_db())
        .await
        .expect("load durable attribution cancellation");
        assert_eq!(
            attribution_source.get::<String, _>("status"),
            STATUS_CANCELLED
        );
        assert!(attribution_source.get::<bool, _>("cancel_requested"));
        let attribution_projection: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&attribution_task.id)
        .fetch_one(state.control_db())
        .await
        .expect("load cancelled attribution projection");
        assert_eq!(attribution_projection, STATUS_CANCELLED);

        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM nl2sql_attribution_tasks WHERE tenant_id = ? AND task_id = ?",
        )
        .bind(&tenant_id)
        .bind(&attribution_resource_id)
        .execute(state.control_db())
        .await
        .expect("clean attribution source fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id IN (?, ?)")
            .bind(&tenant_id)
            .bind(&async_task.id)
            .bind(&attribution_task.id)
            .execute(state.control_db())
            .await
            .expect("clean NL2SQL AgentOps fixtures");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM users WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&owner_user_id)
            .execute(state.control_db())
            .await
            .expect("clean NL2SQL owner fixture");
        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_material_projection_is_idempotent() {
        let db = crate::test_sqlite_pool().await;
        let state = crate::state::AppState {
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
        };
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let owner_user_id = format!("user-{}", uuid::Uuid::new_v4());
        let agent_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let job_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_material_jobs
               (tenant_id, prompt_text, model, asset_type, status, result_count, created_by)
             VALUES (?, 'create a launch brief', 'test-model', 'text', 'completed', 1, ?)",
        )
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .execute(state.control_db())
        .await
        .expect("insert completed material job")
        .last_insert_rowid();
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, source_ref, capability_key, title,
                owner_user_id, initiator_user_id, root_task_id, linked_resource_type,
                linked_resource_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'pm_materials', ?, 'materials', 'material generation',
                     ?, ?, ?, 'pm_material_job', ?, CURRENT_TIMESTAMP)",
        )
        .bind(&agent_task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &agent_task_id,
        ))
        .bind(&tenant_id)
        .bind(job_id.to_string())
        .bind(&owner_user_id)
        .bind(&owner_user_id)
        .bind(&agent_task_id)
        .bind(job_id.to_string())
        .execute(state.control_db())
        .await
        .expect("insert AgentOps material task");
        let asset_id = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_material_assets
               (tenant_id, job_id, asset_type, content_text, meta_json)
             VALUES (?, ?, 'text', '# Launch brief', JSON_OBJECT('model', 'test-model'))",
        )
        .bind(&tenant_id)
        .bind(job_id)
        .execute(state.control_db())
        .await
        .expect("insert material asset")
        .last_insert_rowid();

        sync_pm_material_job_status(&state, &tenant_id, &agent_task_id, &job_id.to_string())
            .await
            .expect("project material job");
        sync_pm_material_job_status(&state, &tenant_id, &agent_task_id, &job_id.to_string())
            .await
            .expect("replay material projection idempotently");

        let projected = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, result_summary, result_artifact_ref
             FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&agent_task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load projected material task");
        assert_eq!(projected.get::<String, _>("status"), STATUS_COMPLETED);
        assert!(projected
            .get::<Option<String>, _>("result_summary")
            .is_some_and(|value| value.contains("1")));
        let expected_artifact_ref = format!("pm-material-asset:{asset_id}");
        assert_eq!(
            projected
                .get::<Option<String>, _>("result_artifact_ref")
                .as_deref(),
            Some(expected_artifact_ref.as_str())
        );
        let artifact_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_artifacts
             WHERE tenant_id = ? AND task_id = ? AND owner_user_id = ?
               AND artifact_ref = ?",
        )
        .bind(&tenant_id)
        .bind(&agent_task_id)
        .bind(&owner_user_id)
        .bind(format!("pm-material-asset:{asset_id}"))
        .fetch_one(state.control_db())
        .await
        .expect("count projected material artifacts");
        assert_eq!(artifact_count, 1);

        let unsupported_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, owner_user_id,
                initiator_user_id, root_task_id, linked_resource_type, linked_resource_id,
                last_heartbeat_at)
             VALUES (?, ?, ?, 'test', 'unsupported', 'unsupported cancellation', ?, ?, ?,
                     'demo_scenario', 'demo-1', CURRENT_TIMESTAMP)",
        )
        .bind(&unsupported_task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &unsupported_task_id,
        ))
        .bind(&tenant_id)
        .bind(&owner_user_id)
        .bind(&owner_user_id)
        .bind(&unsupported_task_id)
        .execute(state.control_db())
        .await
        .expect("insert unsupported cancellation fixture");
        let cancel_error = cancel_agent_task_by_id(
            &state,
            &tenant_id,
            &unsupported_task_id,
            &WatchDogActionAudit::webui(&owner_user_id),
        )
        .await
        .expect_err("unsupported cancellation must fail closed");
        assert!(cancel_error
            .to_string()
            .contains("does not support cancellation"));
        let unsupported_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&unsupported_task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load unsupported task status");
        assert_eq!(unsupported_status, STATUS_CREATED);

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id IN (?, ?)")
            .bind(&tenant_id)
            .bind(&agent_task_id)
            .bind(&unsupported_task_id)
            .execute(state.control_db())
            .await
            .expect("delete AgentOps material fixture");
        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM pm_material_assets WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(asset_id)
        .execute(state.control_db())
        .await
        .expect("delete material asset fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM pm_material_jobs WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(job_id)
            .execute(state.control_db())
            .await
            .expect("delete material job fixture");
        state.db.close().await;
    }
}
