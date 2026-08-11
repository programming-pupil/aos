//! WatchDog v2 user-facing task control plane.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use axum::{
    extract::{Extension, Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use tokio::sync::watch;

use crate::{
    auth::Claims,
    error::{AppError, Result},
    routes::users::parse_permissions_json,
    state::AppState,
};

const DEFAULT_LIMIT: u32 = 30;
const MAX_LIMIT: u32 = 100;
const ACTIVE_STATUSES: &[&str] = &[
    "created",
    "queued",
    "claimed",
    "running",
    "waiting_input",
    "waiting_approval",
    "blocked",
    "retrying",
    "cancelling",
];
const RUNNING_STATUSES: &[&str] = &[
    "created",
    "queued",
    "claimed",
    "running",
    "retrying",
    "cancelling",
];

static TASK_OUTBOX_NOTIFIERS: OnceLock<Mutex<HashMap<String, watch::Sender<u64>>>> =
    OnceLock::new();

fn task_outbox_notifiers() -> &'static Mutex<HashMap<String, watch::Sender<u64>>> {
    TASK_OUTBOX_NOTIFIERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn subscribe_task_outbox(tenant_id: &str) -> watch::Receiver<u64> {
    let mut notifiers = task_outbox_notifiers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    notifiers.retain(|_, sender| sender.receiver_count() > 0);
    if let Some(sender) = notifiers.get(tenant_id) {
        return sender.subscribe();
    }
    let (sender, receiver) = watch::channel(0);
    notifiers.insert(tenant_id.to_string(), sender);
    receiver
}

pub(crate) fn notify_task_outbox_changed(tenant_id: &str) {
    let notifiers = task_outbox_notifiers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(sender) = notifiers.get(tenant_id) {
        sender.send_modify(|version| *version = version.wrapping_add(1));
    }
}

fn normalized_watchdog_feature_mode(feature_key: &str, value: &str) -> Result<String> {
    let mode = value.trim().to_ascii_lowercase();
    let valid = match feature_key {
        "watchdog_control_plane_v2" | "watchdog_notification_outbox" => {
            matches!(mode.as_str(), "off" | "shadow" | "on")
        }
        "watchdog_external_identity" => {
            matches!(mode.as_str(), "off" | "optional" | "required")
        }
        "watchdog_mobile_handoff" | "watchdog_watch_rules" => {
            matches!(mode.as_str(), "off" | "on")
        }
        _ => false,
    };
    if !valid {
        return Err(AppError::ValidationError(format!(
            "invalid {feature_key} feature mode: {mode}"
        )));
    }
    Ok(mode)
}

pub(crate) async fn watchdog_feature_mode(
    state: &AppState,
    tenant_id: &str,
    feature_key: &str,
    default_mode: &str,
) -> Result<String> {
    let configured = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT mode FROM tenant_agent_features
         WHERE tenant_id = ? AND feature_key = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(feature_key)
    .fetch_optional(state.control_db())
    .await?
    .unwrap_or_else(|| default_mode.to_string());
    normalized_watchdog_feature_mode(feature_key, &configured)
}

async fn require_watchdog_feature(
    state: &AppState,
    tenant_id: &str,
    feature_key: &str,
    default_mode: &str,
) -> Result<()> {
    if watchdog_feature_mode(state, tenant_id, feature_key, default_mode).await? == "on" {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn summary_membership(status: &str) -> (bool, bool, bool) {
    (
        RUNNING_STATUSES.contains(&status),
        matches!(status, "waiting_input" | "waiting_approval" | "blocked"),
        matches!(status, "failed" | "timed_out" | "stale"),
    )
}

fn event_visible_to(is_admin: bool, visibility: &str) -> bool {
    is_admin || visibility != "admin"
}

pub fn routes(state: AppState) -> Router<AppState> {
    let auth_state = state.clone();
    Router::new()
        .route("/summary", get(summary))
        .route("/stream", get(task_stream))
        .route("/deliveries", get(list_deliveries))
        .route("/deliveries/{delivery_id}/replay", post(replay_delivery))
        .route("/presence", get(get_presence).post(update_presence))
        .route(
            "/watch-rules",
            get(list_watch_rules).post(create_watch_rule),
        )
        .route("/watch-rules/pending", get(list_pending_watch_rule_actions))
        .route(
            "/watch-rules/runs/{run_id}/decision",
            post(decide_watch_rule_action),
        )
        .route("/watch-rules/{rule_id}", delete(delete_watch_rule))
        .route("/", get(list_tasks))
        .route("/{id}", get(task_detail))
        .route("/{id}/events", get(task_events))
        .route("/{id}/resources", get(task_resources))
        .route("/{id}/artifacts", get(task_artifacts))
        .route("/{id}/artifacts/{artifact_id}", get(task_artifact_content))
        .route("/{id}/attempts", get(task_attempts))
        .route("/{id}/commands", get(list_commands).post(create_command))
        .route("/{id}/share", post(share_task))
        .route(
            "/{id}/subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route(
            "/{id}/subscriptions/{subscription_id}",
            delete(delete_subscription),
        )
        .layer(axum::middleware::from_fn_with_state(
            auth_state,
            crate::auth_middleware::require_auth,
        ))
        .with_state(state)
}

pub fn identity_routes(state: AppState) -> Router<AppState> {
    let auth_state = state.clone();
    Router::new()
        .route("/", get(list_external_identities))
        .route("/pairing-codes", post(create_pairing_code))
        .route("/{id}", delete(revoke_external_identity))
        .layer(axum::middleware::from_fn_with_state(
            auth_state,
            crate::auth_middleware::require_auth,
        ))
        .with_state(state)
}

#[derive(Debug, Clone)]
struct TaskAccess {
    user_id: String,
    is_admin: bool,
    can_control: bool,
}

fn task_permissions(role: &str, permissions: &[String]) -> (bool, bool, bool) {
    let is_admin = matches!(role, "admin" | "superadmin")
        || permissions
            .iter()
            .any(|value| matches!(value.as_str(), "tasks:admin" | "watchdog:admin"));
    let has_read = is_admin
        || permissions.iter().any(|value| {
            matches!(
                value.as_str(),
                "tasks:read" | "watchdog:read" | "super_assistant:read"
            )
        });
    let can_control = has_read
        && (is_admin
            || permissions
                .iter()
                .any(|value| matches!(value.as_str(), "tasks:control" | "watchdog:write")));
    (has_read, is_admin, can_control)
}

async fn task_access(state: &AppState, claims: &Claims) -> Result<TaskAccess> {
    require_watchdog_feature(state, &claims.tenant_id, "watchdog_control_plane_v2", "on").await?;
    let row: Option<(String, Option<String>)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT role, CAST(menu_permissions_json AS TEXT) FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(state.control_db())
    .await?;
    let Some((role, raw_permissions)) = row else {
        return Err(AppError::Forbidden);
    };
    let permissions = raw_permissions
        .map(|raw| parse_permissions_json(Some(raw)))
        .unwrap_or_default();
    let (has_read, is_admin, can_control) = task_permissions(&role, &permissions);
    if !has_read {
        return Err(AppError::Forbidden);
    }
    Ok(TaskAccess {
        user_id: claims.sub.clone(),
        is_admin,
        can_control,
    })
}

fn can_manage_own_external_identity(
    role: &str,
    permissions: &[String],
    permissions_inherited: bool,
) -> bool {
    matches!(role, "admin" | "superadmin")
        || (permissions_inherited && role == "developer")
        || permissions.iter().any(|value| {
            matches!(
                value.as_str(),
                "bot_agents:read" | "tasks:read" | "watchdog:read" | "super_assistant:read"
            )
        })
}

async fn require_external_identity_access(state: &AppState, claims: &Claims) -> Result<()> {
    let row: Option<(String, Option<String>, bool)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT role, CAST(menu_permissions_json AS TEXT), (menu_permissions_json IS NULL)
         AS permissions_inherited FROM users
         WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(state.control_db())
    .await?;
    let Some((role, raw_permissions, permissions_inherited)) = row else {
        return Err(AppError::Forbidden);
    };
    let permissions = raw_permissions
        .map(|raw| parse_permissions_json(Some(raw)))
        .unwrap_or_default();
    if can_manage_own_external_identity(&role, &permissions, permissions_inherited) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

pub(crate) async fn actor_can_control_task(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    user_id: &str,
) -> Result<bool> {
    let row: Option<(String, Option<String>)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT role, CAST(menu_permissions_json AS TEXT) FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some((role, raw_permissions)) = row else {
        return Ok(false);
    };
    let permissions = raw_permissions
        .map(|raw| parse_permissions_json(Some(raw)))
        .unwrap_or_default();
    let (_, is_admin, can_control) = task_permissions(&role, &permissions);
    if !can_control {
        return Ok(false);
    }
    if is_admin {
        return Ok(true);
    }
    let allowed: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks at
         WHERE at.tenant_id = ? AND at.id = ? AND (
           at.owner_user_id = ? OR at.initiator_user_id = ? OR EXISTS (
             SELECT 1 FROM agent_task_grants grants
             WHERE grants.tenant_id = at.tenant_id
               AND grants.task_id = COALESCE(at.root_task_id, at.id)
               AND grants.grantee_type = 'user' AND grants.grantee_id = ?
               AND grants.permission IN ('control','write') AND grants.revoked_at IS NULL
           )
         )",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(user_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(allowed > 0)
}

pub(crate) async fn actor_can_read_task(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    user_id: &str,
) -> Result<bool> {
    let row: Option<(String, Option<String>)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT role, CAST(menu_permissions_json AS TEXT) FROM users
         WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some((role, raw_permissions)) = row else {
        return Ok(false);
    };
    let permissions = raw_permissions
        .map(|raw| parse_permissions_json(Some(raw)))
        .unwrap_or_default();
    let (has_read, is_admin, _) = task_permissions(&role, &permissions);
    if !has_read {
        return Ok(false);
    }
    if is_admin {
        return Ok(true);
    }
    let allowed: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks at
         WHERE at.tenant_id = ? AND at.id = ? AND (
           COALESCE(at.initiator_user_id, at.owner_user_id) = ? OR at.owner_user_id = ?
           OR EXISTS (
             SELECT 1 FROM agent_task_grants grants
             WHERE grants.tenant_id = at.tenant_id
               AND grants.task_id = COALESCE(at.root_task_id, at.id)
               AND grants.grantee_type = 'user' AND grants.grantee_id = ?
               AND grants.revoked_at IS NULL
           )
         )",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(user_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(allowed > 0)
}

pub(crate) async fn external_destination_is_bound(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    channel_id: &str,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_external_identity_links
         WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
           AND status = 'active' AND revoked_at IS NULL",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(channel_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(count > 0)
}

fn append_own_visibility(sql: &mut String) {
    sql.push_str(
        r" AND (
            COALESCE(at.initiator_user_id, at.owner_user_id) = ?
            OR at.owner_user_id = ?
            OR EXISTS (
                SELECT 1 FROM agent_task_grants grants
                WHERE grants.tenant_id = at.tenant_id
                  AND grants.task_id = COALESCE(at.root_task_id, at.id)
                  AND grants.grantee_type = 'user'
                  AND grants.grantee_id = ?
                  AND grants.revoked_at IS NULL
            )
        )",
    );
}

fn bind_own_visibility<'q>(
    mut query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    user_id: &'q str,
) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    query = query.bind(user_id).bind(user_id).bind(user_id);
    query
}

async fn resolve_visible_task_id(
    state: &AppState,
    tenant_id: &str,
    task_ref: &str,
    access: &TaskAccess,
) -> Result<String> {
    let mut sql = String::from(
        "SELECT at.id FROM agent_tasks at
         WHERE at.tenant_id = ? AND (at.id = ? OR at.short_code = ?)",
    );
    if !access.is_admin {
        append_own_visibility(&mut sql);
    }
    sql.push_str(" LIMIT 1");
    let mut query = sqlx::query_scalar::<sqlx::Sqlite, String>(&sql)
        .bind(tenant_id)
        .bind(task_ref)
        .bind(normalize_short_code(task_ref));
    if !access.is_admin {
        query = query
            .bind(&access.user_id)
            .bind(&access.user_id)
            .bind(&access.user_id);
    }
    query
        .fetch_optional(state.control_db())
        .await?
        .ok_or_else(|| AppError::NotFound("task not found".to_string()))
}

async fn task_can_control_by_id(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    access: &TaskAccess,
) -> Result<bool> {
    if !access.can_control {
        return Ok(false);
    }
    if access.is_admin {
        return Ok(true);
    }
    let count = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_tasks at
         WHERE at.tenant_id = ? AND at.id = ? AND (
           at.owner_user_id = ? OR at.initiator_user_id = ? OR EXISTS (
             SELECT 1 FROM agent_task_grants grants
             WHERE grants.tenant_id = at.tenant_id
               AND grants.task_id = COALESCE(at.root_task_id, at.id)
               AND grants.grantee_type = 'user' AND grants.grantee_id = ?
               AND grants.permission IN ('control','write') AND grants.revoked_at IS NULL
           )
         )",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(&access.user_id)
    .bind(&access.user_id)
    .bind(&access.user_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(count > 0)
}

fn normalize_short_code(value: &str) -> String {
    value.trim().trim_start_matches('#').to_ascii_uppercase()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskListQuery {
    scope: Option<String>,
    status: Option<String>,
    bucket: Option<String>,
    capability_key: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    include_archived: Option<bool>,
    include_children: Option<bool>,
}

fn task_list_includes_archived(query: &TaskListQuery) -> bool {
    let history_bucket = query
        .bucket
        .as_deref()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("history"));
    query.include_archived.unwrap_or(history_bucket)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskListResponse {
    items: Vec<TaskView>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskView {
    id: String,
    short_code: String,
    root_task_id: String,
    parent_task_id: Option<String>,
    title: String,
    summary: Option<String>,
    capability_key: String,
    source: String,
    source_label: Option<String>,
    status: String,
    phase: String,
    state_version: u64,
    progress_percent: i32,
    progress: Option<Value>,
    desired_state: Option<String>,
    last_event: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    result_summary: Option<String>,
    result_artifact_ref: Option<String>,
    sensitivity_label: String,
    origin_session_id: Option<String>,
    origin_turn_id: Option<String>,
    #[serde(skip)]
    linked_resource_type: Option<String>,
    #[serde(skip)]
    linked_resource_id: Option<String>,
    external_platform: Option<String>,
    external_conversation_id: Option<String>,
    owner_user_id: Option<String>,
    initiator_user_id: Option<String>,
    assigned_user_id: Option<String>,
    last_progress_at: Option<String>,
    sla_due_at: Option<String>,
    budget: Option<Value>,
    cost: Option<Value>,
    archived: bool,
    created_at: String,
    updated_at: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    allowed_actions: Vec<String>,
}

const TASK_VIEW_SELECT: &str = r"
at.id, at.short_code, at.root_task_id, at.parent_task_id, at.title, at.summary,
at.capability_key, at.source, at.source_label, at.status, at.phase, at.state_version,
at.progress_percent, CAST(at.progress_json AS TEXT) AS progress_json, at.desired_state,
at.last_event, at.error_code, at.error_message, at.result_summary, at.result_artifact_ref,
at.sensitivity_label, at.origin_session_id, at.origin_turn_id, at.external_platform,
at.external_conversation_id, at.linked_resource_type, at.linked_resource_id,
at.owner_user_id, at.initiator_user_id, at.archived,
at.assigned_user_id, CAST(at.last_progress_at AS TEXT) AS last_progress_at,
CAST(at.sla_due_at AS TEXT) AS sla_due_at, CAST(at.budget_json AS TEXT) AS budget_json,
CAST(at.cost_json AS TEXT) AS cost_json,
CAST(at.created_at AS TEXT) AS created_at, CAST(at.updated_at AS TEXT) AS updated_at,
CAST(at.started_at AS TEXT) AS started_at, CAST(at.completed_at AS TEXT) AS completed_at
";

fn value_from_json_text(value: Option<String>) -> Option<Value> {
    value.and_then(|raw| serde_json::from_str(&raw).ok())
}

fn allowed_actions(
    status: &str,
    can_control: bool,
    source: &str,
    linked_resource_type: Option<&str>,
    linked_resource_id: Option<&str>,
    origin_turn_id: Option<&str>,
) -> Vec<String> {
    let mut actions = vec!["open_result".to_string(), "subscribe".to_string()];
    if !can_control {
        return actions;
    }
    let has_linked_resource = linked_resource_id.is_some();
    let can_cancel = linked_resource_type
        .filter(|_| has_linked_resource)
        .is_some_and(crate::routes::agent_ops::linked_resource_cancel_supported)
        || origin_turn_id.is_some();
    let can_retry = linked_resource_type
        .filter(|_| has_linked_resource)
        .is_some_and(crate::routes::agent_ops::linked_resource_retry_supported);
    if ACTIVE_STATUSES.contains(&status) && can_cancel {
        actions.push("cancel".to_string());
    }
    if matches!(status, "failed" | "cancelled" | "timed_out" | "stale") && can_retry {
        actions.push("retry".to_string());
    }
    if status == "waiting_input" && source == "bot" {
        actions.push("provide_input".to_string());
    }
    if status == "waiting_approval" {
        if linked_resource_type == Some("rd_task") && has_linked_resource {
            actions.push("approve".to_string());
        }
        if can_cancel {
            actions.push("reject".to_string());
        }
    }
    actions
}

fn task_view(row: sqlx::sqlite::SqliteRow, can_control: bool) -> TaskView {
    let id: String = row.get("id");
    let status: String = row.get("status");
    let linked_resource_type: Option<String> = row.get("linked_resource_type");
    let linked_resource_id: Option<String> = row.get("linked_resource_id");
    let origin_turn_id: Option<String> = row.get("origin_turn_id");
    let source: String = row.get("source");
    let initial_allowed_actions = allowed_actions(
        &status,
        can_control,
        &source,
        linked_resource_type.as_deref(),
        linked_resource_id.as_deref(),
        origin_turn_id.as_deref(),
    );
    TaskView {
        short_code: row
            .get::<Option<String>, _>("short_code")
            .unwrap_or_else(|| fallback_short_code(&id)),
        root_task_id: row
            .get::<Option<String>, _>("root_task_id")
            .unwrap_or_else(|| id.clone()),
        id,
        parent_task_id: row.get("parent_task_id"),
        title: row.get("title"),
        summary: row.get("summary"),
        capability_key: row.get("capability_key"),
        source,
        source_label: row.get("source_label"),
        status: status.clone(),
        phase: row.get("phase"),
        state_version: row.get::<u64, _>("state_version"),
        progress_percent: row.get("progress_percent"),
        progress: value_from_json_text(row.get("progress_json")),
        desired_state: row.get("desired_state"),
        last_event: row.get("last_event"),
        error_code: row.get("error_code"),
        error_message: row.get("error_message"),
        result_summary: row.get("result_summary"),
        result_artifact_ref: row.get("result_artifact_ref"),
        sensitivity_label: row.get("sensitivity_label"),
        origin_session_id: row.get("origin_session_id"),
        origin_turn_id,
        linked_resource_type,
        linked_resource_id,
        external_platform: row.get("external_platform"),
        external_conversation_id: row.get("external_conversation_id"),
        owner_user_id: row.get("owner_user_id"),
        initiator_user_id: row.get("initiator_user_id"),
        assigned_user_id: row.get("assigned_user_id"),
        last_progress_at: row.get("last_progress_at"),
        sla_due_at: row.get("sla_due_at"),
        budget: value_from_json_text(row.get("budget_json")),
        cost: value_from_json_text(row.get("cost_json")),
        archived: row.get("archived"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        allowed_actions: initial_allowed_actions,
    }
}

fn task_owned_by(task: &TaskView, user_id: &str) -> bool {
    task.owner_user_id.as_deref() == Some(user_id)
        || task.initiator_user_id.as_deref() == Some(user_id)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskCursor {
    updated_at: String,
    id: String,
}

fn decode_cursor(value: Option<&str>) -> Result<Option<TaskCursor>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppError::ValidationError("invalid task cursor".to_string()))?;
    serde_json::from_slice(&decoded)
        .map(Some)
        .map_err(|_| AppError::ValidationError("invalid task cursor".to_string()))
}

fn encode_cursor(task: &TaskView) -> Option<String> {
    serde_json::to_vec(&TaskCursor {
        updated_at: task.updated_at.clone(),
        id: task.id.clone(),
    })
    .ok()
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
}

async fn list_tasks(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<TaskListResponse>> {
    let access = task_access(&state, &claims).await?;
    let scope = query.scope.as_deref().unwrap_or("own");
    if scope == "tenant" && !access.is_admin {
        return Err(AppError::Forbidden);
    }
    if scope == "team" {
        return Err(AppError::ValidationError(
            "team task scope is unavailable until an organization directory is configured"
                .to_string(),
        ));
    }
    if !matches!(scope, "own" | "tenant") {
        return Err(AppError::ValidationError("invalid task scope".to_string()));
    }
    let cursor = decode_cursor(query.cursor.as_deref())?;
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut sql = format!("SELECT {TASK_VIEW_SELECT} FROM agent_tasks at WHERE at.tenant_id = ?");
    if scope != "tenant" {
        append_own_visibility(&mut sql);
    }
    // History is the terminal-task ledger, so it must include legacy rows that an
    // older projection automatically archived. Other buckets keep archive hiding.
    if !task_list_includes_archived(&query) {
        sql.push_str(" AND at.archived = 0");
    }
    if !query.include_children.unwrap_or(false) {
        sql.push_str(" AND (at.parent_task_id IS NULL OR at.root_task_id = at.id)");
    }
    if query
        .status
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        sql.push_str(" AND at.status = ?");
    }
    match query
        .bucket
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("active") => sql.push_str(
            " AND at.status IN ('created','queued','claimed','running','retrying','cancelling')",
        ),
        Some("waiting") => {
            sql.push_str(" AND at.status IN ('waiting_input','waiting_approval','blocked')")
        }
        Some("history") => {
            sql.push_str(" AND at.status IN ('completed','failed','cancelled','timed_out','stale')")
        }
        Some("failed") => sql.push_str(" AND at.status IN ('failed','timed_out','stale')"),
        Some("following") => sql.push_str(
            " AND EXISTS (
                SELECT 1 FROM agent_task_subscriptions subscriptions
                WHERE subscriptions.tenant_id = at.tenant_id
                  AND subscriptions.task_id = at.id
                  AND subscriptions.user_id = ? AND subscriptions.enabled = 1
              )",
        ),
        Some(_) => {
            return Err(AppError::ValidationError("invalid task bucket".to_string()));
        }
        None => {}
    }
    if query
        .capability_key
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        sql.push_str(" AND at.capability_key = ?");
    }
    if cursor.is_some() {
        sql.push_str(" AND (at.updated_at < ? OR (at.updated_at = ? AND at.id < ?))");
    }
    sql.push_str(" ORDER BY at.updated_at DESC, at.id DESC LIMIT ?");
    let mut db_query = sqlx::query::<sqlx::Sqlite>(&sql).bind(&claims.tenant_id);
    if scope != "tenant" {
        db_query = bind_own_visibility(db_query, &access.user_id);
    }
    if let Some(status) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        db_query = db_query.bind(status);
    }
    if query.bucket.as_deref().map(str::trim) == Some("following") {
        db_query = db_query.bind(&access.user_id);
    }
    if let Some(capability) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        db_query = db_query.bind(capability);
    }
    if let Some(cursor) = cursor {
        let updated_at = cursor.updated_at;
        db_query = db_query
            .bind(updated_at.clone())
            .bind(updated_at)
            .bind(cursor.id);
    }
    let rows = db_query
        .bind(i64::from(limit) + 1)
        .fetch_all(state.control_db())
        .await?;
    let mut items = rows
        .into_iter()
        .map(|row| task_view(row, false))
        .collect::<Vec<_>>();
    let mut controlled_by_grant = HashSet::new();
    if access.can_control && !items.is_empty() {
        let mut grant_sql = String::from(
            "SELECT at.id FROM agent_tasks at
             INNER JOIN agent_task_grants grants
               ON grants.tenant_id = at.tenant_id
              AND grants.task_id = COALESCE(at.root_task_id, at.id)
             WHERE at.tenant_id = ? AND grants.grantee_type = 'user'
               AND grants.grantee_id = ? AND grants.permission IN ('control','write')
               AND grants.revoked_at IS NULL AND at.id IN (",
        );
        grant_sql.push_str(
            &std::iter::repeat_n("?", items.len())
                .collect::<Vec<_>>()
                .join(","),
        );
        grant_sql.push(')');
        let mut grant_query = sqlx::query_scalar::<sqlx::Sqlite, String>(&grant_sql)
            .bind(&claims.tenant_id)
            .bind(&access.user_id);
        for item in &items {
            grant_query = grant_query.bind(&item.id);
        }
        controlled_by_grant.extend(grant_query.fetch_all(state.control_db()).await?);
    }
    for item in &mut items {
        let can_control_task = access.can_control
            && ((scope == "tenant" && access.is_admin)
                || task_owned_by(item, &access.user_id)
                || controlled_by_grant.contains(&item.id));
        item.allowed_actions = allowed_actions(
            &item.status,
            can_control_task,
            &item.source,
            item.linked_resource_type.as_deref(),
            item.linked_resource_id.as_deref(),
            item.origin_turn_id.as_deref(),
        );
    }
    let has_more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = has_more && !items.is_empty();
    let next_cursor = next_cursor
        .then(|| items.last().and_then(encode_cursor))
        .flatten();
    Ok(Json(TaskListResponse { items, next_cursor }))
}

#[derive(Debug, Deserialize)]
struct SummaryQuery {
    scope: Option<String>,
}

async fn summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<SummaryQuery>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let scope = query.scope.as_deref().unwrap_or("own");
    if scope == "tenant" && !access.is_admin {
        return Err(AppError::Forbidden);
    }
    if !matches!(scope, "own" | "tenant") {
        return Err(AppError::ValidationError("invalid task scope".to_string()));
    }
    let mut sql = String::from(
        "SELECT status, CAST(COUNT(*) AS INTEGER) AS count FROM agent_tasks at WHERE at.tenant_id = ? AND at.archived = 0",
    );
    if scope != "tenant" {
        append_own_visibility(&mut sql);
    }
    sql.push_str(" GROUP BY status");
    let mut query = sqlx::query::<sqlx::Sqlite>(&sql).bind(&claims.tenant_id);
    if scope != "tenant" {
        query = bind_own_visibility(query, &access.user_id);
    }
    let rows = query.fetch_all(state.control_db()).await?;
    let mut running = 0_i64;
    let mut waiting = 0_i64;
    let mut failed = 0_i64;
    let mut by_status = Vec::new();
    for row in rows {
        let status: String = row.get("status");
        let count: i64 = row.get("count");
        let (is_running, is_waiting, is_failed) = summary_membership(&status);
        if is_running {
            running += count;
        }
        if is_waiting {
            waiting += count;
        }
        if is_failed {
            failed += count;
        }
        by_status.push(json!({ "status": status, "count": count }));
    }
    Ok(Json(json!({
        "running": running,
        "waiting": waiting,
        "failed": failed,
        "byStatus": by_status,
        "scope": scope
    })))
}

async fn task_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<TaskView>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let row = sqlx::query::<sqlx::Sqlite>(&format!(
        "SELECT {TASK_VIEW_SELECT} FROM agent_tasks at WHERE at.tenant_id = ? AND at.id = ?"
    ))
    .bind(&claims.tenant_id)
    .bind(id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("task not found".to_string()))?;
    let mut task = task_view(row, false);
    let grant_control = if access.can_control && !task_owned_by(&task, &access.user_id) {
        sqlx::query_scalar::<sqlx::Sqlite, i64>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_grants WHERE tenant_id = ? AND task_id = ? AND grantee_type = 'user' AND grantee_id = ? AND permission IN ('control','write') AND revoked_at IS NULL",
        )
        .bind(&claims.tenant_id)
        .bind(&task.root_task_id)
        .bind(&access.user_id)
        .fetch_one(state.control_db())
        .await? > 0
    } else {
        false
    };
    let can_control_task =
        access.can_control && (task_owned_by(&task, &access.user_id) || grant_control);
    task.allowed_actions = allowed_actions(
        &task.status,
        can_control_task,
        &task.source,
        task.linked_resource_type.as_deref(),
        task.linked_resource_id.as_deref(),
        task.origin_turn_id.as_deref(),
    );
    Ok(Json(task))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventQuery {
    after_id: Option<u64>,
    limit: Option<u32>,
}

async fn task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let mut sql = String::from(
        r"SELECT id, event_id, task_id, root_task_id, event_type, state_version, visibility,
                  CAST(payload_json AS TEXT) AS payload_json, CAST(created_at AS TEXT) AS created_at
           FROM agent_task_outbox
           WHERE tenant_id = ? AND root_task_id = ? AND id > ?
             AND event_type <> 'task.idempotency_reused'",
    );
    sql.push_str(" AND visibility <> 'admin'");
    sql.push_str(" ORDER BY id ASC LIMIT ?");
    let rows = sqlx::query::<sqlx::Sqlite>(&sql)
        .bind(&claims.tenant_id)
        .bind(id)
        .bind(crate::sqlite_i64(query.after_id.unwrap_or(0)))
        .bind(i64::from(limit))
        .fetch_all(state.control_db())
        .await?;
    let items = rows
        .into_iter()
        .filter(|row| event_visible_to(false, row.get("visibility")))
        .map(outbox_row_json)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn task_resources(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, resource_type, resource_id, relation_type,
                CAST(metadata_json AS TEXT) AS metadata_json,
                CAST(created_at AS TEXT) AS created_at
         FROM agent_task_resource_links
         WHERE tenant_id = ? AND (task_id = ? OR root_task_id = ?)
         ORDER BY created_at ASC, id ASC",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(&id)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "resourceType": row.get::<String, _>("resource_type"),
                "resourceId": row.get::<String, _>("resource_id"),
                "relationType": row.get::<String, _>("relation_type"),
                "metadata": value_from_json_text(row.get("metadata_json")),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn task_artifacts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, artifact_type, name, artifact_ref, content_hash, mime_type, size_bytes,
                sensitivity_label, CAST(metadata_json AS TEXT) AS metadata_json,
                CAST(created_at AS TEXT) AS created_at
         FROM agent_task_artifacts artifacts
         WHERE tenant_id = ? AND task_id IN (
           SELECT id FROM agent_tasks WHERE tenant_id = ? AND COALESCE(root_task_id, id) = ?
         )
         ORDER BY created_at DESC, id DESC LIMIT 200",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "artifactType": row.get::<String, _>("artifact_type"),
                "name": row.get::<String, _>("name"),
                "artifactRef": row.get::<String, _>("artifact_ref"),
                "contentHash": row.get::<Option<String>, _>("content_hash"),
                "mimeType": row.get::<Option<String>, _>("mime_type"),
                "sizeBytes": row.get::<u64, _>("size_bytes"),
                "sensitivityLabel": row.get::<String, _>("sensitivity_label"),
                "metadata": value_from_json_text(row.get("metadata_json")),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn task_artifact_content(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((task_ref, artifact_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let root_task_id =
        resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let artifact = sqlx::query::<sqlx::Sqlite>(
        "SELECT artifacts.name, artifacts.artifact_type, artifacts.artifact_ref,
                artifacts.mime_type, artifacts.owner_user_id,
                CAST(artifacts.metadata_json AS TEXT) AS metadata_json
         FROM agent_task_artifacts artifacts
         INNER JOIN agent_tasks at
           ON at.tenant_id = artifacts.tenant_id AND at.id = artifacts.task_id
         WHERE artifacts.tenant_id = ? AND artifacts.id = ?
           AND COALESCE(at.root_task_id, at.id) = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&artifact_id)
    .bind(&root_task_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("task artifact not found".to_string()))?;
    let artifact_ref: String = artifact.get("artifact_ref");
    let artifact_type: String = artifact.get("artifact_type");
    let owner_user_id: String = artifact.get("owner_user_id");
    let metadata = value_from_json_text(artifact.get("metadata_json"));
    if artifact_type == "pm_material_asset" {
        let asset_id = artifact_ref
            .strip_prefix("pm-material-asset:")
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                AppError::Internal("invalid PM material artifact reference".to_string())
            })?;
        let asset = sqlx::query::<sqlx::Sqlite>(
            "SELECT assets.asset_type, assets.url, assets.content_text,
                    CAST(assets.meta_json AS TEXT) AS meta_json,
                    CAST(assets.job_id AS INTEGER) AS job_id
             FROM pm_material_assets assets
             INNER JOIN pm_material_jobs jobs
               ON jobs.tenant_id = assets.tenant_id AND jobs.id = assets.job_id
             WHERE assets.tenant_id = ? AND assets.id = ? AND jobs.created_by = ?
             LIMIT 1",
        )
        .bind(&claims.tenant_id)
        .bind(crate::sqlite_i64(asset_id))
        .bind(&owner_user_id)
        .fetch_optional(state.control_db())
        .await?
        .ok_or_else(|| AppError::NotFound("PM material artifact content not found".to_string()))?;
        return Ok(Json(json!({
            "id": artifact_id,
            "name": artifact.get::<String, _>("name"),
            "artifactRef": artifact_ref,
            "mimeType": artifact.get::<Option<String>, _>("mime_type"),
            "content": {
                "kind": "pm_material_asset",
                "jobId": asset.get::<u64, _>("job_id"),
                "assetType": asset.get::<String, _>("asset_type"),
                "url": asset.get::<Option<String>, _>("url"),
                "contentText": asset.get::<Option<String>, _>("content_text"),
                "metadata": value_from_json_text(asset.get("meta_json")),
                "taskMetadata": metadata,
            },
        })));
    }
    if let Some(filename) = upload_artifact_filename(&artifact_ref, &owner_user_id) {
        let path = state
            .data_dir
            .join(".aos")
            .join("uploads")
            .join(&owner_user_id)
            .join(filename);
        let file_metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| AppError::NotFound("uploaded artifact content not found".to_string()))?;
        if !file_metadata.is_file() {
            return Err(AppError::NotFound(
                "uploaded artifact content not found".to_string(),
            ));
        }
        return Ok(Json(json!({
            "id": artifact_id,
            "name": artifact.get::<String, _>("name"),
            "artifactRef": artifact_ref,
            "mimeType": artifact.get::<Option<String>, _>("mime_type"),
            "content": {
                "kind": "user_upload",
                "downloadUrl": artifact_ref,
                "sizeBytes": file_metadata.len(),
                "metadata": metadata,
            },
        })));
    }
    let generated_id = artifact_ref
        .strip_prefix("/generated/")
        .and_then(|value| value.get(..36))
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|value| value.to_string())
        .ok_or_else(|| {
            AppError::ValidationError(
                "this artifact type can only be opened from its source application".to_string(),
            )
        })?;
    let payload: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(payload_json AS TEXT) FROM chat_turn_artifacts
         WHERE tenant_id = ? AND user_id = ? AND id = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(artifact.get::<String, _>("owner_user_id"))
    .bind(generated_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("generated artifact content not found".to_string()))?;
    let content = serde_json::from_str::<Value>(&payload)
        .map_err(|error| AppError::Internal(format!("invalid artifact payload: {error}")))?;
    Ok(Json(json!({
        "id": artifact_id,
        "name": artifact.get::<String, _>("name"),
        "artifactRef": artifact_ref,
        "mimeType": artifact.get::<Option<String>, _>("mime_type"),
        "content": content,
    })))
}

fn upload_artifact_filename<'a>(artifact_ref: &'a str, owner_user_id: &str) -> Option<&'a str> {
    let prefix = format!("/api/v1/uploads/{owner_user_id}/");
    let filename = artifact_ref.strip_prefix(&prefix)?;
    (!filename.is_empty()
        && filename != "."
        && filename != ".."
        && !filename.contains(['/', '\\', '\0', '%']))
    .then_some(filename)
}

async fn task_attempts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT attempts.id, attempts.task_id, attempts.attempt_no, attempts.trigger_type,
                attempts.trigger_ref, attempts.status, attempts.worker_id, attempts.error_code,
                attempts.error_message, CAST(attempts.metadata_json AS TEXT) AS metadata_json,
                CAST(attempts.started_at AS TEXT) AS started_at,
                CAST(attempts.completed_at AS TEXT) AS completed_at,
                CAST(attempts.created_at AS TEXT) AS created_at
         FROM agent_task_attempts attempts
         INNER JOIN agent_tasks at ON at.tenant_id = attempts.tenant_id AND at.id = attempts.task_id
         WHERE attempts.tenant_id = ? AND COALESCE(at.root_task_id, at.id) = ?
         ORDER BY attempts.created_at ASC, attempts.attempt_no ASC",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "taskId": row.get::<String, _>("task_id"),
                "attemptNo": row.get::<u32, _>("attempt_no"),
                "triggerType": row.get::<String, _>("trigger_type"),
                "triggerRef": row.get::<Option<String>, _>("trigger_ref"),
                "status": row.get::<String, _>("status"),
                "workerId": row.get::<Option<String>, _>("worker_id"),
                "errorCode": row.get::<Option<String>, _>("error_code"),
                "errorMessage": row.get::<Option<String>, _>("error_message"),
                "metadata": value_from_json_text(row.get("metadata_json")),
                "startedAt": row.get::<Option<String>, _>("started_at"),
                "completedAt": row.get::<Option<String>, _>("completed_at"),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn list_commands(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let task_owner: Option<String> = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COALESCE(initiator_user_id, owner_user_id) FROM agent_tasks
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_one(state.control_db())
    .await?;
    let can_audit_all = task_owner.as_deref() == Some(&claims.sub);
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, actor_user_id, actor_type, command_type, status,
                expected_state_version, CAST(input_json AS TEXT) AS input_json,
                CAST(result_json AS TEXT) AS result_json, error_message, attempt_count,
                CAST(created_at AS TEXT) AS created_at,
                CAST(completed_at AS TEXT) AS completed_at
         FROM agent_task_command_requests
         WHERE tenant_id = ? AND task_id = ?
         ORDER BY created_at DESC LIMIT 100",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let actor_user_id = row.get::<String, _>("actor_user_id");
            let can_read_command_input = can_audit_all || actor_user_id == claims.sub;
            json!({
                "id": row.get::<String, _>("id"),
                "actorUserId": can_read_command_input.then_some(actor_user_id),
                "actorType": row.get::<String, _>("actor_type"),
                "commandType": row.get::<String, _>("command_type"),
                "status": row.get::<String, _>("status"),
                "expectedStateVersion": row.get::<Option<u64>, _>("expected_state_version"),
                "input": can_read_command_input
                    .then(|| value_from_json_text(row.get("input_json")))
                    .flatten(),
                "result": value_from_json_text(row.get("result_json")),
                "errorMessage": row.get::<Option<String>, _>("error_message"),
                "attemptCount": row.get::<u32, _>("attempt_count"),
                "createdAt": row.get::<String, _>("created_at"),
                "completedAt": row.get::<Option<String>, _>("completed_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareTaskRequest {
    user_id: String,
    permission: Option<String>,
}

async fn share_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
    Json(request): Json<ShareTaskRequest>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let owner: Option<String> = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COALESCE(initiator_user_id, owner_user_id) FROM agent_tasks
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_one(state.control_db())
    .await?;
    if owner.as_deref() != Some(&claims.sub) {
        return Err(AppError::Forbidden);
    }
    let target_user = request.user_id.trim();
    if target_user.is_empty() || target_user == claims.sub {
        return Err(AppError::ValidationError(
            "a different target user is required".to_string(),
        ));
    }
    let exists: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM users
         WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(&claims.tenant_id)
    .bind(target_user)
    .fetch_one(state.control_db())
    .await?;
    if exists == 0 {
        return Err(AppError::NotFound("target user not found".to_string()));
    }
    let permission = request
        .permission
        .as_deref()
        .unwrap_or("read")
        .trim()
        .to_ascii_lowercase();
    if !matches!(permission.as_str(), "read" | "control") {
        return Err(AppError::ValidationError(
            "permission must be read or control".to_string(),
        ));
    }
    let grant_id = format!("agtgrant-{}", uuid::Uuid::new_v4());
    let mut tx = state.control_db().begin().await?;
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_task_grants
           (id, tenant_id, task_id, grantee_type, grantee_id, permission, granted_by)
         VALUES (?, ?, ?, 'user', ?, ?, ?)
         ON CONFLICT DO UPDATE SET revoked_at = NULL, granted_by = excluded.granted_by",
    )
    .bind(&grant_id)
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(target_user)
    .bind(&permission)
    .bind(&claims.sub)
    .execute(&mut *tx)
    .await?;
    let persisted_grant_id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT id FROM agent_task_grants
         WHERE tenant_id = ? AND task_id = ? AND grantee_type = 'user'
           AND grantee_id = ? AND permission = ? AND revoked_at IS NULL
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(target_user)
    .bind(&permission)
    .fetch_one(&mut *tx)
    .await?;
    crate::routes::agent_ops::add_event_tx(
        &mut tx,
        &claims.tenant_id,
        &id,
        "shared",
        None,
        None,
        "info",
        "任务已共享给指定用户",
        Some(json!({
            "granteeUserId": target_user,
            "permission": permission,
            "actorUserId": claims.sub,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "grantId": persisted_grant_id })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryQuery {
    scope: Option<String>,
    status: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    limit: Option<u32>,
}

async fn list_deliveries(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<DeliveryQuery>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let scope = query.scope.as_deref().unwrap_or("own");
    if scope == "tenant" && !access.is_admin {
        return Err(AppError::Forbidden);
    }
    if !matches!(scope, "own" | "tenant") {
        return Err(AppError::ValidationError(
            "invalid delivery scope".to_string(),
        ));
    }
    let status = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.or(query.limit).unwrap_or(30).clamp(1, 100);
    let offset = i64::from(page.saturating_sub(1).saturating_mul(per_page));
    let mut where_sql = String::from(" WHERE deliveries.tenant_id = ?");
    if scope != "tenant" {
        where_sql.push_str(" AND deliveries.user_id = ?");
    }
    if status.is_some() {
        where_sql.push_str(" AND deliveries.status = ?");
    }
    let count_sql = format!(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_notification_deliveries deliveries{where_sql}"
    );
    let mut count_query =
        sqlx::query_scalar::<sqlx::Sqlite, i64>(&count_sql).bind(&claims.tenant_id);
    if scope != "tenant" {
        count_query = count_query.bind(&access.user_id);
    }
    if let Some(status) = status {
        count_query = count_query.bind(status);
    }
    let total = count_query.fetch_one(state.control_db()).await?.max(0);

    let mut sql = String::from(
        "SELECT deliveries.id, deliveries.task_id, at.short_code, at.title,
                deliveries.platform, deliveries.channel_id, deliveries.status,
                deliveries.attempt_count, deliveries.max_attempts,
                deliveries.provider_message_id, deliveries.last_error,
                CAST(deliveries.payload_json AS TEXT) AS payload_json,
                CAST(deliveries.sent_at AS TEXT) AS sent_at,
                CAST(deliveries.created_at AS TEXT) AS created_at,
                CAST(deliveries.updated_at AS TEXT) AS updated_at
         FROM agent_notification_deliveries deliveries
         INNER JOIN agent_tasks at ON at.tenant_id = deliveries.tenant_id AND at.id = deliveries.task_id",
    );
    sql.push_str(&where_sql);
    sql.push_str(" ORDER BY deliveries.updated_at DESC, deliveries.id DESC LIMIT ? OFFSET ?");
    let mut db_query = sqlx::query::<sqlx::Sqlite>(&sql).bind(&claims.tenant_id);
    if scope != "tenant" {
        db_query = db_query.bind(&access.user_id);
    }
    if let Some(status) = status {
        db_query = db_query.bind(status);
    }
    let rows = db_query
        .bind(i64::from(per_page))
        .bind(offset)
        .fetch_all(state.control_db())
        .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "taskId": row.get::<String, _>("task_id"),
                "shortCode": row.get::<Option<String>, _>("short_code"),
                "title": row.get::<String, _>("title"),
                "platform": row.get::<String, _>("platform"),
                "channelId": row.get::<Option<String>, _>("channel_id"),
                "status": row.get::<String, _>("status"),
                "attemptCount": row.get::<u32, _>("attempt_count"),
                "maxAttempts": row.get::<u32, _>("max_attempts"),
                "providerMessageId": row.get::<Option<String>, _>("provider_message_id"),
                "lastError": row.get::<Option<String>, _>("last_error"),
                "payload": value_from_json_text(row.get("payload_json")),
                "sentAt": row.get::<Option<String>, _>("sent_at"),
                "createdAt": row.get::<String, _>("created_at"),
                "updatedAt": row.get::<String, _>("updated_at"),
                "allowedActions": if access.is_admin
                    && matches!(row.get::<String, _>("status").as_str(), "failed" | "unknown")
                {
                    vec!["replay"]
                } else {
                    Vec::<&str>::new()
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "items": items,
        "scope": scope,
        "total": total,
        "page": page,
        "perPage": per_page
    })))
}

async fn replay_delivery(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(delivery_id): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    if !access.is_admin {
        return Err(AppError::Forbidden);
    }
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, status FROM agent_notification_deliveries
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&delivery_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("notification delivery not found".to_string()))?;
    let task_id: String = row.get("task_id");
    let status: String = row.get("status");
    if !matches!(status.as_str(), "failed" | "unknown") {
        return Err(AppError::ValidationError(
            "only failed or receipt-unknown notification deliveries can be replayed".to_string(),
        ));
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_notification_deliveries
         SET status = 'queued', available_at = CURRENT_TIMESTAMP, claimed_by = NULL,
             claimed_at = NULL, lease_expires_at = NULL, attempt_count = 0,
             dispatch_started_at = NULL, last_error = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ? AND status IN ('failed', 'unknown')",
    )
    .bind(&claims.tenant_id)
    .bind(&delivery_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_trace_events
           (id, tenant_id, task_id, event_type, severity, message, metadata_json)
         VALUES (?, ?, ?, 'notification_replay_requested', 'info', ?, ?)",
    )
    .bind(format!("agtr-{}", uuid::Uuid::new_v4()))
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .bind("管理员已请求重新投递失败通知")
    .bind(
        json!({
            "deliveryId": delivery_id,
            "actorUserId": claims.sub,
        })
        .to_string(),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "ok": true,
        "deliveryId": delivery_id,
        "status": "queued",
    })))
}

fn outbox_row_json(row: sqlx::sqlite::SqliteRow) -> Value {
    let payload = value_from_json_text(row.get("payload_json")).unwrap_or(Value::Null);
    json!({
        "id": row.get::<u64, _>("id"),
        "eventId": row.get::<String, _>("event_id"),
        "taskId": row.get::<String, _>("task_id"),
        "rootTaskId": row.get::<String, _>("root_task_id"),
        "eventType": row.get::<String, _>("event_type"),
        "stateVersion": row.get::<u64, _>("state_version"),
        "visibility": row.get::<String, _>("visibility"),
        "payload": payload,
        "createdAt": row.get::<String, _>("created_at"),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamQuery {
    after_event_id: Option<u64>,
    scope: Option<String>,
}

async fn task_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    let access = task_access(&state, &claims).await?;
    let tenant_id = claims.tenant_id.clone();
    let user_id = access.user_id.clone();
    let stream_claims = claims.clone();
    let scope = query.scope.as_deref().unwrap_or("own");
    if scope == "tenant" && !access.is_admin {
        return Err(AppError::Forbidden);
    }
    if !matches!(scope, "own" | "tenant") {
        return Err(AppError::ValidationError("invalid task scope".to_string()));
    }
    let tenant_scope = scope == "tenant";
    let mut after_id = query.after_event_id.unwrap_or(0);
    let mut outbox_changes = subscribe_task_outbox(&tenant_id);
    let recovery_poll = Duration::from_millis(
        std::env::var("TASK_CONTROL_SSE_RECOVERY_POLL_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(15_000)
            .clamp(3_000, 60_000),
    );
    let stream = async_stream::stream! {
        let mut next_authorization_check = tokio::time::Instant::now();
        let mut error_delay = Duration::from_millis(750);
        loop {
            if tokio::time::Instant::now() >= next_authorization_check {
                match task_access(&state, &stream_claims).await {
                    Ok(refreshed) if !tenant_scope || refreshed.is_admin => {
                        next_authorization_check = tokio::time::Instant::now()
                            + Duration::from_secs(15);
                    }
                    _ => {
                        yield Ok(Event::default().event("authorization_revoked").data(
                            json!({"message": "task stream authorization is no longer valid"}).to_string()
                        ));
                        break;
                    }
                }
            }
            let upper_id = match sqlx::query_scalar::<sqlx::Sqlite, u64>(
                "SELECT id FROM agent_task_outbox
                 WHERE tenant_id = ? AND id > ?
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(&tenant_id)
            .bind(crate::sqlite_i64(after_id))
            .fetch_optional(state.control_db())
            .await
            {
                Ok(value) => {
                    error_delay = Duration::from_millis(750);
                    value.unwrap_or(after_id)
                }
                Err(error) => {
                    tracing::warn!(tenant_id, user_id, error = %error, "task SSE cursor snapshot failed; retrying");
                    yield Ok(Event::default().event("stream_warning").data(
                        json!({"message": "task event stream temporarily unavailable"}).to_string()
                    ));
                    tokio::time::sleep(error_delay).await;
                    error_delay = (error_delay * 2).min(Duration::from_secs(15));
                    continue;
                }
            };
            if upper_id <= after_id {
                tokio::select! {
                    changed = outbox_changes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        // insert_outbox_tx notifies before its surrounding
                        // transaction commits. Give the commit a brief head
                        // start, then read the authoritative local outbox.
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    _ = tokio::time::sleep(recovery_poll) => {}
                }
                continue;
            }
            // Recheck authorization immediately before reading a newly
            // available batch. The periodic check closes idle streams, while
            // this check prevents any post-revocation event from being sent.
            match task_access(&state, &stream_claims).await {
                Ok(refreshed) if !tenant_scope || refreshed.is_admin => {
                    next_authorization_check = tokio::time::Instant::now()
                        + Duration::from_secs(15);
                }
                _ => {
                    yield Ok(Event::default().event("authorization_revoked").data(
                        json!({"message": "task stream authorization is no longer valid"}).to_string()
                    ));
                    break;
                }
            }
            let mut sql = String::from(
                r"SELECT outbox.id, outbox.event_id, outbox.task_id, outbox.root_task_id,
                          outbox.event_type, outbox.state_version,
                          outbox.visibility, CAST(outbox.payload_json AS TEXT) AS payload_json,
                          CAST(outbox.created_at AS TEXT) AS created_at
                   FROM agent_task_outbox outbox
                   INNER JOIN agent_tasks at
                     ON at.tenant_id = outbox.tenant_id AND at.id = outbox.root_task_id
                   WHERE outbox.tenant_id = ? AND outbox.id > ? AND outbox.id <= ?
                     AND outbox.event_type <> 'task.idempotency_reused'",
            );
            if !tenant_scope {
                append_own_visibility(&mut sql);
                sql.push_str(" AND outbox.visibility <> 'admin'");
            }
            sql.push_str(" ORDER BY outbox.id ASC LIMIT 100");
            let mut db_query = sqlx::query::<sqlx::Sqlite>(&sql)
                .bind(&tenant_id)
                .bind(crate::sqlite_i64(after_id))
                .bind(crate::sqlite_i64(upper_id));
            if !tenant_scope {
                db_query = bind_own_visibility(db_query, &user_id);
            }
            match db_query.fetch_all(state.control_db()).await {
                Ok(rows) => {
                    let batch_is_complete = rows.len() < 100;
                    for row in rows {
                        let id = row.get::<u64, _>("id");
                        after_id = after_id.max(id);
                        if !event_visible_to(tenant_scope, row.get("visibility")) {
                            continue;
                        }
                        let payload = outbox_row_json(row);
                        yield Ok(Event::default()
                            .id(id.to_string())
                            .event("task_event")
                            .data(payload.to_string()));
                    }
                    if batch_is_complete {
                        after_id = upper_id;
                    }
                }
                Err(error) => {
                    tracing::warn!(tenant_id, user_id, error = %error, "task SSE query failed; retrying");
                    yield Ok(Event::default().event("stream_warning").data(
                        json!({"message": "task event stream temporarily unavailable"}).to_string()
                    ));
                }
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommandRequest {
    command_type: String,
    expected_state_version: Option<u64>,
    idempotency_key: Option<String>,
    input: Option<Value>,
}

fn supported_command(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cancel" => Some("cancel"),
        "kill" => Some("kill"),
        "retry" => Some("retry"),
        "pause" => Some("pause"),
        "resume" => Some("resume"),
        "provide_input" => Some("provide_input"),
        "approve" => Some("approve"),
        "reject" => Some("reject"),
        _ => None,
    }
}

fn command_idempotency_scope_matches(
    existing_task_id: &str,
    existing_actor_user_id: &str,
    existing_command_type: &str,
    task_id: &str,
    actor_user_id: &str,
    command_type: &str,
) -> bool {
    existing_task_id == task_id
        && existing_actor_user_id == actor_user_id
        && existing_command_type == command_type
}

async fn create_command(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
    Json(request): Json<CommandRequest>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    if !access.can_control {
        return Err(AppError::Forbidden);
    }
    let task_id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    if !task_can_control_by_id(&state, &claims.tenant_id, &task_id, &access).await? {
        return Err(AppError::Forbidden);
    }
    let command_type = supported_command(&request.command_type)
        .ok_or_else(|| AppError::ValidationError("unsupported task command".to_string()))?;
    let id = format!("agtcmd-{}", uuid::Uuid::new_v4());
    let idempotency_key = request
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            format!(
                "task-command:{task_id}:{command_type}:{}",
                uuid::Uuid::new_v4()
            )
        });
    if idempotency_key.chars().count() > 128 {
        return Err(AppError::ValidationError(
            "idempotencyKey must not exceed 128 characters".to_string(),
        ));
    }
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    if let Some(existing) = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, task_id, actor_user_id, command_type, status
         FROM agent_task_command_requests
         WHERE tenant_id = ? AND idempotency_key = ? LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&idempotency_key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if !command_idempotency_scope_matches(
            existing.get::<String, _>("task_id").as_str(),
            existing.get::<String, _>("actor_user_id").as_str(),
            existing.get::<String, _>("command_type").as_str(),
            &task_id,
            &claims.sub,
            command_type,
        ) {
            return Err(AppError::Conflict(
                "idempotency key is already bound to a different task command".to_string(),
            ));
        }
        let command_id: String = existing.get("id");
        let status: String = existing.get("status");
        tx.commit().await?;
        return Ok(Json(json!({
            "accepted": true,
            "commandId": command_id,
            "status": status,
            "reused": true
        })));
    }
    let expected_state_version = request.expected_state_version.ok_or_else(|| {
        AppError::ValidationError(
            "expectedStateVersion is required for task state changes".to_string(),
        )
    })?;
    let task_row = sqlx::query::<sqlx::Sqlite>(
        "SELECT state_version, status, source, linked_resource_type, linked_resource_id,
                origin_turn_id
         FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("task not found".to_string()))?;
    let state_version: u64 = task_row.get("state_version");
    if expected_state_version != state_version {
        return Err(AppError::ValidationError(format!(
            "task state changed; expected version {}, current version {state_version}",
            expected_state_version
        )));
    }
    let status: String = task_row.get("status");
    let source: String = task_row.get("source");
    let linked_resource_type: Option<String> = task_row.get("linked_resource_type");
    let linked_resource_id: Option<String> = task_row.get("linked_resource_id");
    let origin_turn_id: Option<String> = task_row.get("origin_turn_id");
    let available_actions = allowed_actions(
        &status,
        true,
        &source,
        linked_resource_type.as_deref(),
        linked_resource_id.as_deref(),
        origin_turn_id.as_deref(),
    );
    if !available_actions
        .iter()
        .any(|action| action == command_type)
    {
        return Err(AppError::ValidationError(format!(
            "task command '{command_type}' is not available in status '{status}'"
        )));
    }
    if command_type == "retry" {
        let pending_retry = sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT id FROM agent_task_command_requests
             WHERE tenant_id = ? AND task_id = ? AND command_type = 'retry'
               AND status IN ('queued','claimed')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&claims.tenant_id)
        .bind(&task_id)
        .fetch_optional(&mut *tx)
        .await?;
        let active_attempt = sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT id FROM agent_tasks
             WHERE tenant_id = ? AND parent_task_id = ? AND source = 'watchdog'
               AND status IN ('created','queued','claimed','running','retrying','cancelling',
                              'waiting_input','waiting_approval','blocked')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&claims.tenant_id)
        .bind(&task_id)
        .fetch_optional(&mut *tx)
        .await?;
        if pending_retry.is_some() || active_attempt.is_some() {
            return Err(AppError::Conflict(
                "a retry command or retry attempt is already active for this task".to_string(),
            ));
        }
    }
    let input_json = request.input.as_ref().map(Value::to_string);
    let insert = sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_task_command_requests
           (id, tenant_id, task_id, actor_user_id, actor_type, command_type, status,
            expected_state_version, idempotency_key, input_json)
           VALUES (?, ?, ?, ?, 'user', ?, 'queued', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .bind(&claims.sub)
    .bind(command_type)
    .bind(crate::sqlite_i64(expected_state_version))
    .bind(&idempotency_key)
    .bind(input_json)
    .execute(&mut *tx)
    .await;
    if let Err(error) = insert {
        if matches!(&error, sqlx::Error::Database(db) if db.is_unique_violation()) {
            let existing = sqlx::query::<sqlx::Sqlite>(
                "SELECT id, task_id, actor_user_id, command_type, status
                 FROM agent_task_command_requests
                 WHERE tenant_id = ? AND idempotency_key = ? LIMIT 1",
            )
            .bind(&claims.tenant_id)
            .bind(&idempotency_key)
            .fetch_optional(&mut *tx)
            .await?;
            let existing = existing.ok_or_else(|| {
                AppError::Conflict(
                    "task command idempotency race could not be resolved".to_string(),
                )
            })?;
            if !command_idempotency_scope_matches(
                existing.get::<String, _>("task_id").as_str(),
                existing.get::<String, _>("actor_user_id").as_str(),
                existing.get::<String, _>("command_type").as_str(),
                &task_id,
                &claims.sub,
                command_type,
            ) {
                return Err(AppError::Conflict(
                    "idempotency key is already bound to a different task command".to_string(),
                ));
            }
            let command_id: String = existing.get("id");
            let status: String = existing.get("status");
            tx.commit().await?;
            return Ok(Json(json!({
                "accepted": true,
                "commandId": command_id,
                "status": status,
                "reused": true
            })));
        }
        return Err(AppError::Database(error));
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks
         SET desired_state = CASE WHEN ? IN ('cancel', 'kill') THEN 'cancelled' ELSE desired_state END,
             state_version = state_version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(command_type)
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .execute(&mut *tx)
    .await?;
    insert_outbox_tx(
        &mut tx,
        &claims.tenant_id,
        &task_id,
        "task.command_requested",
        "owner",
        json!({
            "commandId": id,
            "commandType": command_type,
            "actorUserId": claims.sub,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "accepted": true,
        "commandId": id,
        "status": "queued",
        "reused": false
    })))
}

pub(crate) async fn insert_outbox_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    event_type: &str,
    visibility: &str,
    payload: Value,
) -> Result<u64> {
    let task = sqlx::query::<sqlx::Sqlite>(
        "SELECT COALESCE(root_task_id, id) AS root_task_id, state_version FROM agent_tasks WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::NotFound("task not found".to_string()))?;
    let root_task_id: String = task.get("root_task_id");
    let state_version: u64 = task.get("state_version");
    let event_id = format!("agtevt-{}", uuid::Uuid::new_v4());
    let result = sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_task_outbox
           (event_id, tenant_id, task_id, root_task_id, event_type, state_version, visibility, payload_json)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event_id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(root_task_id)
    .bind(event_type)
    .bind(crate::sqlite_i64(state_version))
    .bind(visibility)
    .bind(payload.to_string())
    .execute(&mut **tx)
    .await?;
    notify_task_outbox_changed(tenant_id);
    Ok(u64::try_from(result.last_insert_rowid()).unwrap_or(0))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscriptionRequest {
    event_types: Vec<String>,
    destination_type: String,
    destination_ref: Option<String>,
    policy: Option<Value>,
}

fn normalized_event_types(values: Vec<String>) -> Result<Vec<String>> {
    let mut out = values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    if out.is_empty() || out.len() > 20 {
        return Err(AppError::ValidationError(
            "eventTypes must contain between 1 and 20 values".to_string(),
        ));
    }
    Ok(out)
}

fn subscription_destination_key(destination_ref: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(destination_ref.unwrap_or_default().as_bytes());
    hex::encode(hasher.finalize())
}

async fn create_subscription(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
    Json(request): Json<SubscriptionRequest>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let task_id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let event_types = normalized_event_types(request.event_types)?;
    let destination_type = request.destination_type.trim().to_ascii_lowercase();
    if !matches!(destination_type.as_str(), "webui" | "bot" | "webhook") {
        return Err(AppError::ValidationError(
            "destinationType must be webui, bot, or webhook".to_string(),
        ));
    }
    let destination_ref = request
        .destination_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if matches!(destination_type.as_str(), "bot" | "webhook") && destination_ref.is_none() {
        return Err(AppError::ValidationError(
            "bot and webhook subscriptions require destinationRef".to_string(),
        ));
    }
    if matches!(destination_type.as_str(), "bot" | "webhook") {
        if !external_destination_is_bound(
            &state,
            &claims.tenant_id,
            &claims.sub,
            destination_ref.as_deref().unwrap_or_default(),
        )
        .await?
        {
            return Err(AppError::ValidationError(
                "the selected external destination is not bound to this user".to_string(),
            ));
        }
    }
    let proposed_id = format!("agtsub-{}", uuid::Uuid::new_v4());
    sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_task_subscriptions
           (id, tenant_id, task_id, task_key, user_id, event_types_json, destination_type,
            destination_ref, destination_key, policy_json)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
           ON CONFLICT DO UPDATE SET event_types_json = excluded.event_types_json,
             policy_json = excluded.policy_json, enabled = 1, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&proposed_id)
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .bind(&task_id)
    .bind(&claims.sub)
    .bind(
        serde_json::to_string(&event_types)
            .map_err(|error| AppError::Internal(error.to_string()))?,
    )
    .bind(&destination_type)
    .bind(destination_ref.as_deref())
    .bind(subscription_destination_key(destination_ref.as_deref()))
    .bind(request.policy.map(|value| value.to_string()))
    .execute(state.control_db())
    .await?;
    let id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT id FROM agent_task_subscriptions
         WHERE tenant_id = ? AND task_id = ? AND user_id = ? AND destination_type = ?
           AND COALESCE(destination_ref, '') = COALESCE(?, '') LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .bind(&claims.sub)
    .bind(&destination_type)
    .bind(destination_ref.as_deref())
    .fetch_one(state.control_db())
    .await?;
    Ok(Json(
        json!({ "id": id, "taskId": task_id, "enabled": true }),
    ))
}

async fn list_subscriptions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_ref): Path<String>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let task_id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"SELECT id, CAST(event_types_json AS TEXT) AS event_types_json, destination_type,
                  destination_ref, CAST(policy_json AS TEXT) AS policy_json, enabled,
                  CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
           FROM agent_task_subscriptions
           WHERE tenant_id = ? AND task_id = ? AND user_id = ?
           ORDER BY updated_at DESC",
    )
    .bind(&claims.tenant_id)
    .bind(&task_id)
    .bind(&claims.sub)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "eventTypes": value_from_json_text(row.get("event_types_json")).unwrap_or(json!([])),
                "destinationType": row.get::<String, _>("destination_type"),
                "destinationRef": row.get::<Option<String>, _>("destination_ref"),
                "policy": value_from_json_text(row.get("policy_json")),
                "enabled": row.get::<bool, _>("enabled"),
                "createdAt": row.get::<String, _>("created_at"),
                "updatedAt": row.get::<String, _>("updated_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn delete_subscription(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((task_ref, subscription_id)): Path<(String, String)>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    let task_id = resolve_visible_task_id(&state, &claims.tenant_id, &task_ref, &access).await?;
    let result = sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM agent_task_subscriptions WHERE tenant_id = ? AND task_id = ? AND user_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(task_id)
    .bind(&claims.sub)
    .bind(subscription_id)
    .execute(state.control_db())
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("subscription not found".to_string()));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchRuleRequest {
    name: String,
    scope_type: Option<String>,
    scope_ref: Option<String>,
    condition: Value,
    action: Value,
    quiet_hours: Option<Value>,
    max_actions_per_day: Option<u32>,
    requires_confirmation: Option<bool>,
    enabled: Option<bool>,
}

async fn list_watch_rules(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_watch_rules", "on").await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, name, scope_type, scope_ref,
                CAST(condition_json AS TEXT) AS condition_json,
                CAST(action_json AS TEXT) AS action_json,
                CAST(quiet_hours_json AS TEXT) AS quiet_hours_json,
                max_actions_per_day, requires_confirmation, enabled,
                CAST(created_at AS TEXT) AS created_at,
                CAST(updated_at AS TEXT) AS updated_at
         FROM agent_watch_rules
         WHERE tenant_id = ? AND user_id = ?
         ORDER BY updated_at DESC",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "name": row.get::<String, _>("name"),
                "scopeType": row.get::<String, _>("scope_type"),
                "scopeRef": row.get::<Option<String>, _>("scope_ref"),
                "condition": value_from_json_text(row.get("condition_json")),
                "action": value_from_json_text(row.get("action_json")),
                "quietHours": value_from_json_text(row.get("quiet_hours_json")),
                "maxActionsPerDay": row.get::<u32, _>("max_actions_per_day"),
                "requiresConfirmation": row.get::<bool, _>("requires_confirmation"),
                "enabled": row.get::<bool, _>("enabled"),
                "createdAt": row.get::<String, _>("created_at"),
                "updatedAt": row.get::<String, _>("updated_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn list_pending_watch_rule_actions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_watch_rules", "on").await?;
    if !access.can_control {
        return Ok(Json(json!({ "items": [] })));
    }
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT runs.id, runs.task_id, rules.name,
                CAST(rules.action_json AS TEXT) AS action_json,
                CAST(runs.detail_json AS TEXT) AS detail_json,
                tasks.short_code, tasks.title, tasks.status,
                CAST(runs.created_at AS TEXT) AS created_at
         FROM agent_watch_rule_runs runs
         INNER JOIN agent_watch_rules rules
           ON rules.tenant_id = runs.tenant_id AND rules.id = runs.rule_id
         INNER JOIN agent_tasks tasks
           ON tasks.tenant_id = runs.tenant_id AND tasks.id = runs.task_id
         WHERE runs.tenant_id = ? AND rules.user_id = ?
           AND runs.action_status = 'awaiting_confirmation'
           AND (
             tasks.owner_user_id = ? OR tasks.initiator_user_id = ? OR EXISTS (
               SELECT 1 FROM agent_task_grants grants
               WHERE grants.tenant_id = tasks.tenant_id AND grants.task_id = tasks.id
                 AND grants.grantee_type = 'user' AND grants.grantee_id = ?
                 AND grants.permission IN ('control','write') AND grants.revoked_at IS NULL
             )
           )
         ORDER BY runs.created_at ASC, runs.id ASC
         LIMIT 100",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&claims.sub)
    .bind(&claims.sub)
    .bind(&claims.sub)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "runId": row.get::<u64, _>("id"),
                "taskId": row.get::<String, _>("task_id"),
                "shortCode": row.get::<Option<String>, _>("short_code"),
                "taskTitle": row.get::<String, _>("title"),
                "taskStatus": row.get::<String, _>("status"),
                "ruleName": row.get::<String, _>("name"),
                "action": value_from_json_text(row.get("action_json")),
                "detail": value_from_json_text(row.get("detail_json")),
                "createdAt": row.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchRuleDecisionRequest {
    approve: bool,
}

async fn decide_watch_rule_action(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<u64>,
    Json(request): Json<WatchRuleDecisionRequest>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_watch_rules", "on").await?;
    if !access.can_control {
        return Err(AppError::Forbidden);
    }
    let task_id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT runs.task_id FROM agent_watch_rule_runs runs
         INNER JOIN agent_watch_rules rules
           ON rules.tenant_id = runs.tenant_id AND rules.id = runs.rule_id
         WHERE runs.tenant_id = ? AND runs.id = ? AND rules.user_id = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(crate::sqlite_i64(run_id))
    .bind(&claims.sub)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("pending watch rule action not found".to_string()))?;
    if !task_can_control_by_id(&state, &claims.tenant_id, &task_id, &access).await? {
        return Err(AppError::Forbidden);
    }

    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT runs.task_id, runs.outbox_id,
                CAST(rules.action_json AS TEXT) AS action_json,
                CAST(runs.detail_json AS TEXT) AS detail_json,
                tasks.status, tasks.state_version
         FROM agent_watch_rule_runs runs
         INNER JOIN agent_watch_rules rules
           ON rules.tenant_id = runs.tenant_id AND rules.id = runs.rule_id
         INNER JOIN agent_tasks tasks
           ON tasks.tenant_id = runs.tenant_id AND tasks.id = runs.task_id
         WHERE runs.tenant_id = ? AND runs.id = ? AND rules.user_id = ?
           AND runs.action_status = 'awaiting_confirmation'",
    )
    .bind(&claims.tenant_id)
    .bind(crate::sqlite_i64(run_id))
    .bind(&claims.sub)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        AppError::Conflict("watch rule action was already decided or is unavailable".to_string())
    })?;
    let task_id: String = row.get("task_id");
    if !request.approve {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_watch_rule_runs
             SET action_status = 'rejected', reason_code = 'rejected_by_user'
             WHERE tenant_id = ? AND id = ? AND action_status = 'awaiting_confirmation'",
        )
        .bind(&claims.tenant_id)
        .bind(crate::sqlite_i64(run_id))
        .execute(&mut *tx)
        .await?;
        insert_watch_rule_decision_trace_tx(
            &mut tx,
            &claims.tenant_id,
            &task_id,
            run_id,
            &claims.sub,
            false,
            "值守规则动作已被用户拒绝",
        )
        .await?;
        tx.commit().await?;
        return Ok(Json(json!({ "ok": true, "status": "rejected" })));
    }

    let action = value_from_json_text(row.get("action_json")).unwrap_or(Value::Null);
    let action_type = action
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let task_status: String = row.get("status");
    let state_version: u64 = row.get("state_version");
    let outcome = match action_type.as_str() {
        "retry_once"
            if matches!(
                task_status.as_str(),
                "failed" | "cancelled" | "timed_out" | "stale"
            ) =>
        {
            let command_id = enqueue_watch_rule_command_tx(
                &mut tx,
                &claims.tenant_id,
                &task_id,
                &claims.sub,
                "retry",
                state_version,
                run_id,
            )
            .await?;
            json!({ "kind": "command", "commandId": command_id, "commandType": "retry" })
        }
        "retry_once" => {
            return Err(AppError::Conflict(format!(
                "task status '{task_status}' is no longer retryable"
            )));
        }
        "request_approval" if task_status == "waiting_approval" => {
            let command_id = enqueue_watch_rule_command_tx(
                &mut tx,
                &claims.tenant_id,
                &task_id,
                &claims.sub,
                "approve",
                state_version,
                run_id,
            )
            .await?;
            json!({ "kind": "command", "commandId": command_id, "commandType": "approve" })
        }
        "request_approval" => {
            return Err(AppError::Conflict(format!(
                "task status '{task_status}' is no longer waiting for approval"
            )));
        }
        "escalate" => {
            let destination_type = action
                .get("destinationType")
                .and_then(Value::as_str)
                .unwrap_or("webui")
                .trim()
                .to_ascii_lowercase();
            let destination_ref = action
                .get("destinationRef")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if matches!(destination_type.as_str(), "bot" | "webhook") {
                let destination_ref = destination_ref.ok_or_else(|| {
                    AppError::ValidationError(
                        "confirmed escalation destination is missing".to_string(),
                    )
                })?;
                let bound: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
                    "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_external_identity_links
                     WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
                       AND status = 'active' AND revoked_at IS NULL",
                )
                .bind(&claims.tenant_id)
                .bind(&claims.sub)
                .bind(destination_ref)
                .fetch_one(&mut *tx)
                .await?;
                if bound == 0 {
                    return Err(AppError::Forbidden);
                }
            }
            let outbox_id: u64 = row.get::<Option<u64>, _>("outbox_id").ok_or_else(|| {
                AppError::Conflict("watch rule action has no source event".to_string())
            })?;
            let detail = value_from_json_text(row.get("detail_json")).unwrap_or(Value::Null);
            let mut payload = detail.get("deliveryPayload").cloned().unwrap_or_else(|| {
                json!({
                    "schemaVersion": 1,
                    "taskId": task_id,
                    "eventType": "task.watch_rule_escalation",
                    "title": "AOS WatchDog",
                    "body": "值守规则升级动作已获用户批准。",
                })
            });
            payload["watchRuleRunId"] = json!(run_id);
            payload["confirmationRequired"] = json!(false);
            payload["body"] = json!(format!(
                "{}\n值守规则升级动作已获用户批准。",
                payload
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ));
            crate::routes::task_control_worker::insert_delivery_tx(
                &mut tx,
                outbox_id,
                &claims.tenant_id,
                &task_id,
                None,
                &claims.sub,
                &destination_type,
                destination_ref,
                true,
                payload,
                Some(&format!("confirmed-rule-{run_id}")),
            )
            .await?;
            json!({ "kind": "delivery", "destinationType": destination_type })
        }
        _ => {
            return Err(AppError::ValidationError(
                "watch rule action cannot be confirmed".to_string(),
            ));
        }
    };
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_watch_rule_runs
         SET action_status = 'action_queued', reason_code = 'confirmed_by_user',
             detail_json = JSON_SET(
               COALESCE(detail_json, JSON_OBJECT()),
               '$.decisionOutcome',
               JSON_EXTRACT(?, '$')
             )
         WHERE tenant_id = ? AND id = ? AND action_status = 'awaiting_confirmation'",
    )
    .bind(outcome.to_string())
    .bind(&claims.tenant_id)
    .bind(crate::sqlite_i64(run_id))
    .execute(&mut *tx)
    .await?;
    insert_watch_rule_decision_trace_tx(
        &mut tx,
        &claims.tenant_id,
        &task_id,
        run_id,
        &claims.sub,
        true,
        "值守规则动作已获用户批准并进入执行队列",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "ok": true,
        "status": "action_queued",
        "outcome": outcome,
    })))
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_watch_rule_command_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    actor_user_id: &str,
    command_type: &str,
    expected_state_version: u64,
    run_id: u64,
) -> Result<String> {
    let command_id = format!("agtcmd-{}", uuid::Uuid::new_v4());
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_task_command_requests
           (id, tenant_id, task_id, actor_user_id, actor_type, command_type, status,
            expected_state_version, idempotency_key, input_json)
         VALUES (?, ?, ?, ?, 'user', ?, 'queued', ?, ?, ?)",
    )
    .bind(&command_id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(actor_user_id)
    .bind(command_type)
    .bind(crate::sqlite_i64(expected_state_version))
    .bind(format!("watch-rule:{run_id}:{command_type}"))
    .bind(json!({ "watchRuleRunId": run_id }).to_string())
    .execute(&mut **tx)
    .await?;
    let updated = sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks SET state_version = state_version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ? AND state_version = ?",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(crate::sqlite_i64(expected_state_version))
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::Conflict(
            "task changed while the watch rule action was being queued".to_string(),
        ));
    }
    insert_outbox_tx(
        tx,
        tenant_id,
        task_id,
        "task.command_requested",
        "owner",
        json!({
            "commandId": command_id,
            "commandType": command_type,
            "actorUserId": actor_user_id,
            "watchRuleRunId": run_id,
        }),
    )
    .await?;
    Ok(command_id)
}

async fn insert_watch_rule_decision_trace_tx(
    tx: &mut Transaction<'_, Sqlite>,
    tenant_id: &str,
    task_id: &str,
    run_id: u64,
    actor_user_id: &str,
    approved: bool,
    message: &str,
) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_trace_events
           (id, tenant_id, task_id, event_type, severity, message, metadata_json)
         VALUES (?, ?, ?, 'watch_rule_decision', 'info', ?, ?)",
    )
    .bind(format!("agtr-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(task_id)
    .bind(message)
    .bind(
        json!({
            "watchRuleRunId": run_id,
            "actorUserId": actor_user_id,
            "approved": approved,
        })
        .to_string(),
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn create_watch_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<WatchRuleRequest>,
) -> Result<Json<Value>> {
    let access = task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_watch_rules", "on").await?;
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(AppError::ValidationError(
            "invalid watch rule name".to_string(),
        ));
    }
    let scope_type = request
        .scope_type
        .as_deref()
        .unwrap_or("own")
        .trim()
        .to_ascii_lowercase();
    if !matches!(scope_type.as_str(), "own" | "task" | "capability") {
        return Err(AppError::ValidationError(
            "scopeType must be own, task, or capability".to_string(),
        ));
    }
    if scope_type != "own"
        && request
            .scope_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(AppError::ValidationError(
            "scopeRef is required for this rule scope".to_string(),
        ));
    }
    if !request.condition.is_object() || !request.action.is_object() {
        return Err(AppError::ValidationError(
            "condition and action must be JSON objects".to_string(),
        ));
    }
    let action_type = request
        .action
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action_type.as_str(),
        "notify" | "retry_once" | "request_approval" | "escalate"
    ) {
        return Err(AppError::ValidationError(
            "unsupported watch rule action".to_string(),
        ));
    }
    let destination_type = request
        .action
        .get("destinationType")
        .and_then(Value::as_str)
        .unwrap_or("webui")
        .trim()
        .to_ascii_lowercase();
    if !matches!(destination_type.as_str(), "webui" | "bot" | "webhook") {
        return Err(AppError::ValidationError(
            "watch rule destinationType must be webui, bot, or webhook".to_string(),
        ));
    }
    if matches!(destination_type.as_str(), "bot" | "webhook")
        && request
            .action
            .get("destinationRef")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(AppError::ValidationError(
            "watch rule bot/webhook destinations require destinationRef".to_string(),
        ));
    }
    if matches!(destination_type.as_str(), "bot" | "webhook") {
        let destination = request
            .action
            .get("destinationRef")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if !external_destination_is_bound(&state, &claims.tenant_id, &claims.sub, destination)
            .await?
        {
            return Err(AppError::ValidationError(
                "the selected watch rule destination is not bound to this user".to_string(),
            ));
        }
    }
    let requires_confirmation = request.requires_confirmation.unwrap_or(true);
    if action_type != "notify" && !requires_confirmation {
        return Err(AppError::ValidationError(
            "actions with side effects require confirmation".to_string(),
        ));
    }
    let id = format!("agtrule-{}", uuid::Uuid::new_v4());
    let scope_ref = match scope_type.as_str() {
        "task" => Some(
            resolve_visible_task_id(
                &state,
                &claims.tenant_id,
                request.scope_ref.as_deref().unwrap_or_default(),
                &access,
            )
            .await?,
        ),
        "capability" => request
            .scope_ref
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(128).collect::<String>()),
        _ => None,
    };
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO agent_watch_rules
           (id, tenant_id, user_id, name, scope_type, scope_ref, condition_json,
            action_json, quiet_hours_json, max_actions_per_day,
            requires_confirmation, enabled, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(name)
    .bind(scope_type)
    .bind(scope_ref)
    .bind(request.condition.to_string())
    .bind(request.action.to_string())
    .bind(request.quiet_hours.map(|value| value.to_string()))
    .bind(request.max_actions_per_day.unwrap_or(20).clamp(1, 1000))
    .bind(requires_confirmation)
    .bind(request.enabled.unwrap_or(true))
    .bind(&claims.sub)
    .execute(state.control_db())
    .await?;
    Ok(Json(
        json!({ "id": id, "enabled": request.enabled.unwrap_or(true) }),
    ))
}

async fn delete_watch_rule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(rule_id): Path<String>,
) -> Result<Json<Value>> {
    task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_watch_rules", "on").await?;
    let result = sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM agent_watch_rules WHERE tenant_id = ? AND user_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(rule_id)
    .execute(state.control_db())
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("watch rule not found".to_string()));
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresenceRequest {
    client_id: String,
    current_path: Option<String>,
    mobile_follow_enabled: Option<bool>,
    ttl_seconds: Option<u32>,
}

async fn get_presence(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_mobile_handoff", "on").await?;
    let enabled = sqlx::query_scalar::<sqlx::Sqlite, bool>(
        "SELECT mobile_follow_enabled FROM agent_user_presence_leases
         WHERE tenant_id = ? AND user_id = ?
         ORDER BY updated_at DESC, client_id DESC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(state.control_db())
    .await?
    .unwrap_or(false);
    Ok(Json(json!({ "mobileFollowEnabled": enabled })))
}

async fn update_presence(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<PresenceRequest>,
) -> Result<Json<Value>> {
    task_access(&state, &claims).await?;
    require_watchdog_feature(&state, &claims.tenant_id, "watchdog_mobile_handoff", "on").await?;
    let client_id = request.client_id.trim();
    if client_id.is_empty() || client_id.len() > 128 {
        return Err(AppError::ValidationError("invalid clientId".to_string()));
    }
    let ttl = request.ttl_seconds.unwrap_or(60).clamp(15, 300);
    let mobile_follow_enabled = match request.mobile_follow_enabled {
        Some(value) => value,
        None => sqlx::query_scalar::<sqlx::Sqlite, bool>(
            "SELECT mobile_follow_enabled FROM agent_user_presence_leases
             WHERE tenant_id = ? AND user_id = ?
             ORDER BY updated_at DESC, client_id DESC LIMIT 1",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .fetch_optional(state.control_db())
        .await?
        .unwrap_or(false),
    };
    sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_user_presence_leases
           (tenant_id, user_id, client_id, current_path, mobile_follow_enabled, last_seen_at, expires_at)
           VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)))
           ON CONFLICT DO UPDATE SET current_path = excluded.current_path,
             mobile_follow_enabled = excluded.mobile_follow_enabled, last_seen_at = CURRENT_TIMESTAMP,
             expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?))",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(client_id)
    .bind(request.current_path.map(|value| value.chars().take(512).collect::<String>()))
    .bind(mobile_follow_enabled)
    .bind(ttl)
    .bind(ttl)
    .execute(state.control_db())
    .await?;
    Ok(Json(json!({ "ok": true, "expiresInSeconds": ttl })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingCodeRequest {
    platform: Option<String>,
    channel_id: Option<String>,
    ttl_seconds: Option<u32>,
}

fn pairing_code() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(12)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn pairing_hash(code: &str) -> String {
    hex::encode(Sha256::digest(code.trim().to_ascii_uppercase().as_bytes()))
}

fn pairing_channel_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match expected {
        Some(expected) => actual == Some(expected),
        None => true,
    }
}

async fn create_pairing_code(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(request): Json<PairingCodeRequest>,
) -> Result<Json<Value>> {
    require_external_identity_access(&state, &claims).await?;
    if watchdog_feature_mode(
        &state,
        &claims.tenant_id,
        "watchdog_external_identity",
        "required",
    )
    .await?
        == "off"
    {
        return Err(AppError::Forbidden);
    }
    let code = pairing_code();
    let ttl = request.ttl_seconds.unwrap_or(300).clamp(60, 900);
    let id = format!("agtpair-{}", uuid::Uuid::new_v4());
    let requested_platform = request
        .platform
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if requested_platform
        .as_deref()
        .is_some_and(|value| value.len() > 64)
    {
        return Err(AppError::ValidationError(
            "invalid pairing platform".to_string(),
        ));
    }
    let channel_id = request
        .channel_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if channel_id.as_deref().is_some_and(|value| value.len() > 128) {
        return Err(AppError::ValidationError(
            "invalid pairing channel".to_string(),
        ));
    }
    let channel_platform = match channel_id.as_deref() {
        Some(channel_id) => Some(
            sqlx::query_scalar::<sqlx::Sqlite, String>(
                "SELECT platform FROM bot_agent_channels
                 WHERE tenant_id = ? AND id = ? AND enabled = 1",
            )
            .bind(&claims.tenant_id)
            .bind(channel_id)
            .fetch_optional(state.control_db())
            .await?
            .ok_or_else(|| {
                AppError::ValidationError(
                    "pairing channel was not found or is disabled".to_string(),
                )
            })?
            .to_ascii_lowercase(),
        ),
        None => None,
    };
    if requested_platform.as_deref().is_some_and(|platform| {
        channel_platform
            .as_deref()
            .is_some_and(|channel_platform| channel_platform != platform)
    }) {
        return Err(AppError::ValidationError(
            "pairing channel platform does not match the requested platform".to_string(),
        ));
    }
    let platform = channel_platform.or(requested_platform);
    sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_external_identity_pairings
           (id, tenant_id, user_id, code_hash, platform, channel_id, expires_at)
           VALUES (?, ?, ?, ?, ?, ?, datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)))",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(pairing_hash(&code))
    .bind(platform)
    .bind(channel_id)
    .bind(ttl)
    .execute(state.control_db())
    .await?;
    Ok(Json(json!({ "code": code, "expiresInSeconds": ttl })))
}

pub(crate) async fn claim_external_identity_pairing(
    state: &AppState,
    tenant_id: &str,
    code: &str,
    platform: &str,
    external_user_id: &str,
    channel_id: Option<&str>,
    external_conversation_id: Option<&str>,
    display_name: Option<&str>,
) -> Result<Option<String>> {
    if watchdog_feature_mode(state, tenant_id, "watchdog_external_identity", "required").await?
        == "off"
    {
        return Err(AppError::ValidationError(
            "external identity pairing is disabled for this tenant".to_string(),
        ));
    }
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let row = sqlx::query::<sqlx::Sqlite>(
        r"SELECT id, user_id, platform, channel_id FROM agent_external_identity_pairings
           WHERE tenant_id = ? AND code_hash = ? AND claimed_at IS NULL AND expires_at > CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(pairing_hash(code))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let expected_platform: Option<String> = row.get("platform");
    if expected_platform
        .as_deref()
        .is_some_and(|value| !value.eq_ignore_ascii_case(platform))
    {
        return Ok(None);
    }
    let expected_channel_id: Option<String> = row.get("channel_id");
    if !pairing_channel_matches(expected_channel_id.as_deref(), channel_id) {
        return Ok(None);
    }
    let pairing_id: String = row.get("id");
    let user_id: String = row.get("user_id");
    let existing_user: Option<String> = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT user_id FROM agent_external_identity_links
         WHERE tenant_id = ? AND platform = ? AND external_user_id = ?
           AND status = 'active' AND revoked_at IS NULL",
    )
    .bind(tenant_id)
    .bind(platform.to_ascii_lowercase())
    .bind(external_user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if existing_user
        .as_deref()
        .is_some_and(|existing| existing != user_id)
    {
        return Err(AppError::Conflict(
            "external identity is already bound to another user".to_string(),
        ));
    }
    sqlx::query::<sqlx::Sqlite>(
        r"INSERT INTO agent_external_identity_links
           (id, tenant_id, user_id, platform, external_user_id, channel_id,
            external_conversation_id, display_name, status, verified_at, last_seen_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'active', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
           ON CONFLICT DO UPDATE SET user_id = excluded.user_id, channel_id = excluded.channel_id,
             external_conversation_id = excluded.external_conversation_id,
             display_name = excluded.display_name, status = 'active', revoked_at = NULL,
             verified_at = CURRENT_TIMESTAMP, last_seen_at = CURRENT_TIMESTAMP",
    )
    .bind(format!("agtident-{}", uuid::Uuid::new_v4()))
    .bind(tenant_id)
    .bind(&user_id)
    .bind(platform.to_ascii_lowercase())
    .bind(external_user_id)
    .bind(channel_id)
    .bind(external_conversation_id)
    .bind(display_name)
    .execute(&mut *tx)
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_external_identity_pairings SET claimed_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(pairing_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(user_id))
}

pub(crate) async fn resolve_external_identity_user(
    state: &AppState,
    tenant_id: &str,
    platform: &str,
    external_user_id: &str,
) -> Result<Option<String>> {
    if watchdog_feature_mode(state, tenant_id, "watchdog_external_identity", "required").await?
        == "off"
    {
        return Ok(None);
    }
    let row = sqlx::query_scalar::<sqlx::Sqlite, String>(
        r"SELECT links.user_id
           FROM agent_external_identity_links links
           INNER JOIN users ON users.tenant_id = links.tenant_id AND users.id = links.user_id
           WHERE links.tenant_id = ? AND links.platform = ? AND links.external_user_id = ?
             AND links.status = 'active' AND links.revoked_at IS NULL AND users.is_active = 1
           LIMIT 1",
    )
    .bind(tenant_id)
    .bind(platform.to_ascii_lowercase())
    .bind(external_user_id)
    .fetch_optional(state.control_db())
    .await?;
    if row.is_some() {
        let _ = sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_external_identity_links SET last_seen_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND platform = ? AND external_user_id = ?",
        )
        .bind(tenant_id)
        .bind(platform.to_ascii_lowercase())
        .bind(external_user_id)
        .execute(state.control_db())
        .await;
    }
    Ok(row)
}

async fn list_external_identities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>> {
    require_external_identity_access(&state, &claims).await?;
    if watchdog_feature_mode(
        &state,
        &claims.tenant_id,
        "watchdog_external_identity",
        "required",
    )
    .await?
        == "off"
    {
        return Err(AppError::Forbidden);
    }
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"SELECT id, platform, external_user_id, channel_id, external_conversation_id,
                  display_name, status,
                  CAST(verified_at AS TEXT) AS verified_at, CAST(last_seen_at AS TEXT) AS last_seen_at
           FROM agent_external_identity_links
           WHERE tenant_id = ? AND user_id = ? AND status = 'active' AND revoked_at IS NULL
           ORDER BY updated_at DESC",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(state.control_db())
    .await?;
    let items = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "platform": row.get::<String, _>("platform"),
                "externalUserId": row.get::<String, _>("external_user_id"),
                "channelId": row.get::<Option<String>, _>("channel_id"),
                "externalConversationId": row.get::<Option<String>, _>("external_conversation_id"),
                "displayName": row.get::<Option<String>, _>("display_name"),
                "status": row.get::<String, _>("status"),
                "verifiedAt": row.get::<String, _>("verified_at"),
                "lastSeenAt": row.get::<Option<String>, _>("last_seen_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn revoke_external_identity(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    require_external_identity_access(&state, &claims).await?;
    if watchdog_feature_mode(
        &state,
        &claims.tenant_id,
        "watchdog_external_identity",
        "required",
    )
    .await?
        == "off"
    {
        return Err(AppError::Forbidden);
    }
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let link = sqlx::query::<sqlx::Sqlite>(
        "SELECT channel_id FROM agent_external_identity_links
         WHERE tenant_id = ? AND user_id = ? AND id = ? AND status = 'active'",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(link) = link else {
        return Err(AppError::NotFound(
            "external identity not found".to_string(),
        ));
    };
    let channel_id: Option<String> = link.get("channel_id");
    let result = sqlx::query::<sqlx::Sqlite>(
        r"UPDATE agent_external_identity_links
           SET status = 'revoked', revoked_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
           WHERE tenant_id = ? AND user_id = ? AND id = ? AND status = 'active'",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "external identity not found".to_string(),
        ));
    }
    if let Some(channel_id) = channel_id {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_subscriptions SET enabled = 0, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND destination_type IN ('bot','webhook')
               AND destination_ref = ?",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&channel_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_notification_deliveries
             SET status = 'failed', last_error = 'external identity was revoked', updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND platform IN ('bot','webhook') AND channel_id = ?
               AND status = 'queued'",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(channel_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

fn fallback_short_code(task_id: &str) -> String {
    let compact = task_id
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    compact
        .chars()
        .rev()
        .take(10)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

pub(crate) fn short_code_for_task_id(task_id: &str) -> String {
    let digest = Sha256::digest(task_id.as_bytes());
    let mut value = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix is 8 bytes"));
    let mut code = [b'0'; 12];
    for slot in code.iter_mut().rev() {
        let digit = (value % 36) as u8;
        *slot = if digit < 10 {
            b'0' + digit
        } else {
            b'A' + (digit - 10)
        };
        value /= 36;
    }
    String::from_utf8(code.to_vec()).expect("base36 task code is ASCII")
}

fn required_tool_string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::ValidationError(format!("{key} is required")))
}

pub(crate) async fn execute_parent_task_tool(
    state: &AppState,
    claims: &Claims,
    tool_name: &str,
    input: &Value,
) -> Result<Value> {
    match tool_name {
        "task_list" => {
            let bucket = input
                .get("bucket")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let status = input
                .get("status")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let capability_key = input
                .get("capabilityKey")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            let Json(response) = list_tasks(
                State(state.clone()),
                Extension(claims.clone()),
                Query(TaskListQuery {
                    scope: Some("own".to_string()),
                    status,
                    bucket,
                    capability_key,
                    cursor: None,
                    limit,
                    include_archived: Some(false),
                    include_children: Some(false),
                }),
            )
            .await?;
            serde_json::to_value(response).map_err(|error| AppError::Internal(error.to_string()))
        }
        "task_get" | "task_explain_blocker" | "task_open_result" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let Json(task) = task_detail(
                State(state.clone()),
                Extension(claims.clone()),
                Path(task_ref),
            )
            .await?;
            let value = serde_json::to_value(task)
                .map_err(|error| AppError::Internal(error.to_string()))?;
            if tool_name == "task_explain_blocker" {
                Ok(json!({
                    "id": value.get("id"),
                    "shortCode": value.get("shortCode"),
                    "status": value.get("status"),
                    "phase": value.get("phase"),
                    "lastEvent": value.get("lastEvent"),
                    "errorCode": value.get("errorCode"),
                    "errorMessage": value.get("errorMessage"),
                    "updatedAt": value.get("updatedAt"),
                }))
            } else if tool_name == "task_open_result" {
                Ok(json!({
                    "id": value.get("id"),
                    "shortCode": value.get("shortCode"),
                    "status": value.get("status"),
                    "resultSummary": value.get("resultSummary"),
                    "resultArtifactRef": value.get("resultArtifactRef"),
                    "originSessionId": value.get("originSessionId"),
                    "originTurnId": value.get("originTurnId"),
                }))
            } else {
                Ok(value)
            }
        }
        "task_timeline" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            let Json(value) = task_events(
                State(state.clone()),
                Extension(claims.clone()),
                Path(task_ref),
                Query(EventQuery {
                    after_id: None,
                    limit,
                }),
            )
            .await?;
            Ok(value)
        }
        "task_subscribe" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let event_types = input
                .get("eventTypes")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    vec![
                        "task.completed".to_string(),
                        "task.failed".to_string(),
                        "task.waiting_input".to_string(),
                        "task.waiting_approval".to_string(),
                    ]
                });
            let Json(value) = create_subscription(
                State(state.clone()),
                Extension(claims.clone()),
                Path(task_ref),
                Json(SubscriptionRequest {
                    event_types,
                    destination_type: "webui".to_string(),
                    destination_ref: None,
                    policy: input.get("policy").cloned(),
                }),
            )
            .await?;
            Ok(value)
        }
        "task_unsubscribe" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let subscription_id = required_tool_string(input, "subscriptionId")?.to_string();
            let Json(value) = delete_subscription(
                State(state.clone()),
                Extension(claims.clone()),
                Path((task_ref, subscription_id)),
            )
            .await?;
            Ok(value)
        }
        "task_cancel" | "task_retry" | "task_provide_input" | "task_approve" | "task_reject"
        | "task_pause" | "task_resume" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let command_type = tool_name.trim_start_matches("task_").to_string();
            let command_input = if tool_name == "task_provide_input" {
                Some(input.get("input").cloned().ok_or_else(|| {
                    AppError::ValidationError("task_provide_input requires input".to_string())
                })?)
            } else {
                input.get("input").cloned()
            };
            let Json(value) = create_command(
                State(state.clone()),
                Extension(claims.clone()),
                Path(task_ref),
                Json(CommandRequest {
                    command_type,
                    expected_state_version: input
                        .get("expectedStateVersion")
                        .and_then(Value::as_u64),
                    idempotency_key: input
                        .get("idempotencyKey")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    input: command_input,
                }),
            )
            .await?;
            Ok(value)
        }
        "task_share" => {
            let task_ref = required_tool_string(input, "taskRef")?.to_string();
            let user_id = required_tool_string(input, "userId")?.to_string();
            let permission = input
                .get("permission")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let Json(value) = share_task(
                State(state.clone()),
                Extension(claims.clone()),
                Path(task_ref),
                Json(ShareTaskRequest {
                    user_id,
                    permission,
                }),
            )
            .await?;
            Ok(value)
        }
        "task_watch_rule_list" => {
            let Json(value) =
                list_watch_rules(State(state.clone()), Extension(claims.clone())).await?;
            Ok(value)
        }
        "task_watch_rule_create" => {
            if input.get("confirmed").and_then(Value::as_bool) != Some(true) {
                return Ok(json!({
                    "created": false,
                    "confirmationRequired": true,
                    "draft": input,
                }));
            }
            let name = required_tool_string(input, "name")?.to_string();
            let condition = input
                .get("condition")
                .cloned()
                .ok_or_else(|| AppError::ValidationError("condition is required".to_string()))?;
            let action = input
                .get("action")
                .cloned()
                .ok_or_else(|| AppError::ValidationError("action is required".to_string()))?;
            let Json(value) = create_watch_rule(
                State(state.clone()),
                Extension(claims.clone()),
                Json(WatchRuleRequest {
                    name,
                    scope_type: input
                        .get("scopeType")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    scope_ref: input
                        .get("scopeRef")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    condition,
                    action,
                    quiet_hours: input.get("quietHours").cloned(),
                    max_actions_per_day: input
                        .get("maxActionsPerDay")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()),
                    requires_confirmation: Some(true),
                    enabled: Some(true),
                }),
            )
            .await?;
            Ok(value)
        }
        other => Err(AppError::ValidationError(format!(
            "unsupported parent task tool: {other}"
        ))),
    }
}

fn external_task_short_code(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| {
                    ch == '#'
                        || ch == '＃'
                        || ch.is_ascii_punctuation() && ch != '-'
                        || matches!(ch, '，' | '。' | '：' | '；' | '！' | '？')
                })
                .to_ascii_uppercase()
        })
        .find(|token| {
            (token.len() == 10 && token.chars().all(|ch| ch.is_ascii_hexdigit()))
                || (token.len() == 12 && token.chars().all(|ch| ch.is_ascii_alphanumeric()))
        })
}

fn external_task_intent(text: &str) -> Option<&'static str> {
    let compact = text.trim();
    let lower = compact.to_ascii_lowercase();
    if compact == "取消当前会话" || lower == "/cancel-session" {
        return Some("cancel_session");
    }
    let has_task_code = external_task_short_code(compact).is_some();
    let generic_progress_probe = external_task_generic_progress_probe(compact);
    let active_scope = external_task_mentions_active(compact);
    let has_task_reference = has_task_code
        || compact.contains("任务")
        || compact.contains("研究")
        || compact.contains("调研")
        || compact.contains("研发")
        || compact.contains("对抗")
        || lower.contains("task")
        || lower.contains("research")
        || lower == "/cancel latest"
        || lower.starts_with("/kill")
        || generic_progress_probe;
    if has_task_reference
        && (compact.starts_with("强制取消")
            || compact.starts_with("强制停止")
            || lower.starts_with("/kill")
            || lower.starts_with("kill "))
    {
        Some("kill")
    } else if has_task_reference
        && (compact.starts_with("取消")
            || compact.starts_with("停止")
            || compact.starts_with("停掉")
            || (active_scope && compact.contains("取消"))
            || (active_scope && compact.contains("停止"))
            || lower.starts_with("/cancel")
            || lower.starts_with("cancel")
            || lower.starts_with("stop"))
    {
        Some("cancel")
    } else if has_task_reference
        && (compact.starts_with("重试")
            || compact.starts_with("再试")
            || lower.starts_with("retry"))
    {
        Some("retry")
    } else if has_task_code
        && (compact.starts_with("补充")
            || compact.starts_with("继续")
            || lower.starts_with("provide")
            || lower.starts_with("input"))
    {
        Some("provide_input")
    } else if has_task_code
        && (compact.starts_with("批准")
            || compact.starts_with("同意")
            || lower.starts_with("approve"))
    {
        Some("approve")
    } else if has_task_code && (compact.starts_with("拒绝") || lower.starts_with("reject")) {
        Some("reject")
    } else if compact.contains("哪些任务")
        || compact.contains("任务有哪些")
        || compact.contains("任务列表")
        || compact.contains("正在执行的任务")
        || compact.contains("正在运行的任务")
        || (compact.contains("失败")
            && (compact.contains("哪些") || compact.contains("有哪") || compact.contains("有什么")))
        || lower.contains("running tasks")
        || lower.contains("task list")
        || lower.contains("what tasks")
    {
        Some("list")
    } else if has_task_reference
        && (compact.contains("进度")
            || compact.contains("状态")
            || compact.contains("卡在哪")
            || compact.contains("卡在什么")
            || compact.contains("为什么卡")
            || compact.contains("进展")
            || compact.contains("耗时")
            || compact.contains("执行到哪")
            || compact.contains("做到哪")
            || compact.contains("哪个阶段")
            || compact.contains("当前阶段")
            || compact.contains("跑了多久")
            || compact.starts_with("查看")
            || lower.contains("status")
            || lower.contains("progress")
            || lower.starts_with("show"))
    {
        Some("detail")
    } else {
        None
    }
}

fn external_task_generic_progress_probe(text: &str) -> bool {
    let compact = text
        .trim()
        .trim_matches(|ch: char| matches!(ch, '？' | '?' | '。' | '.' | '！' | '!'));
    let lower = compact.to_ascii_lowercase();
    matches!(
        compact,
        "什么进度了"
            | "现在什么进度"
            | "进度怎么样"
            | "进展怎么样"
            | "进行到哪了"
            | "做到哪了"
            | "到哪了"
    ) || (compact.contains("任务")
        && (compact.contains("耗时")
            || compact.contains("执行到哪")
            || compact.contains("做到哪")
            || compact.contains("哪个阶段")
            || compact.contains("当前阶段")
            || compact.contains("跑了多久")
            || compact.contains("已经多久")))
        || matches!(
            lower.as_str(),
            "what's the progress"
                | "what is the progress"
                | "progress update"
                | "what's the status"
                | "what is the status"
                | "status update"
        )
}

fn external_task_mentions_today(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("今天") || text.contains("今日") || lower.contains("today")
}

fn external_task_mentions_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("失败")
        || text.contains("出错")
        || text.contains("异常")
        || lower.contains("failed")
        || lower.contains("error")
}

fn external_task_mentions_active(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("正在执行")
        || text.contains("正在运行")
        || text.contains("执行中")
        || text.contains("运行中")
        || text.contains("进行中")
        || lower.contains("running")
        || lower.contains("in progress")
        || lower.contains("active task")
}

fn external_task_prefers_english(text: &str) -> bool {
    text.chars().any(|ch| ch.is_ascii_alphabetic())
        && !text
            .chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

fn external_task_reference_domain(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if text.contains("研发") || text.contains("代码") || lower.contains("coding") {
        Some("rd")
    } else if text.contains("对抗") || lower.contains("adversarial") {
        Some("adversarial")
    } else if text.contains("数据探索") || text.contains("数据分析") || lower.contains("nl2sql")
    {
        Some("nl2sql")
    } else if text.contains("研究") || text.contains("调研") || lower.contains("research") {
        Some("research")
    } else {
        None
    }
}

fn external_task_domain_matches(domain: Option<&str>, capability: &str, title: &str) -> bool {
    let title_lower = title.to_ascii_lowercase();
    match domain {
        None => true,
        Some("rd") => {
            capability == "rd_agent"
                || capability.starts_with("rd_")
                || title.contains("研发")
                || title.contains("代码")
        }
        Some("adversarial") => capability.contains("adversarial") || title.contains("对抗"),
        Some("nl2sql") => {
            capability.contains("nl2sql")
                || title.contains("数据探索")
                || title.contains("数据分析")
        }
        Some("research") => {
            capability == "pm_assistant"
                || capability.contains("research")
                || title.contains("研究")
                || title.contains("调研")
                || title_lower.contains("research")
        }
        Some(_) => true,
    }
}

fn external_task_has_relative_reference(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "刚才",
        "刚刚",
        "最近",
        "上一个",
        "上次",
        "那个",
        "latest",
        "last",
    ]
    .iter()
    .any(|needle| text.contains(needle) || lower.contains(needle))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalTaskCandidate {
    short_code: String,
    title: String,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExternalTaskResolution {
    Resolved(String),
    Ambiguous(Vec<ExternalTaskCandidate>),
    NotFound,
}

fn external_task_reference_phrase(text: &str) -> Option<String> {
    let mut phrase = text.to_lowercase();
    if let Some(code) = external_task_short_code(text) {
        phrase = phrase.replace(&code.to_lowercase(), " ");
    }
    for noise in [
        "强制取消",
        "强制停止",
        "/cancel-session",
        "/cancel",
        "/kill",
        "取消",
        "停止",
        "停掉",
        "重试",
        "再试",
        "查看",
        "刚才",
        "刚刚",
        "最近",
        "上一个",
        "上次",
        "那个",
        "这个",
        "latest",
        "last",
        "cancel",
        "kill",
        "stop",
        "retry",
        "show",
        "task",
        "任务",
        "卡在哪里",
        "卡在什么",
        "为什么卡",
        "进度",
        "状态",
        "进展",
        "一下",
        "请",
        "的",
        "#",
        "＃",
    ] {
        phrase = phrase.replace(noise, " ");
    }
    let phrase = phrase
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|ch: char| ch.is_ascii_punctuation() || ch.is_whitespace())
        .to_string();
    (phrase.chars().count() >= 2).then_some(phrase)
}

fn external_task_title_match_score(title: &str, phrase: &str) -> usize {
    let title = title.to_lowercase();
    if title.contains(phrase) || phrase.contains(&title) {
        return 100 + phrase.chars().count();
    }
    phrase
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|token| token.chars().count() >= 2 && title.contains(token))
        .map(|token| token.chars().count())
        .sum()
}

async fn resolve_external_relative_task(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    text: &str,
    intent: &str,
    external_conversation_id: Option<&str>,
) -> Result<ExternalTaskResolution> {
    let domain = external_task_reference_domain(text);
    let allow_implicit_write = external_task_has_relative_reference(text)
        || (matches!(intent, "cancel" | "kill") && external_task_mentions_active(text))
        || (intent == "retry" && external_task_mentions_failure(text));
    if matches!(
        intent,
        "cancel" | "kill" | "retry" | "provide_input" | "approve" | "reject"
    ) && !allow_implicit_write
    {
        return Ok(ExternalTaskResolution::NotFound);
    }
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT at.short_code, at.capability_key, at.title, at.status,
                at.external_conversation_id
         FROM agent_tasks at
         WHERE at.tenant_id = ? AND at.archived = 0
           AND (at.parent_task_id IS NULL OR at.root_task_id = at.id)
           AND (
             COALESCE(at.initiator_user_id, at.owner_user_id) = ?
             OR at.owner_user_id = ?
             OR EXISTS (
               SELECT 1 FROM agent_task_grants grants
               WHERE grants.tenant_id = at.tenant_id AND grants.task_id = at.id
                 AND grants.grantee_type = 'user' AND grants.grantee_id = ?
                 AND grants.revoked_at IS NULL
             )
           )
         ORDER BY at.updated_at DESC, at.id DESC
         LIMIT 30",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_all(state.control_db())
    .await?;
    let phrase =
        if external_task_generic_progress_probe(text) || external_task_mentions_active(text) {
            None
        } else {
            external_task_reference_phrase(text)
        };
    let prefer_conversation = external_conversation_id
        .filter(|value| !value.trim().is_empty())
        .filter(|_| {
            external_task_generic_progress_probe(text) || external_task_has_relative_reference(text)
        });
    let mut candidates = rows
        .into_iter()
        .filter_map(|row| {
            let status: String = row.get("status");
            let status_matches = match intent {
                "cancel" | "kill" => ACTIVE_STATUSES.contains(&status.as_str()),
                "retry" => matches!(
                    status.as_str(),
                    "failed" | "cancelled" | "timed_out" | "stale"
                ),
                "provide_input" => status == "waiting_input",
                "approve" | "reject" => status == "waiting_approval",
                _ => true,
            };
            if !status_matches
                || !external_task_domain_matches(
                    domain,
                    &row.get::<String, _>("capability_key"),
                    &row.get::<String, _>("title"),
                )
            {
                return None;
            }
            let short_code = row.get::<Option<String>, _>("short_code")?;
            let title = row.get::<String, _>("title");
            let mut score = phrase
                .as_deref()
                .map_or(1, |value| external_task_title_match_score(&title, value));
            // A conversation match may resolve a generic relative reference
            // ("cancel the previous task") or a read-only progress query.  It
            // must not break a title-match tie for a write operation: when two
            // active tasks both match "weather", cancellation must ask the
            // user which task they mean instead of silently picking the task
            // from the current IM conversation.
            let conversation_may_break_tie = phrase.is_none()
                || !matches!(
                    intent,
                    "cancel" | "kill" | "retry" | "provide_input" | "approve" | "reject"
                );
            if conversation_may_break_tie
                && prefer_conversation.is_some_and(|conversation_id| {
                    row.get::<Option<String>, _>("external_conversation_id")
                        .as_deref()
                        == Some(conversation_id)
                })
            {
                score += 1_000;
            }
            (score > 0).then_some((
                score,
                ExternalTaskCandidate {
                    short_code,
                    title,
                    status,
                },
            ))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(ExternalTaskResolution::NotFound);
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let explicit_latest = text.trim().eq_ignore_ascii_case("/cancel latest");
    if explicit_latest || candidates.len() == 1 {
        return Ok(ExternalTaskResolution::Resolved(
            candidates.remove(0).1.short_code,
        ));
    }
    let best_score = candidates[0].0;
    let best = candidates
        .into_iter()
        .filter(|(score, _)| *score == best_score)
        .map(|(_, candidate)| candidate)
        .take(5)
        .collect::<Vec<_>>();
    if best.len() == 1 && (phrase.is_some() || prefer_conversation.is_some()) {
        return Ok(ExternalTaskResolution::Resolved(best[0].short_code.clone()));
    }
    Ok(ExternalTaskResolution::Ambiguous(best))
}

fn external_supplemental_input(text: &str, short_code: &str) -> Option<String> {
    let mut kept = Vec::new();
    for (index, token) in text.split_whitespace().enumerate() {
        let normalized = token
            .trim_matches(|ch: char| {
                ch == '#'
                    || ch == '＃'
                    || ch.is_ascii_punctuation() && ch != '-'
                    || matches!(ch, '，' | '。' | '：' | '；' | '！' | '？')
            })
            .to_ascii_uppercase();
        if normalized == short_code || index == 0 {
            continue;
        }
        kept.push(token);
    }
    let value = kept.join(" ").trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(crate) async fn handle_external_task_request(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    text: &str,
    idempotency_key: &str,
    external_conversation_id: Option<&str>,
) -> Result<Option<String>> {
    let Some(intent) = external_task_intent(text) else {
        return Ok(None);
    };
    let english = external_task_prefers_english(text);
    if watchdog_feature_mode(state, tenant_id, "watchdog_control_plane_v2", "on").await? != "on" {
        return Ok(Some(if english {
            "Task control is not enabled for this tenant. Open the original task in the WebUI."
                .to_string()
        } else {
            "当前租户尚未启用任务指挥控制面，请在 WebUI 中查看原任务。".to_string()
        }));
    }
    if intent == "cancel_session" {
        let rows = sqlx::query::<sqlx::Sqlite>(
            "SELECT at.title, at.short_code, at.status
             FROM agent_tasks at
             WHERE at.tenant_id = ? AND at.archived = 0
               AND at.external_conversation_id = ?
               AND at.status IN ('created','queued','claimed','running','retrying','cancelling',
                                 'waiting_input','waiting_approval','blocked')
               AND (COALESCE(at.initiator_user_id, at.owner_user_id) = ? OR at.owner_user_id = ?)
             ORDER BY at.updated_at DESC LIMIT 8",
        )
        .bind(tenant_id)
        .bind(external_conversation_id.unwrap_or_default())
        .bind(user_id)
        .bind(user_id)
        .fetch_all(state.control_db())
        .await?;
        if rows.is_empty() {
            return Ok(Some(if english {
                "This conversation has no running or queued tasks.".to_string()
            } else {
                "当前会话没有运行中或排队中的任务。".to_string()
            }));
        }
        let mut lines = vec![if english {
            "This conversation has the following tasks. Choose one by its task code:".to_string()
        } else {
            "当前会话有以下任务，请用编号明确选择取消范围：".to_string()
        }];
        for row in rows {
            lines.push(format!(
                "- {} · #{} · {}",
                row.get::<String, _>("title"),
                row.get::<Option<String>, _>("short_code")
                    .unwrap_or_else(|| "UNKNOWN".to_string()),
                task_status_label_localized(&row.get::<String, _>("status"), english),
            ));
        }
        lines.push(if english {
            "Use `/cancel #TASK_CODE` for cooperative cancellation or `/kill #TASK_CODE` for immediate cancellation."
                .to_string()
        } else {
            "使用“/cancel #任务编号”协作取消，或“/kill #任务编号”立即终止。".to_string()
        });
        return Ok(Some(lines.join("\n")));
    }
    if intent == "list" {
        let rows = sqlx::query::<sqlx::Sqlite>(
            "SELECT title, short_code, capability_key, status, phase, last_event,
                    CAST(date(created_at, 'localtime') = date('now', 'localtime') AS INTEGER) AS is_today,
                    CAST(date(COALESCE(completed_at, updated_at), 'localtime') = date('now', 'localtime') AS INTEGER) AS is_updated_today,
                    CAST(updated_at AS TEXT) AS updated_at
             FROM agent_tasks at
             WHERE at.tenant_id = ? AND at.archived = 0
               AND (at.parent_task_id IS NULL OR at.root_task_id = at.id)
               AND (
                 COALESCE(at.initiator_user_id, at.owner_user_id) = ?
                 OR at.owner_user_id = ?
                 OR EXISTS (
                   SELECT 1 FROM agent_task_grants grants
                   WHERE grants.tenant_id = at.tenant_id AND grants.task_id = at.id
                     AND grants.grantee_type = 'user' AND grants.grantee_id = ?
                     AND grants.revoked_at IS NULL
                 )
               )
             ORDER BY CASE at.status
                        WHEN 'waiting_input' THEN 1 WHEN 'waiting_approval' THEN 2
                        WHEN 'blocked' THEN 3 WHEN 'running' THEN 4
                        WHEN 'claimed' THEN 5 WHEN 'queued' THEN 6
                        WHEN 'retrying' THEN 7 WHEN 'cancelling' THEN 8
                        WHEN 'failed' THEN 9 WHEN 'completed' THEN 10
                        WHEN 'cancelled' THEN 11 ELSE 12 END,
                      at.updated_at DESC
             LIMIT 50",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .fetch_all(state.control_db())
        .await?;
        let domain = external_task_reference_domain(text);
        let today_only = external_task_mentions_today(text);
        let failed_only = external_task_mentions_failure(text);
        let active_only = external_task_mentions_active(text);
        let rows = rows
            .into_iter()
            .filter(|row| {
                (!today_only
                    || if failed_only {
                        row.get::<i64, _>("is_updated_today") == 1
                    } else {
                        row.get::<i64, _>("is_today") == 1
                    })
                    && (!failed_only
                        || matches!(
                            row.get::<String, _>("status").as_str(),
                            "failed" | "timed_out" | "stale"
                        ))
                    && (!active_only
                        || ACTIVE_STATUSES.contains(&row.get::<String, _>("status").as_str()))
                    && external_task_domain_matches(
                        domain,
                        &row.get::<String, _>("capability_key"),
                        &row.get::<String, _>("title"),
                    )
            })
            .take(8)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            return Ok(Some(if english && active_only {
                "There are no running or queued tasks.".to_string()
            } else if english {
                "No visible tasks match this request.".to_string()
            } else if active_only {
                "当前没有正在执行或排队的任务。".to_string()
            } else {
                "当前没有可见任务。".to_string()
            }));
        }
        let mut lines = vec![if english && today_only && failed_only {
            "Tasks that failed today:".to_string()
        } else if english && active_only {
            "Running and queued tasks:".to_string()
        } else if english {
            "Current tasks:".to_string()
        } else if today_only && failed_only {
            "今天失败或异常的任务：".to_string()
        } else if active_only {
            "正在执行或排队的任务：".to_string()
        } else {
            "当前任务：".to_string()
        }];
        for row in rows {
            let code = row
                .get::<Option<String>, _>("short_code")
                .unwrap_or_else(|| "UNKNOWN".to_string());
            lines.push(format!(
                "- {} · #{} · {} · {}",
                row.get::<String, _>("title"),
                code,
                task_status_label_localized(&row.get::<String, _>("status"), english),
                row.get::<Option<String>, _>("last_event")
                    .unwrap_or_else(|| row.get::<String, _>("phase"))
            ));
        }
        return Ok(Some(lines.join("\n")));
    }

    let resolution = match external_task_short_code(text) {
        Some(code) => ExternalTaskResolution::Resolved(code),
        None => {
            resolve_external_relative_task(
                state,
                tenant_id,
                user_id,
                text,
                intent,
                external_conversation_id,
            )
            .await?
        }
    };
    let short_code = match resolution {
        ExternalTaskResolution::Resolved(code) => code,
        ExternalTaskResolution::NotFound => {
            return Ok(Some(if english {
                "No controllable task matched. Ask for `task list` or use a stable code such as `/cancel #A1B2C3D4E5`."
                    .to_string()
            } else {
                "没有找到匹配的可控任务。你可以说“任务列表”，或使用稳定编号，例如“查看 #A1B2C3D4E5”。".to_string()
            }));
        }
        ExternalTaskResolution::Ambiguous(candidates) => {
            let mut lines = vec![if english {
                "Multiple tasks matched. No action was taken. Choose one by task code:".to_string()
            } else {
                "找到多个可能的任务，未执行任何操作。请使用任务编号明确选择：".to_string()
            }];
            for candidate in candidates {
                lines.push(format!(
                    "- {} · #{} · {}",
                    candidate.title,
                    candidate.short_code,
                    task_status_label_localized(&candidate.status, english),
                ));
            }
            return Ok(Some(lines.join("\n")));
        }
    };
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT at.id, at.title, at.short_code, at.status, at.phase, at.progress_percent,
                at.last_event, at.state_version, CAST(at.updated_at AS TEXT) AS updated_at,
                CAST(strftime('%s', 'now') - strftime('%s', at.created_at) AS INTEGER) AS elapsed_seconds
         FROM agent_tasks at
         WHERE at.tenant_id = ? AND at.short_code = ?
           AND (
             COALESCE(at.initiator_user_id, at.owner_user_id) = ?
             OR at.owner_user_id = ?
             OR EXISTS (
               SELECT 1 FROM agent_task_grants grants
               WHERE grants.tenant_id = at.tenant_id AND grants.task_id = at.id
                 AND grants.grantee_type = 'user' AND grants.grantee_id = ?
                 AND grants.revoked_at IS NULL
             )
           )
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&short_code)
    .bind(user_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(row) = row else {
        return Ok(Some(if english {
            format!("Task #{short_code} was not found or is not accessible.")
        } else {
            format!("未找到可访问的任务 #{short_code}。")
        }));
    };
    let task_id: String = row.get("id");
    let status: String = row.get("status");
    if intent == "detail" {
        return Ok(Some(if english {
            format!(
                "{} · #{}\nStatus: {}\nPhase: {}\nElapsed: {}\nProgress: {}\nLatest activity: {}\nUpdated: {}",
                row.get::<String, _>("title"),
                short_code,
                task_status_label_localized(&status, true),
                row.get::<String, _>("phase"),
                format_elapsed_seconds(row.get::<i64, _>("elapsed_seconds")),
                if row.get::<i32, _>("progress_percent") > 0 {
                    format!("{}%", row.get::<i32, _>("progress_percent"))
                } else {
                    "In progress".to_string()
                },
                row.get::<Option<String>, _>("last_event")
                    .unwrap_or_else(|| "No activity details".to_string()),
                row.get::<String, _>("updated_at"),
            )
        } else {
            format!(
                "{} · #{}\n状态：{}\n阶段：{}\n已耗时：{}\n进度：{}\n最近活动：{}\n更新时间：{}",
                row.get::<String, _>("title"),
                short_code,
                task_status_label(&status),
                row.get::<String, _>("phase"),
                format_elapsed_seconds(row.get::<i64, _>("elapsed_seconds")),
                if row.get::<i32, _>("progress_percent") > 0 {
                    format!("{}%", row.get::<i32, _>("progress_percent"))
                } else {
                    "进行中".to_string()
                },
                row.get::<Option<String>, _>("last_event")
                    .unwrap_or_else(|| "暂无活动说明".to_string()),
                row.get::<String, _>("updated_at"),
            )
        }));
    }
    let valid = match intent {
        "cancel" | "kill" => ACTIVE_STATUSES.contains(&status.as_str()),
        "retry" => matches!(
            status.as_str(),
            "failed" | "cancelled" | "timed_out" | "stale"
        ),
        "provide_input" => status == "waiting_input",
        "approve" | "reject" => status == "waiting_approval",
        _ => false,
    };
    if !valid {
        return Ok(Some(if english {
            format!(
                "Task #{short_code} is currently {} and cannot accept {}.",
                task_status_label_localized(&status, true),
                task_command_label_localized(intent, true),
            )
        } else {
            format!(
                "任务 #{short_code} 当前状态为“{}”，不能执行{}。",
                task_status_label(&status),
                task_command_label(intent)
            )
        }));
    }
    if !actor_can_control_task(state, tenant_id, &task_id, user_id).await? {
        return Ok(Some(if english {
            "This account cannot control the task, but it can still read status and results."
                .to_string()
        } else {
            "当前账号没有任务控制权限；你仍可查询任务状态和结果。".to_string()
        }));
    }
    let mut tx = state.control_db().begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let command_id = format!("agtcmd-{}", uuid::Uuid::new_v4());
    let command_input = (intent == "provide_input")
        .then(|| external_supplemental_input(text, &short_code))
        .flatten();
    if intent == "provide_input" && command_input.is_none() {
        return Ok(Some(if english {
            format!("Add the input after the task code, for example `input #{short_code} use the APAC definition`.")
        } else {
            format!("请在任务编号后补充内容，例如“补充 #{short_code} 使用华东区口径”。")
        }));
    }
    let inserted = sqlx::query::<sqlx::Sqlite>(
        "INSERT OR IGNORE INTO agent_task_command_requests
           (id, tenant_id, task_id, actor_user_id, actor_type, command_type, status,
            expected_state_version, idempotency_key, input_json)
         VALUES (?, ?, ?, ?, 'bot', ?, 'queued', ?, ?, ?)",
    )
    .bind(&command_id)
    .bind(tenant_id)
    .bind(&task_id)
    .bind(user_id)
    .bind(intent)
    .bind(row.get::<i64, _>("state_version"))
    .bind(idempotency_key)
    .bind(
        command_input
            .as_ref()
            .map(|value| json!({ "text": value }).to_string()),
    )
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(Some(if english {
            format!(
                "The {} request for task #{} has already been submitted.",
                task_command_label_localized(intent, true),
                short_code
            )
        } else {
            format!(
                "{}任务 #{} 的请求已经提交，请勿重复操作。",
                task_command_label(intent),
                short_code
            )
        }));
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks
         SET desired_state = CASE WHEN ? IN ('cancel', 'kill') THEN 'cancelled' ELSE desired_state END,
             state_version = state_version + 1, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(intent)
    .bind(tenant_id)
    .bind(&task_id)
    .execute(&mut *tx)
    .await?;
    insert_outbox_tx(
        &mut tx,
        tenant_id,
        &task_id,
        "task.command_requested",
        "owner",
        json!({
            "schemaVersion": 1,
            "actorType": "bot",
            "actorId": user_id,
            "commandId": command_id,
            "commandType": intent,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(if english {
        format!(
            "The {} request for task #{} was accepted. Status changes will continue to be delivered.",
            task_command_label_localized(intent, true),
            short_code
        )
    } else {
        format!(
            "已提交{}任务 #{} 的请求，状态变化会继续通知你。",
            task_command_label(intent),
            short_code
        )
    }))
}

fn task_command_label_localized(intent: &str, english: bool) -> &'static str {
    if !english {
        return task_command_label(intent);
    }
    match intent {
        "cancel" => "cancel",
        "kill" => "force-cancel",
        "retry" => "retry",
        "provide_input" => "input",
        "approve" => "approve",
        "reject" => "reject",
        _ => "control",
    }
}

fn task_command_label(intent: &str) -> &'static str {
    match intent {
        "cancel" => "取消",
        "kill" => "强制取消",
        "retry" => "重试",
        "provide_input" => "补充信息",
        "approve" => "批准",
        "reject" => "拒绝",
        _ => "控制",
    }
}

fn task_status_label(status: &str) -> &str {
    match status {
        "created" | "queued" | "claimed" => "排队中",
        "running" | "retrying" => "执行中",
        "waiting_input" => "等待补充信息",
        "waiting_approval" => "等待审批",
        "blocked" => "已阻塞",
        "cancelling" => "取消中",
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "timed_out" => "已超时",
        "stale" => "状态失联",
        _ => status,
    }
}

fn task_status_label_localized(status: &str, english: bool) -> &str {
    if !english {
        return task_status_label(status);
    }
    match status {
        "created" | "queued" | "claimed" => "queued",
        "running" | "retrying" => "running",
        "waiting_input" => "waiting for input",
        "waiting_approval" => "waiting for approval",
        "blocked" => "blocked",
        "cancelling" => "cancelling",
        "completed" => "completed",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "timed_out" => "timed out",
        "stale" => "stale",
        _ => status,
    }
}

fn format_elapsed_seconds(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let remainder = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {remainder}s")
    } else {
        format!("{remainder}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_test_state(db: sqlx::SqlitePool) -> AppState {
        AppState {
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

    async fn visible_task_ids(
        db: &sqlx::SqlitePool,
        tenant_id: &str,
        user_id: &str,
    ) -> Vec<String> {
        let mut sql = String::from("SELECT at.id FROM agent_tasks at WHERE at.tenant_id = ?");
        append_own_visibility(&mut sql);
        sql.push_str(" ORDER BY at.id");
        sqlx::query_scalar::<sqlx::Sqlite, String>(&sql)
            .bind(tenant_id)
            .bind(user_id)
            .bind(user_id)
            .bind(user_id)
            .fetch_all(db)
            .await
            .expect("query visible task ids")
    }

    #[test]
    fn short_codes_are_stable_and_user_friendly() {
        let first = short_code_for_task_id("agt-123");
        assert_eq!(first, short_code_for_task_id("agt-123"));
        assert_eq!(first.len(), 12);
        assert!(first.chars().all(|value| value.is_ascii_alphanumeric()));
        assert_ne!(first, short_code_for_task_id("agt-124"));
    }

    #[test]
    fn history_includes_legacy_archived_tasks_by_default() {
        let query = TaskListQuery {
            scope: None,
            status: None,
            bucket: Some("history".to_string()),
            capability_key: None,
            cursor: None,
            limit: None,
            include_archived: None,
            include_children: None,
        };
        assert!(task_list_includes_archived(&query));

        let active = TaskListQuery {
            bucket: Some("active".to_string()),
            ..query
        };
        assert!(!task_list_includes_archived(&active));
    }

    #[test]
    fn watchdog_feature_modes_are_strict_and_normalized() {
        assert_eq!(
            normalized_watchdog_feature_mode("watchdog_control_plane_v2", " ON ")
                .expect("control plane mode"),
            "on"
        );
        assert_eq!(
            normalized_watchdog_feature_mode("watchdog_external_identity", "Optional")
                .expect("identity mode"),
            "optional"
        );
        assert!(normalized_watchdog_feature_mode("watchdog_mobile_handoff", "shadow").is_err());
        assert!(normalized_watchdog_feature_mode("unknown_watchdog_feature", "on").is_err());
    }

    #[test]
    fn short_code_sample_has_no_collisions() {
        let mut codes = HashSet::new();
        for index in 0..10_000 {
            assert!(codes.insert(short_code_for_task_id(&format!("agt-sample-{index}"))));
        }
    }

    #[test]
    fn subscription_destination_keys_compare_the_complete_identifier() {
        let prefix = "x".repeat(220);
        let first = format!("{prefix}-conversation-a");
        let second = format!("{prefix}-conversation-b");
        assert_eq!(&first[..191], &second[..191]);
        assert_ne!(
            subscription_destination_key(Some(&first)),
            subscription_destination_key(Some(&second))
        );
        assert_eq!(subscription_destination_key(None).len(), 64);
    }

    #[test]
    fn upload_artifact_references_are_owner_scoped_and_path_safe() {
        assert_eq!(
            upload_artifact_filename("/api/v1/uploads/user-a/file-1.pdf", "user-a"),
            Some("file-1.pdf")
        );
        assert_eq!(
            upload_artifact_filename("/api/v1/uploads/user-b/file-1.pdf", "user-a"),
            None
        );
        assert_eq!(
            upload_artifact_filename("/api/v1/uploads/user-a/../secret", "user-a"),
            None
        );
        assert_eq!(
            upload_artifact_filename("/api/v1/uploads/user-a/%2e%2e%2fsecret", "user-a"),
            None
        );
    }

    #[test]
    fn bot_task_references_accept_new_and_legacy_short_codes() {
        assert_eq!(
            external_task_short_code("查看 #A1B2C3D4E5"),
            Some("A1B2C3D4E5".to_string())
        );
        assert_eq!(
            external_task_short_code("查看 #12ABCD34EFGH"),
            Some("12ABCD34EFGH".to_string())
        );
        assert_eq!(external_task_short_code("查看 #NOT-A-TASK"), None);
    }

    #[test]
    fn cursor_round_trip_is_lossless() {
        let task = TaskView {
            id: "task-1".to_string(),
            short_code: "ABC123".to_string(),
            root_task_id: "task-1".to_string(),
            parent_task_id: None,
            title: "title".to_string(),
            summary: None,
            capability_key: "ai_chat".to_string(),
            source: "webui".to_string(),
            source_label: None,
            status: "running".to_string(),
            phase: "model_calling".to_string(),
            state_version: 1,
            progress_percent: 0,
            progress: None,
            desired_state: None,
            last_event: None,
            error_code: None,
            error_message: None,
            result_summary: None,
            result_artifact_ref: None,
            sensitivity_label: "internal".to_string(),
            origin_session_id: None,
            origin_turn_id: None,
            linked_resource_type: None,
            linked_resource_id: None,
            external_platform: None,
            external_conversation_id: None,
            owner_user_id: None,
            initiator_user_id: None,
            assigned_user_id: None,
            last_progress_at: None,
            sla_due_at: None,
            budget: None,
            cost: None,
            archived: false,
            created_at: "2026-07-25 00:00:00.000".to_string(),
            updated_at: "2026-07-25 00:00:00.000".to_string(),
            started_at: None,
            completed_at: None,
            allowed_actions: vec![],
        };
        let encoded = encode_cursor(&task).expect("cursor");
        let decoded = decode_cursor(Some(&encoded))
            .expect("valid cursor")
            .expect("value");
        assert_eq!(decoded.id, task.id);
        assert_eq!(decoded.updated_at, task.updated_at);
    }

    #[test]
    fn task_actions_only_expose_real_control_adapters() {
        let unsupported = allowed_actions(
            "running",
            true,
            "super_assistant",
            Some("nl2sql_agent_query"),
            Some("query-1"),
            None,
        );
        assert!(!unsupported.iter().any(|action| action == "cancel"));

        let bot = allowed_actions(
            "running",
            true,
            "bot",
            Some("bot_inbound_message"),
            Some("log-1"),
            None,
        );
        assert!(bot.iter().any(|action| action == "cancel"));

        let retryable = allowed_actions(
            "failed",
            true,
            "nl2sql",
            Some("nl2sql_agent_query"),
            Some("query-1"),
            None,
        );
        assert!(retryable.iter().any(|action| action == "retry"));

        let not_retryable = allowed_actions(
            "failed",
            true,
            "pm",
            Some("pm_material_job"),
            Some("42"),
            None,
        );
        assert!(!not_retryable.iter().any(|action| action == "retry"));

        let non_bot_input = allowed_actions(
            "waiting_input",
            true,
            "super_assistant",
            Some("super_assistant_turn"),
            Some("turn-1"),
            Some("turn-1"),
        );
        assert!(!non_bot_input.iter().any(|action| action == "provide_input"));

        let bot_input = allowed_actions(
            "waiting_input",
            true,
            "bot",
            Some("bot_inbound_message"),
            Some("log-1"),
            None,
        );
        assert!(bot_input.iter().any(|action| action == "provide_input"));

        let unsupported_approval = allowed_actions(
            "waiting_approval",
            true,
            "super_assistant",
            Some("super_assistant_turn"),
            Some("turn-1"),
            Some("turn-1"),
        );
        assert!(!unsupported_approval
            .iter()
            .any(|action| action == "approve"));
    }

    #[test]
    fn pairing_hash_is_case_insensitive() {
        assert_eq!(pairing_hash("abcd1234"), pairing_hash("ABCD1234"));
        let code = pairing_code();
        assert_eq!(code.len(), 12);
        assert!(code.chars().all(|value| value.is_ascii_hexdigit()));
    }

    #[test]
    fn channel_scoped_pairing_rejects_a_different_bot_channel() {
        assert!(pairing_channel_matches(None, Some("channel-b")));
        assert!(pairing_channel_matches(
            Some("channel-a"),
            Some("channel-a")
        ));
        assert!(!pairing_channel_matches(
            Some("channel-a"),
            Some("channel-b")
        ));
        assert!(!pairing_channel_matches(Some("channel-a"), None));
    }

    #[test]
    fn bot_gateway_readers_can_manage_only_their_own_external_identity() {
        assert!(can_manage_own_external_identity(
            "member",
            &["bot_agents:read".to_string()],
            false,
        ));
        assert!(can_manage_own_external_identity("admin", &[], false));
        assert!(can_manage_own_external_identity("developer", &[], true));
        assert!(!can_manage_own_external_identity("developer", &[], false));
        assert!(can_manage_own_external_identity(
            "member",
            &["tasks:read".to_string()],
            false,
        ));
        assert!(!can_manage_own_external_identity(
            "member",
            &["skills:read".to_string()],
            false,
        ));
    }

    #[test]
    fn unsupported_commands_are_rejected() {
        assert_eq!(supported_command("cancel"), Some("cancel"));
        assert_eq!(supported_command("kill"), Some("kill"));
        assert_eq!(supported_command("DROP TABLE"), None);
    }

    #[test]
    fn command_idempotency_keys_cannot_cross_security_scope() {
        assert!(command_idempotency_scope_matches(
            "task-a", "user-a", "cancel", "task-a", "user-a", "cancel",
        ));
        assert!(!command_idempotency_scope_matches(
            "task-b", "user-a", "cancel", "task-a", "user-a", "cancel",
        ));
        assert!(!command_idempotency_scope_matches(
            "task-a", "user-b", "cancel", "task-a", "user-a", "cancel",
        ));
        assert!(!command_idempotency_scope_matches(
            "task-a", "user-a", "retry", "task-a", "user-a", "cancel",
        ));
    }

    #[test]
    fn task_command_fast_path_supports_safe_natural_references() {
        assert_eq!(external_task_intent("取消订阅怎么实现？"), None);
        assert_eq!(external_task_intent("cancel the current request"), None);
        assert_eq!(external_task_intent("停掉刚才那个研究任务"), Some("cancel"));
        assert_eq!(external_task_intent("今天失败的有哪些"), Some("list"));
        assert_eq!(
            external_task_intent("现在正在运行的任务有哪些"),
            Some("list")
        );
        assert_eq!(external_task_intent("什么进度了？"), Some("detail"));
        assert_eq!(
            external_task_intent("这个任务耗时多久了？执行到哪一个阶段了？"),
            Some("detail")
        );
        assert_eq!(external_task_intent("把正在执行的任务取消"), Some("cancel"));
        assert_eq!(external_task_intent("研发任务卡在哪里"), Some("detail"));
        assert_eq!(external_task_intent("取消 #A1B2C3D4E5"), Some("cancel"));
        assert_eq!(external_task_intent("/cancel latest"), Some("cancel"));
        assert_eq!(external_task_intent("/kill #A1B2C3D4E5"), Some("kill"));
        assert_eq!(external_task_intent("强制取消刚才的任务"), Some("kill"));
        assert_eq!(
            external_task_intent("/cancel-session"),
            Some("cancel_session")
        );
        assert_eq!(external_task_intent("retry #A1B2C3D4E5"), Some("retry"));
        assert_eq!(
            external_task_intent("补充 #A1B2C3D4E5 使用华东区口径"),
            Some("provide_input")
        );
        assert_eq!(external_task_intent("approve #A1B2C3D4E5"), Some("approve"));
        assert_eq!(external_task_intent("拒绝 #A1B2C3D4E5"), Some("reject"));
        assert_eq!(
            external_supplemental_input("补充 #A1B2C3D4E5 使用华东区口径", "A1B2C3D4E5").as_deref(),
            Some("使用华东区口径")
        );
        assert!(external_task_has_relative_reference("停掉刚才那个研究任务"));
        assert_eq!(
            external_task_reference_domain("研发任务卡在哪里"),
            Some("rd")
        );
        assert!(external_task_domain_matches(
            Some("rd"),
            "rd_agent",
            "修复登录问题"
        ));
        assert!(!external_task_domain_matches(
            Some("rd"),
            "nl2sql",
            "查询收入"
        ));
    }

    #[tokio::test]
    async fn natural_task_references_resolve_by_recency_domain_and_state() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let research_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let rd_task_id = format!("agt-{}", uuid::Uuid::new_v4());

        for (task_id, capability, title, status) in [
            (&research_task_id, "pm_assistant", "竞品研究", "running"),
            (&rd_task_id, "rd_agent", "研发登录流程", "blocked"),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, source, capability_key, title, status, phase,
                    owner_user_id, initiator_user_id, root_task_id, last_heartbeat_at)
                 VALUES (?, ?, ?, 'test', ?, ?, ?, 'executing', ?, ?, ?, CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(short_code_for_task_id(task_id))
            .bind(&tenant_id)
            .bind(capability)
            .bind(title)
            .bind(status)
            .bind(&user_id)
            .bind(&user_id)
            .bind(task_id)
            .execute(state.control_db())
            .await
            .expect("insert natural reference fixture");
        }

        assert_eq!(
            resolve_external_relative_task(
                &state,
                &tenant_id,
                &user_id,
                "停掉刚才那个研究任务",
                "cancel",
                None,
            )
            .await
            .expect("resolve research task"),
            ExternalTaskResolution::Resolved(short_code_for_task_id(&research_task_id))
        );
        assert_eq!(
            resolve_external_relative_task(
                &state,
                &tenant_id,
                &user_id,
                "研发任务卡在哪里",
                "detail",
                None,
            )
            .await
            .expect("resolve RD task"),
            ExternalTaskResolution::Resolved(short_code_for_task_id(&rd_task_id))
        );

        let failed_today_id = format!("agt-{}", uuid::Uuid::new_v4());
        let failed_yesterday_id = format!("agt-{}", uuid::Uuid::new_v4());
        let completed_today_id = format!("agt-{}", uuid::Uuid::new_v4());
        for (task_id, title, status) in [
            (&failed_today_id, "today failed task", "failed"),
            (&failed_yesterday_id, "old failed task", "failed"),
            (&completed_today_id, "today completed task", "completed"),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, source, capability_key, title, status, phase,
                    owner_user_id, initiator_user_id, root_task_id, last_heartbeat_at)
                 VALUES (?, ?, ?, 'test', 'ai_chat', ?, ?, 'finalizing', ?, ?, ?,
                         CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(short_code_for_task_id(task_id))
            .bind(&tenant_id)
            .bind(title)
            .bind(status)
            .bind(&user_id)
            .bind(&user_id)
            .bind(task_id)
            .execute(state.control_db())
            .await
            .expect("insert task list fixture");
        }
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks SET created_at = datetime(CURRENT_TIMESTAMP, '-2 days')
             WHERE id = ?",
        )
        .bind(&failed_today_id)
        .execute(state.control_db())
        .await
        .expect("backdate task that failed today");
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks
             SET created_at = datetime(CURRENT_TIMESTAMP, '-2 days'),
                 updated_at = datetime(CURRENT_TIMESTAMP, '-1 day'),
                 completed_at = datetime(CURRENT_TIMESTAMP, '-1 day')
             WHERE id = ?",
        )
        .bind(&failed_yesterday_id)
        .execute(state.control_db())
        .await
        .expect("backdate old failed fixture");
        let failed_today = handle_external_task_request(
            &state,
            &tenant_id,
            &user_id,
            "今天失败的有哪些",
            "natural-list-test",
            None,
        )
        .await
        .expect("list today's failed tasks")
        .expect("handled task list request");
        assert!(failed_today.contains("today failed task"));
        assert!(!failed_today.contains("old failed task"));
        assert!(!failed_today.contains("today completed task"));

        let active_tasks = handle_external_task_request(
            &state,
            &tenant_id,
            &user_id,
            "现在正在运行的任务有哪些",
            "natural-active-list-test",
            None,
        )
        .await
        .expect("list active tasks")
        .expect("handled active task list request");
        assert!(active_tasks.contains("竞品研究"));
        assert!(active_tasks.contains("研发登录流程"));
        assert!(!active_tasks.contains("today failed task"));
        assert!(!active_tasks.contains("today completed task"));

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ?")
            .bind(&tenant_id)
            .execute(state.control_db())
            .await
            .expect("delete natural reference fixtures");
        state.db.close().await;
    }

    #[tokio::test]
    async fn bot_task_resolution_is_unique_ambiguous_latest_and_conversation_scoped() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let beijing_id = format!("agt-{}", uuid::Uuid::new_v4());
        let shanghai_id = format!("agt-{}", uuid::Uuid::new_v4());
        let research_id = format!("agt-{}", uuid::Uuid::new_v4());

        for (task_id, title, capability, conversation_id, age_minutes) in [
            (
                &beijing_id,
                "北京天气预报",
                "ai_chat",
                "conversation-a",
                2_i64,
            ),
            (
                &research_id,
                "研发路线研究",
                "pm_assistant",
                "conversation-a",
                1_i64,
            ),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, source, capability_key, title, status, phase,
                    owner_user_id, initiator_user_id, root_task_id, external_conversation_id,
                    updated_at, last_heartbeat_at)
                 VALUES (?, ?, ?, 'bot', ?, ?, 'running', 'executing', ?, ?, ?, ?,
                         datetime(CURRENT_TIMESTAMP, ?), CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(short_code_for_task_id(task_id))
            .bind(&tenant_id)
            .bind(capability)
            .bind(title)
            .bind(&user_id)
            .bind(&user_id)
            .bind(task_id)
            .bind(conversation_id)
            .bind(format!("-{age_minutes} minutes"))
            .execute(state.control_db())
            .await
            .expect("insert Bot task-resolution fixture");
        }

        assert_eq!(
            resolve_external_relative_task(
                &state,
                &tenant_id,
                &user_id,
                "取消刚才天气预报那个任务",
                "cancel",
                Some("conversation-a"),
            )
            .await
            .expect("resolve unique weather title"),
            ExternalTaskResolution::Resolved(short_code_for_task_id(&beijing_id))
        );

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, external_conversation_id,
                updated_at, last_heartbeat_at)
             VALUES (?, ?, ?, 'bot', 'ai_chat', '上海天气提醒', 'running', 'executing',
                     ?, ?, ?, 'conversation-b', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&shanghai_id)
        .bind(short_code_for_task_id(&shanghai_id))
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&shanghai_id)
        .execute(state.control_db())
        .await
        .expect("insert second weather task");

        let ambiguous = resolve_external_relative_task(
            &state,
            &tenant_id,
            &user_id,
            "取消刚才天气任务",
            "cancel",
            None,
        )
        .await
        .expect("resolve ambiguous weather title");
        assert!(matches!(
            ambiguous,
            ExternalTaskResolution::Ambiguous(ref candidates) if candidates.len() == 2
        ));

        let active_scope = resolve_external_relative_task(
            &state,
            &tenant_id,
            &user_id,
            "把正在执行的任务取消",
            "cancel",
            None,
        )
        .await
        .expect("resolve active task scope");
        assert!(matches!(
            active_scope,
            ExternalTaskResolution::Ambiguous(ref candidates) if candidates.len() == 3
        ));

        assert_eq!(
            resolve_external_relative_task(
                &state,
                &tenant_id,
                &user_id,
                "什么进度了？",
                "detail",
                Some("conversation-b"),
            )
            .await
            .expect("resolve current conversation progress"),
            ExternalTaskResolution::Resolved(short_code_for_task_id(&shanghai_id))
        );

        let response = handle_external_task_request(
            &state,
            &tenant_id,
            &user_id,
            "取消刚才天气任务",
            "ambiguous-weather-cancel",
            Some("conversation-a"),
        )
        .await
        .expect("handle ambiguous cancellation")
        .expect("task request is handled");
        assert!(response.contains("未执行任何操作"));
        let command_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_command_requests
             WHERE tenant_id = ?",
        )
        .bind(&tenant_id)
        .fetch_one(state.control_db())
        .await
        .expect("count cancellation commands");
        assert_eq!(command_count, 0);

        assert_eq!(
            resolve_external_relative_task(
                &state,
                &tenant_id,
                &user_id,
                "/cancel latest",
                "cancel",
                None,
            )
            .await
            .expect("resolve latest active root"),
            ExternalTaskResolution::Resolved(short_code_for_task_id(&shanghai_id))
        );

        let conversation_tasks = handle_external_task_request(
            &state,
            &tenant_id,
            &user_id,
            "/cancel-session",
            "conversation-scope-list",
            Some("conversation-a"),
        )
        .await
        .expect("list current conversation tasks")
        .expect("session control request is handled");
        assert!(conversation_tasks.contains("北京天气预报"));
        assert!(conversation_tasks.contains("研发路线研究"));
        assert!(!conversation_tasks.contains("上海天气提醒"));

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ?")
            .bind(&tenant_id)
            .execute(state.control_db())
            .await
            .expect("delete Bot task-resolution fixtures");
        state.db.close().await;
    }

    #[test]
    fn summary_keeps_running_waiting_and_failed_buckets_separate() {
        assert_eq!(summary_membership("running"), (true, false, false));
        assert_eq!(summary_membership("waiting_input"), (false, true, false));
        assert_eq!(summary_membership("waiting_approval"), (false, true, false));
        assert_eq!(summary_membership("blocked"), (false, true, false));
        assert_eq!(summary_membership("failed"), (false, false, true));
        assert_eq!(summary_membership("completed"), (false, false, false));
    }

    #[test]
    fn admin_events_fail_closed_for_non_admin_users() {
        assert!(!event_visible_to(false, "admin"));
        assert!(event_visible_to(false, "owner"));
        assert!(event_visible_to(false, "team"));
        assert!(event_visible_to(true, "admin"));
    }

    #[test]
    fn control_permission_requires_task_read_access() {
        let control_only = vec!["tasks:control".to_string()];
        assert_eq!(
            task_permissions("viewer", &control_only),
            (false, false, false)
        );

        let read_and_control = vec!["tasks:read".to_string(), "tasks:control".to_string()];
        assert_eq!(
            task_permissions("viewer", &read_and_control),
            (true, false, true)
        );
        assert_eq!(
            task_permissions("developer", &["tasks:read".to_string()]),
            (true, false, false)
        );
        assert_eq!(task_permissions("admin", &[]), (true, true, true));
    }

    #[tokio::test]
    async fn sqlite_task_visibility_and_revocation_are_isolated() {
        let db = crate::test_sqlite_pool().await;
        let tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let other_tenant = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_a = format!("user-{}", uuid::Uuid::new_v4());
        let user_b = format!("user-{}", uuid::Uuid::new_v4());
        let task_a = format!("agt-{}", uuid::Uuid::new_v4());
        let task_b = format!("agt-{}", uuid::Uuid::new_v4());
        let task_c = format!("agt-{}", uuid::Uuid::new_v4());

        for (task_id, task_tenant, owner, title) in [
            (&task_a, &tenant, &user_a, "A private task"),
            (&task_b, &tenant, &user_b, "B private task"),
            (&task_c, &other_tenant, &user_a, "other tenant task"),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, capability_key, title, owner_user_id,
                    initiator_user_id, root_task_id, last_heartbeat_at)
                 VALUES (?, ?, ?, 'test', ?, ?, ?, ?, CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(short_code_for_task_id(task_id))
            .bind(task_tenant)
            .bind(title)
            .bind(owner)
            .bind(owner)
            .bind(task_id)
            .execute(&db)
            .await
            .expect("insert task fixture");
        }

        assert_eq!(
            visible_task_ids(&db, &tenant, &user_a).await,
            vec![task_a.clone()]
        );
        assert_eq!(
            visible_task_ids(&db, &tenant, &user_b).await,
            vec![task_b.clone()]
        );

        let grant_id = format!("agtgrant-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_grants
               (id, tenant_id, task_id, grantee_type, grantee_id, permission, granted_by)
             VALUES (?, ?, ?, 'user', ?, 'read', ?)",
        )
        .bind(&grant_id)
        .bind(&tenant)
        .bind(&task_b)
        .bind(&user_a)
        .bind(&user_b)
        .execute(&db)
        .await
        .expect("grant B task to A");
        let mut shared_expected = vec![task_a.clone(), task_b.clone()];
        shared_expected.sort();
        assert_eq!(
            visible_task_ids(&db, &tenant, &user_a).await,
            shared_expected
        );

        let task_b_child = format!("agt-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, capability_key, title, owner_user_id,
                initiator_user_id, parent_task_id, root_task_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'test', 'B retry child', ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_b_child)
        .bind(short_code_for_task_id(&task_b_child))
        .bind(&tenant)
        .bind(&user_b)
        .bind(&user_b)
        .bind(&task_b)
        .bind(&task_b)
        .execute(&db)
        .await
        .expect("insert shared root child task");
        let mut shared_with_child = vec![task_a.clone(), task_b.clone(), task_b_child.clone()];
        shared_with_child.sort();
        assert_eq!(
            visible_task_ids(&db, &tenant, &user_a).await,
            shared_with_child,
            "a root grant must authorize the root task and its child execution units"
        );
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_outbox
               (event_id, tenant_id, task_id, root_task_id, event_type, state_version,
                visibility, payload_json)
             VALUES (?, ?, ?, ?, 'task.progress', 1, 'owner', JSON_OBJECT())",
        )
        .bind(format!("agtevt-{}", uuid::Uuid::new_v4()))
        .bind(&tenant)
        .bind(&task_b_child)
        .bind(&task_b)
        .execute(&db)
        .await
        .expect("insert child outbox event");
        let mut shared_event_sql = String::from(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_task_outbox outbox
             INNER JOIN agent_tasks at
               ON at.tenant_id = outbox.tenant_id AND at.id = outbox.root_task_id
             WHERE outbox.tenant_id = ? AND outbox.task_id = ?",
        );
        append_own_visibility(&mut shared_event_sql);
        let visible_before_revoke: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(&shared_event_sql)
            .bind(&tenant)
            .bind(&task_b_child)
            .bind(&user_a)
            .bind(&user_a)
            .bind(&user_a)
            .fetch_one(&db)
            .await
            .expect("shared child event visibility");
        assert_eq!(visible_before_revoke, 1);

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_grants SET revoked_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&grant_id)
        .execute(&db)
        .await
        .expect("revoke task grant");
        let visible_after_revoke: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(&shared_event_sql)
            .bind(&tenant)
            .bind(&task_b_child)
            .bind(&user_a)
            .bind(&user_a)
            .bind(&user_a)
            .fetch_one(&db)
            .await
            .expect("revoked child event visibility");
        assert_eq!(visible_after_revoke, 0);
        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE id = ?")
            .bind(&task_b_child)
            .execute(&db)
            .await
            .expect("clean shared child fixture");
        assert_eq!(
            visible_task_ids(&db, &tenant, &user_a).await,
            vec![task_a.clone()]
        );

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE id IN (?, ?, ?)")
            .bind(&task_a)
            .bind(&task_b)
            .bind(&task_c)
            .execute(&db)
            .await
            .expect("clean task fixtures");
    }

    #[tokio::test]
    async fn sqlite_private_task_commands_and_subscriptions_are_owner_isolated() {
        let db = crate::test_sqlite_pool().await;
        let state = sqlite_test_state(db);
        let tenant_id = uuid::Uuid::new_v4().to_string();
        let user_a = uuid::Uuid::new_v4().to_string();
        let user_b = uuid::Uuid::new_v4().to_string();
        let root_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let child_task_id = format!("agt-{}", uuid::Uuid::new_v4());

        for (user_id, label) in [(&user_a, "a"), (&user_b, "b")] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO users
                   (id, email, name, password_hash, role, tenant_id, is_active,
                    menu_permissions_json)
                 VALUES (?, ?, ?, 'not-used', 'developer', ?, 1,
                         JSON_ARRAY('tasks:read','tasks:control'))",
            )
            .bind(user_id)
            .bind(format!(
                "watchdog-{label}-{}@example.invalid",
                uuid::Uuid::new_v4()
            ))
            .bind(format!("WatchDog {label}"))
            .bind(&tenant_id)
            .execute(state.control_db())
            .await
            .expect("insert task-control user fixture");
        }
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, root_task_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'test', 'test', 'A private root', 'running', 'executing',
                     ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&root_task_id)
        .bind(short_code_for_task_id(&root_task_id))
        .bind(&tenant_id)
        .bind(&user_a)
        .bind(&user_a)
        .bind(&root_task_id)
        .execute(state.control_db())
        .await
        .expect("insert private root task");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, capability_key, title, status, phase,
                owner_user_id, initiator_user_id, parent_task_id, root_task_id,
                origin_turn_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'test', 'test', 'A private child', 'running', 'executing',
                     ?, ?, ?, ?, 'turn-isolation-test', CURRENT_TIMESTAMP)",
        )
        .bind(&child_task_id)
        .bind(short_code_for_task_id(&child_task_id))
        .bind(&tenant_id)
        .bind(&user_a)
        .bind(&user_a)
        .bind(&root_task_id)
        .bind(&root_task_id)
        .execute(state.control_db())
        .await
        .expect("insert private child task");

        assert!(
            !actor_can_read_task(&state, &tenant_id, &child_task_id, &user_b)
                .await
                .expect("check private task read access")
        );
        assert!(
            !actor_can_control_task(&state, &tenant_id, &child_task_id, &user_b)
                .await
                .expect("check private task control access")
        );
        let claims_b = Claims::new(
            &user_b,
            "watchdog-b@example.invalid",
            "developer",
            &tenant_id,
        );
        let command_error = create_command(
            State(state.clone()),
            Extension(claims_b.clone()),
            Path(child_task_id.clone()),
            Json(CommandRequest {
                command_type: "cancel".to_string(),
                expected_state_version: Some(0),
                idempotency_key: Some(format!("isolation-{}", uuid::Uuid::new_v4())),
                input: None,
            }),
        )
        .await
        .expect_err("B must not enqueue a command for A's private task");
        assert!(matches!(command_error, AppError::NotFound(_)));
        let subscription_error = create_subscription(
            State(state.clone()),
            Extension(claims_b.clone()),
            Path(child_task_id.clone()),
            Json(SubscriptionRequest {
                event_types: vec!["task.completed".to_string()],
                destination_type: "webui".to_string(),
                destination_ref: None,
                policy: None,
            }),
        )
        .await
        .expect_err("B must not subscribe to A's private task");
        assert!(matches!(subscription_error, AppError::NotFound(_)));

        let grant_id = format!("agtgrant-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_task_grants
               (id, tenant_id, task_id, grantee_type, grantee_id, permission, granted_by)
             VALUES (?, ?, ?, 'user', ?, 'control', ?)",
        )
        .bind(&grant_id)
        .bind(&tenant_id)
        .bind(&root_task_id)
        .bind(&user_b)
        .bind(&user_a)
        .execute(state.control_db())
        .await
        .expect("grant root task control to B");
        assert!(
            actor_can_control_task(&state, &tenant_id, &child_task_id, &user_b)
                .await
                .expect("root grant must authorize child control")
        );
        let Json(shared_child) = task_detail(
            State(state.clone()),
            Extension(claims_b.clone()),
            Path(child_task_id.clone()),
        )
        .await
        .expect("root control grant must apply in child task detail");
        assert!(shared_child
            .allowed_actions
            .iter()
            .any(|action| action == "cancel"));
        let _ = create_subscription(
            State(state.clone()),
            Extension(claims_b.clone()),
            Path(child_task_id.clone()),
            Json(SubscriptionRequest {
                event_types: vec!["task.completed".to_string()],
                destination_type: "webui".to_string(),
                destination_ref: None,
                policy: None,
            }),
        )
        .await
        .expect("explicit root grant permits child subscription");

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_task_grants SET revoked_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&grant_id)
        .execute(state.control_db())
        .await
        .expect("revoke root task grant");
        assert!(
            !actor_can_read_task(&state, &tenant_id, &child_task_id, &user_b)
                .await
                .expect("revoked root grant removes child read access")
        );
        let revoked_subscription_error = list_subscriptions(
            State(state.clone()),
            Extension(claims_b),
            Path(child_task_id.clone()),
        )
        .await
        .expect_err("revoked user must not list prior subscriptions");
        assert!(matches!(revoked_subscription_error, AppError::NotFound(_)));

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id IN (?, ?)")
            .bind(&tenant_id)
            .bind(&child_task_id)
            .bind(&root_task_id)
            .execute(state.control_db())
            .await
            .expect("clean isolated task fixtures");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM users WHERE tenant_id = ? AND id IN (?, ?)")
            .bind(&tenant_id)
            .bind(&user_a)
            .bind(&user_b)
            .execute(state.control_db())
            .await
            .expect("clean task-control user fixtures");
        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_watchdog_baseline_is_complete() {
        let db = crate::test_sqlite_pool().await;
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name NOT LIKE '_sqlx_%'",
        )
        .fetch_one(&db)
        .await
        .expect("count SQLite baseline tables");
        assert_eq!(table_count, 164);

        let migration_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&db)
            .await
            .expect("read SQLx migration ledger");
        assert_eq!(migration_count, 16);

        let important_indexes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name IN (
                'idx__agent_task_outbox__idx_agent_outbox_ready_v2',
                'idx__agent_task_command_requests__uk_agent_task_active_retry',
                'idx__agent_watch_rule_runs__uk_agent_watch_rule_event'
            )",
        )
        .fetch_one(&db)
        .await
        .expect("verify WatchDog indexes");
        assert_eq!(important_indexes, 3);

        let foreign_key_violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&db)
            .await
            .expect("run SQLite foreign key check");
        assert!(foreign_key_violations.is_empty());
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&db)
            .await
            .expect("run SQLite integrity check");
        assert_eq!(integrity, "ok");
    }
}
