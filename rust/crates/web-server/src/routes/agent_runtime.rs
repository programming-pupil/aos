//! Agent Runtime local-process session registry.

#![cfg_attr(not(feature = "rd"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use axum::{
    extract::{Extension, Path as AxumPath, Query, State},
    routing::{get as routing_get, post as routing_post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use tokio::{
    fs,
    io::AsyncReadExt,
    time::{timeout, Duration},
};

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::PaginationParams;
use crate::state::AppState;

pub const RUNTIME_STATUS_CANCELLING: &str = "cancelling";
pub const RUNTIME_PROCESS_STATUS_CANCELLED: &str = "cancelled";
pub const RUNTIME_PROCESS_STATUS_COMPLETED: &str = "completed";
pub const RUNTIME_PROCESS_STATUS_FAILED: &str = "failed";
pub const RUNTIME_PROCESS_STATUS_TIMED_OUT: &str = "timed_out";

const RUNTIME_SESSION_SELECT: &str = r"
id, tenant_id, user_id, agent_task_id, capability_key, workspace_root, status,
isolation_mode, pid, process_group_id, cancel_requested, heartbeat_at, started_at,
completed_at, created_at, updated_at
";

const RUNTIME_PROCESS_SELECT: &str = r"
id, tenant_id, runtime_session_id, agent_task_id, command, cwd,
CAST(env_redacted_json AS TEXT) AS env_redacted_json, status, pid, process_group_id,
exit_code, stdout_preview, stderr_preview, stdout_artifact_id, stderr_artifact_id,
started_at, completed_at, created_at, updated_at
";

const RUNTIME_ARTIFACT_SELECT: &str = r"
id, tenant_id, runtime_session_id, agent_task_id, artifact_type, path, content_text,
content_hash, size_bytes, created_at
";

const ARTIFACT_DETAIL_MAX_BYTES: u64 = 200_000;
const RUNTIME_ISOLATION_LOCAL_PROCESS: &str = "local_process";
const RUNTIME_ISOLATION_DOCKER_SANDBOX: &str = "docker_sandbox";

#[derive(Debug, Clone)]
pub struct RuntimeSessionCreateInput {
    pub tenant_id: String,
    pub user_id: String,
    pub agent_task_id: Option<String>,
    pub capability_key: String,
    pub isolation_mode: Option<String>,
    pub workspace_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeCommandInput {
    pub tenant_id: String,
    pub runtime_session_id: String,
    pub agent_task_id: Option<String>,
    pub command: String,
    pub cwd: PathBuf,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct RuntimeCommandResult {
    pub process_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout_text: String,
    pub stderr_text: String,
}

#[derive(Debug, Clone)]
pub struct RuntimeArtifactWriteInput {
    pub tenant_id: String,
    pub runtime_session_id: String,
    pub agent_task_id: Option<String>,
    pub artifact_type: String,
    pub relative_path: String,
    pub content: Vec<u8>,
    pub content_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeSessionInfo {
    pub id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub agent_task_id: Option<String>,
    pub capability_key: String,
    pub workspace_root: String,
    pub status: String,
    pub isolation_mode: String,
    pub pid: Option<i64>,
    pub process_group_id: Option<i64>,
    pub cancel_requested: bool,
    pub heartbeat_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProcessInfo {
    pub id: String,
    pub tenant_id: String,
    pub runtime_session_id: String,
    pub agent_task_id: Option<String>,
    pub command: String,
    pub cwd: String,
    pub env_redacted_json: Option<Value>,
    pub status: String,
    pub pid: Option<i64>,
    pub process_group_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub stdout_preview: Option<String>,
    pub stderr_preview: Option<String>,
    pub stdout_artifact_id: Option<String>,
    pub stderr_artifact_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeArtifactInfo {
    pub id: String,
    pub tenant_id: String,
    pub runtime_session_id: String,
    pub agent_task_id: Option<String>,
    pub artifact_type: String,
    pub path: Option<String>,
    pub content_text: Option<String>,
    pub content_hash: Option<String>,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeArtifactDetail {
    #[serde(flatten)]
    pub artifact: AgentRuntimeArtifactInfo,
    pub content: Option<String>,
    pub content_truncated: bool,
    pub read_source: String,
}

#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}

#[derive(Debug, Deserialize, Default)]
pub struct SessionListQuery {
    pub status: Option<String>,
    pub capability_key: Option<String>,
    pub agent_task_id: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RuntimeRecoverRequest {
    pub timeout_secs: Option<u64>,
}

pub fn routes(state: AppState) -> Router<AppState> {
    let auth_state = state.clone();
    Router::new()
        .route("/sessions", routing_get(list_sessions))
        .route("/sessions/recover", routing_post(recover_sessions))
        .route("/sessions/{id}", routing_get(get_session))
        .route("/sessions/{id}/processes", routing_get(list_processes))
        .route("/sessions/{id}/artifacts", routing_get(list_artifacts))
        .route(
            "/sessions/{id}/artifacts/{artifact_id}",
            routing_get(get_artifact),
        )
        .route("/sessions/{id}/cancel", routing_post(cancel_session))
        .layer(axum::middleware::from_fn_with_state(
            auth_state,
            crate::auth_middleware::require_auth,
        ))
        .with_state(state)
}

pub async fn create_runtime_session(
    state: &AppState,
    input: RuntimeSessionCreateInput,
) -> Result<AgentRuntimeSessionInfo> {
    let id = uuid::Uuid::new_v4().to_string();
    let isolation_mode = runtime_isolation_mode(input.isolation_mode.as_deref())?;
    let workspace_root = build_workspace_root(state, &input)?;
    fs::create_dir_all(workspace_root.join("workspace/repo")).await?;
    fs::create_dir_all(workspace_root.join("workspace/.aos/commands")).await?;
    fs::create_dir_all(workspace_root.join("workspace/.aos/artifacts")).await?;
    fs::create_dir_all(workspace_root.join("workspace/.aos/traces")).await?;
    let workspace_root = workspace_root.to_string_lossy().to_string();
    sqlx::query(
        r"
        INSERT INTO agent_runtime_sessions
          (id, tenant_id, user_id, agent_task_id, capability_key, workspace_root, status,
           isolation_mode, heartbeat_at, started_at)
        VALUES (?, ?, ?, ?, ?, ?, 'running', ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
        ",
    )
    .bind(&id)
    .bind(&input.tenant_id)
    .bind(&input.user_id)
    .bind(&input.agent_task_id)
    .bind(&input.capability_key)
    .bind(&workspace_root)
    .bind(&isolation_mode)
    .execute(&state.db)
    .await?;
    add_runtime_event(
        state,
        &input.tenant_id,
        input.agent_task_id.as_deref(),
        Some(&id),
        None,
        "runtime.session.created",
        "runtime session 已创建",
        Some(json!({
            "runtimeSessionId": id,
            "workspaceRoot": workspace_root,
            "capabilityKey": input.capability_key,
            "isolationMode": isolation_mode
        })),
    )
    .await?;
    get_runtime_session(&state.db, &input.tenant_id, &id).await
}

pub async fn request_cancel_runtime_session(
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
) -> Result<()> {
    sqlx::query(
        r"
        UPDATE agent_runtime_sessions
        SET cancel_requested = 1,
            status = CASE
                WHEN status IN ('completed','failed','cancelled') THEN status
                ELSE 'cancelling'
            END,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(session_id)
    .execute(&state.db)
    .await?;
    sqlx::query(
        r"
        UPDATE agent_runtime_processes
        SET stderr_preview = CASE
                WHEN status IN ('queued','running','cancelling')
                THEN COALESCE(stderr_preview, 'runtime cancellation requested')
                ELSE stderr_preview
            END,
            completed_at = CASE WHEN status = 'queued' THEN COALESCE(completed_at, CURRENT_TIMESTAMP) ELSE completed_at END,
            status = CASE
                WHEN status = 'queued' THEN 'cancelled'
                WHEN status IN ('running','cancelling') THEN 'cancelling'
                ELSE status
            END,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND runtime_session_id = ? AND status IN ('queued','running','cancelling')
        ",
    )
    .bind(tenant_id)
    .bind(session_id)
    .execute(&state.db)
    .await?;
    let _ = kill_runtime_session_process_group(&state.db, tenant_id, session_id).await;
    if let Some(row) = sqlx::query(
        "SELECT agent_task_id FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(session_id)
    .fetch_optional(&state.db)
    .await?
    {
        let task_id: Option<String> = row.get("agent_task_id");
        add_runtime_event(
            state,
            tenant_id,
            task_id.as_deref(),
            Some(session_id),
            None,
            "runtime.session.cancel_requested",
            "runtime session 已请求取消",
            Some(json!({ "runtimeSessionId": session_id })),
        )
        .await?;
    }
    Ok(())
}

pub async fn complete_runtime_session(
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
    status: &str,
) -> Result<()> {
    let normalized = match status {
        "completed" | "failed" | "cancelled" => status,
        _ => "completed",
    };
    sqlx::query(
        r"
        UPDATE agent_runtime_sessions
        SET status = ?, completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(normalized)
    .bind(tenant_id)
    .bind(session_id)
    .execute(&state.db)
    .await?;
    Ok(())
}

pub async fn run_runtime_command(
    state: &AppState,
    input: RuntimeCommandInput,
) -> Result<RuntimeCommandResult> {
    let session =
        get_runtime_session(&state.db, &input.tenant_id, &input.runtime_session_id).await?;
    let agent_task_id = input
        .agent_task_id
        .clone()
        .or_else(|| session.agent_task_id.clone());
    let workspace_root = PathBuf::from(&session.workspace_root);
    ensure_workspace_child_safe(&workspace_root, &input.cwd)?;
    if session.cancel_requested {
        return Err(AppError::ValidationError(
            "runtime session is cancelling; command was not started".to_string(),
        ));
    }

    let process_id = uuid::Uuid::new_v4().to_string();
    let launch_plan = runtime_launch_plan(
        &session,
        &input.cwd,
        &input.command,
        &process_id,
        input.timeout_secs,
    )?;
    sqlx::query(
        r"
        INSERT INTO agent_runtime_processes
          (id, tenant_id, runtime_session_id, agent_task_id, command, cwd, env_redacted_json,
           status, started_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, 'running', CURRENT_TIMESTAMP)
        ",
    )
    .bind(&process_id)
    .bind(&input.tenant_id)
    .bind(&input.runtime_session_id)
    .bind(&agent_task_id)
    .bind(&input.command)
    .bind(input.cwd.to_string_lossy().to_string())
    .bind(launch_plan.env_redacted_json.clone())
    .execute(&state.db)
    .await?;
    heartbeat_runtime_session(&state.db, &input.tenant_id, &input.runtime_session_id).await?;
    add_runtime_event(
        state,
        &input.tenant_id,
        agent_task_id.as_deref(),
        Some(&input.runtime_session_id),
        Some(&process_id),
        "runtime.command.started",
        "runtime command 已开始执行",
        Some(json!({
            "runtimeSessionId": input.runtime_session_id,
            "processId": process_id,
            "command": input.command,
            "cwd": input.cwd.to_string_lossy(),
            "isolationMode": session.isolation_mode,
            "launchProgram": launch_plan.program,
            "timeoutSecs": input.timeout_secs
        })),
    )
    .await?;

    let mut command = tokio::process::Command::new(&launch_plan.program);
    command
        .args(&launch_plan.args)
        .current_dir(&launch_plan.host_cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|error| AppError::Internal(format!("runtime command spawn failed: {error}")))?;
    let pid = child.id().map(i64::from);
    sqlx::query(
        r"
        UPDATE agent_runtime_processes
        SET pid = ?, process_group_id = ?, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(pid)
    .bind(pid)
    .bind(&input.tenant_id)
    .bind(&process_id)
    .execute(&state.db)
    .await?;
    sqlx::query(
        r"
        UPDATE agent_runtime_sessions
        SET status = 'running', pid = COALESCE(pid, ?), process_group_id = COALESCE(process_group_id, ?),
            heartbeat_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(pid)
    .bind(pid)
    .bind(&input.tenant_id)
    .bind(&input.runtime_session_id)
    .execute(&state.db)
    .await?;

    let output_result = timeout(
        Duration::from_secs(input.timeout_secs),
        child.wait_with_output(),
    )
    .await;
    let (mut status, exit_code, stdout_text, mut stderr_text) = match output_result {
        Ok(Ok(output)) => {
            let status = if output.status.success() {
                RUNTIME_PROCESS_STATUS_COMPLETED
            } else {
                RUNTIME_PROCESS_STATUS_FAILED
            };
            (
                status.to_string(),
                output.status.code(),
                preview_text(String::from_utf8_lossy(&output.stdout).as_ref(), 60_000),
                preview_text(String::from_utf8_lossy(&output.stderr).as_ref(), 60_000),
            )
        }
        Ok(Err(error)) => (
            RUNTIME_PROCESS_STATUS_FAILED.to_string(),
            None,
            String::new(),
            error.to_string(),
        ),
        Err(_) => {
            let _ = kill_runtime_session_process_group(
                &state.db,
                &input.tenant_id,
                &input.runtime_session_id,
            )
            .await;
            (
                RUNTIME_PROCESS_STATUS_TIMED_OUT.to_string(),
                None,
                String::new(),
                format!("runtime command timed out after {}s", input.timeout_secs),
            )
        }
    };
    if runtime_command_cancel_was_requested(
        &state.db,
        &input.tenant_id,
        &input.runtime_session_id,
        &process_id,
    )
    .await?
    {
        status = RUNTIME_PROCESS_STATUS_CANCELLED.to_string();
        if stderr_text.trim().is_empty() {
            stderr_text = "runtime command was cancelled".to_string();
        } else if !stderr_text.contains("runtime command was cancelled") {
            stderr_text.push_str("\nruntime command was cancelled");
        }
    }
    let stdout_preview = preview_text(&stdout_text, 8_000);
    let stderr_preview = preview_text(&stderr_text, 8_000);
    let stdout_artifact_id = if stdout_text.is_empty() {
        None
    } else {
        Some(
            write_runtime_text_artifact(
                state,
                &input.tenant_id,
                &input.runtime_session_id,
                agent_task_id.as_deref(),
                "stdout",
                &process_id,
                &stdout_text,
            )
            .await?,
        )
    };
    let stderr_artifact_id = if stderr_text.is_empty() {
        None
    } else {
        Some(
            write_runtime_text_artifact(
                state,
                &input.tenant_id,
                &input.runtime_session_id,
                agent_task_id.as_deref(),
                "stderr",
                &process_id,
                &stderr_text,
            )
            .await?,
        )
    };
    sqlx::query(
        r"
        UPDATE agent_runtime_processes
        SET status = ?, exit_code = ?, stdout_preview = ?, stderr_preview = ?,
            stdout_artifact_id = ?, stderr_artifact_id = ?,
            completed_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(&status)
    .bind(exit_code)
    .bind(&stdout_preview)
    .bind(&stderr_preview)
    .bind(&stdout_artifact_id)
    .bind(&stderr_artifact_id)
    .bind(&input.tenant_id)
    .bind(&process_id)
    .execute(&state.db)
    .await?;
    heartbeat_runtime_session(&state.db, &input.tenant_id, &input.runtime_session_id).await?;
    add_runtime_event(
        state,
        &input.tenant_id,
        agent_task_id.as_deref(),
        Some(&input.runtime_session_id),
        Some(&process_id),
        match status.as_str() {
            RUNTIME_PROCESS_STATUS_COMPLETED => "runtime.command.completed",
            RUNTIME_PROCESS_STATUS_CANCELLED => "runtime.command.cancelled",
            _ => "runtime.command.failed",
        },
        "runtime command 已结束",
        Some(json!({
            "runtimeSessionId": input.runtime_session_id,
            "processId": process_id,
            "status": status,
            "exitCode": exit_code,
            "stdoutPreview": stdout_preview,
            "stderrPreview": stderr_preview,
            "stdoutArtifactId": stdout_artifact_id,
            "stderrArtifactId": stderr_artifact_id
        })),
    )
    .await?;
    Ok(RuntimeCommandResult {
        process_id,
        status,
        exit_code,
        stdout_text,
        stderr_text,
    })
}

pub async fn write_runtime_artifact(
    state: &AppState,
    input: RuntimeArtifactWriteInput,
) -> Result<String> {
    let artifact_id = uuid::Uuid::new_v4().to_string();
    let runtime = sqlx::query(
        "SELECT workspace_root, agent_task_id FROM agent_runtime_sessions
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&input.tenant_id)
    .bind(&input.runtime_session_id)
    .fetch_one(&state.db)
    .await?;
    let workspace_root: String = runtime.get("workspace_root");
    let agent_task_id = input
        .agent_task_id
        .clone()
        .or_else(|| runtime.get::<Option<String>, _>("agent_task_id"));
    let relative_path = normalize_runtime_artifact_path(&input.relative_path)?;
    let absolute_path = PathBuf::from(&workspace_root).join(&relative_path);
    ensure_workspace_child_safe(Path::new(&workspace_root), &absolute_path)?;
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&absolute_path, &input.content).await?;
    let mut hasher = Sha256::new();
    hasher.update(&input.content);
    let content_hash = hex::encode(hasher.finalize());
    let size_bytes = i64::try_from(input.content.len()).unwrap_or(i64::MAX);
    let content_text = input
        .content_text_preview
        .as_deref()
        .map(|value| preview_text(value, 120_000));
    sqlx::query(
        r"
        INSERT INTO agent_runtime_artifacts
            (id, tenant_id, runtime_session_id, agent_task_id, artifact_type, path,
             content_text, content_hash, size_bytes)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&artifact_id)
    .bind(&input.tenant_id)
    .bind(&input.runtime_session_id)
    .bind(&agent_task_id)
    .bind(&input.artifact_type)
    .bind(relative_path)
    .bind(content_text)
    .bind(content_hash)
    .bind(size_bytes)
    .execute(&state.db)
    .await?;
    add_runtime_event(
        state,
        &input.tenant_id,
        agent_task_id.as_deref(),
        Some(&input.runtime_session_id),
        None,
        "runtime.artifact.created",
        "runtime artifact 已创建",
        Some(json!({
            "runtimeSessionId": input.runtime_session_id,
            "artifactId": artifact_id,
            "artifactType": input.artifact_type,
            "sizeBytes": size_bytes
        })),
    )
    .await?;
    Ok(artifact_id)
}

async fn list_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<ListResponse<AgentRuntimeSessionInfo>>> {
    require_runtime_read(&claims)?;
    let pagination = PaginationParams {
        page: query.page,
        per_page: query.per_page,
    };
    let mut where_sql = String::from(" WHERE tenant_id = ?");
    if query
        .status
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        where_sql.push_str(" AND status = ?");
    }
    if query
        .capability_key
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        where_sql.push_str(" AND capability_key = ?");
    }
    if query
        .agent_task_id
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        where_sql.push_str(" AND agent_task_id = ?");
    }
    let count_sql =
        format!("SELECT CAST(COUNT(*) AS INTEGER) FROM agent_runtime_sessions{where_sql}");
    let list_sql = format!(
        "SELECT {RUNTIME_SESSION_SELECT} FROM agent_runtime_sessions{where_sql} ORDER BY updated_at DESC LIMIT ? OFFSET ?"
    );
    let mut count =
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_sql)).bind(&claims.tenant_id);
    let mut list = sqlx::query(sqlx::AssertSqlSafe(list_sql)).bind(&claims.tenant_id);
    if let Some(value) = query
        .status
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count = count.bind(value);
        list = list.bind(value);
    }
    if let Some(value) = query
        .capability_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count = count.bind(value);
        list = list.bind(value);
    }
    if let Some(value) = query
        .agent_task_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        count = count.bind(value);
        list = list.bind(value);
    }
    let total = count.fetch_one(&state.db).await?;
    let rows = list
        .bind(pagination.limit())
        .bind(pagination.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(session_from_row).collect(),
        total,
    }))
}

async fn get_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<AgentRuntimeSessionInfo>> {
    require_runtime_read(&claims)?;
    Ok(Json(
        get_runtime_session(&state.db, &claims.tenant_id, &id).await?,
    ))
}

async fn list_processes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<ListResponse<AgentRuntimeProcessInfo>>> {
    require_runtime_read(&claims)?;
    ensure_runtime_session_exists(&state.db, &claims.tenant_id, &id).await?;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_runtime_processes WHERE tenant_id = ? AND runtime_session_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {RUNTIME_PROCESS_SELECT} FROM agent_runtime_processes WHERE tenant_id = ? AND runtime_session_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )))
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(pagination.limit())
    .bind(pagination.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(process_from_row).collect(),
        total,
    }))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<ListResponse<AgentRuntimeArtifactInfo>>> {
    require_runtime_read(&claims)?;
    ensure_runtime_session_exists(&state.db, &claims.tenant_id, &id).await?;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_runtime_artifacts WHERE tenant_id = ? AND runtime_session_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {RUNTIME_ARTIFACT_SELECT} FROM agent_runtime_artifacts WHERE tenant_id = ? AND runtime_session_id = ? ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )))
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(pagination.limit())
    .bind(pagination.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(artifact_from_row).collect(),
        total,
    }))
}

async fn get_artifact(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath((id, artifact_id)): AxumPath<(String, String)>,
) -> Result<Json<AgentRuntimeArtifactDetail>> {
    require_runtime_read(&claims)?;
    let row = sqlx::query(
        r"
        SELECT
          a.id, a.tenant_id, a.runtime_session_id, a.agent_task_id, a.artifact_type,
          a.path, a.content_text, a.content_hash, a.size_bytes, a.created_at,
          s.workspace_root
        FROM agent_runtime_artifacts a
        INNER JOIN agent_runtime_sessions s
          ON s.tenant_id = a.tenant_id AND s.id = a.runtime_session_id
        WHERE a.tenant_id = ? AND a.runtime_session_id = ? AND a.id = ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(&artifact_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| {
        AppError::NotFound(format!("agent runtime artifact '{artifact_id}' not found"))
    })?;
    let workspace_root: String = row.get("workspace_root");
    let artifact = artifact_from_row(row);
    let (content, content_truncated, read_source) =
        read_runtime_artifact_content(&workspace_root, &artifact).await?;
    Ok(Json(AgentRuntimeArtifactDetail {
        artifact,
        content,
        content_truncated,
        read_source,
    }))
}

async fn cancel_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>> {
    require_runtime_write(&claims)?;
    request_cancel_runtime_session(&state, &claims.tenant_id, &id).await?;
    Ok(Json(
        json!({ "ok": true, "status": RUNTIME_STATUS_CANCELLING }),
    ))
}

async fn recover_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    req: Option<Json<RuntimeRecoverRequest>>,
) -> Result<Json<Value>> {
    require_runtime_write(&claims)?;
    let req = req.map(|Json(value)| value).unwrap_or_default();
    let timeout_secs = req.timeout_secs.unwrap_or(900).clamp(60, 86_400);
    let result = recover_stale_runtime_sessions(&state, &claims.tenant_id, timeout_secs).await?;
    Ok(Json(result))
}

pub async fn recover_stale_runtime_sessions(
    state: &AppState,
    tenant_id: &str,
    timeout_secs: u64,
) -> Result<Value> {
    let timeout_secs_i64 = i64::try_from(timeout_secs).unwrap_or(i64::MAX);
    let rows = sqlx::query(
        r"
        SELECT id, agent_task_id, status
        FROM agent_runtime_sessions
        WHERE tenant_id = ?
          AND status IN ('running','cancelling')
          AND COALESCE(heartbeat_at, updated_at, started_at, created_at) < datetime(CURRENT_TIMESTAMP, printf('-%d seconds', ?))
        ORDER BY updated_at ASC
        LIMIT 200
        ",
    )
    .bind(tenant_id)
    .bind(timeout_secs_i64)
    .fetch_all(&state.db)
    .await?;

    let mut recovered = 0_u64;
    for row in rows {
        let session_id: String = row.get("id");
        let task_id: Option<String> = row.get("agent_task_id");
        let previous_status: String = row.get("status");
        let session_update = sqlx::query(
            r"
            UPDATE agent_runtime_sessions
            SET status = ?,
                cancel_requested = 1,
                completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND id = ? AND status IN ('running','cancelling')
            ",
        )
        .bind(crate::routes::agent_ops::STATUS_STALE)
        .bind(tenant_id)
        .bind(&session_id)
        .execute(&state.db)
        .await?;
        if session_update.rows_affected() == 0 {
            continue;
        }
        recovered = recovered.saturating_add(1);
        sqlx::query(
            r"
            UPDATE agent_runtime_processes
            SET status = CASE WHEN status IN ('queued','running','cancelling') THEN 'timed_out' ELSE status END,
                stderr_preview = COALESCE(stderr_preview, 'runtime session marked stale by recovery'),
                completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND runtime_session_id = ?
            ",
        )
        .bind(tenant_id)
        .bind(&session_id)
        .execute(&state.db)
        .await?;
        let _ = kill_runtime_session_process_group(&state.db, tenant_id, &session_id).await;

        if let Some(task_id) = task_id.as_deref() {
            if let Err(error) = sqlx::query(
                r"
                UPDATE agent_tasks
                SET status = ?,
                    phase = ?,
                    progress_percent = 100,
                    last_event = 'runtime session 心跳超时，已标记为 stale',
                    queue_status = CASE WHEN queue_status IN ('claimed','running','cancelling') THEN 'dead' ELSE queue_status END,
                    lease_expires_at = NULL,
                    finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP),
                    completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                    updated_at = CURRENT_TIMESTAMP
                WHERE tenant_id = ? AND id = ? AND status IN ('created','queued','claimed','running','waiting_input','cancelling','retrying')
                ",
            )
            .bind(crate::routes::agent_ops::STATUS_STALE)
            .bind(crate::routes::agent_ops::PHASE_FINALIZING)
            .bind(tenant_id)
            .bind(task_id)
            .execute(&state.db)
            .await
            {
                tracing::warn!(
                    tenant_id,
                    task_id,
                    runtime_session_id = %session_id,
                    "failed to mark linked agent task stale: {}",
                    error
                );
            }
        }

        add_runtime_event(
            state,
            tenant_id,
            task_id.as_deref(),
            Some(&session_id),
            None,
            "runtime.session.stale",
            "runtime session 心跳超时，已标记为 stale",
            Some(json!({
                "runtimeSessionId": session_id,
                "previousStatus": previous_status,
                "timeoutSecs": timeout_secs,
                "recovery": true,
            })),
        )
        .await?;
    }

    Ok(json!({
        "recovered": recovered,
        "timeoutSecs": timeout_secs,
    }))
}

async fn get_runtime_session(
    db: &SqlitePool,
    tenant_id: &str,
    id: &str,
) -> Result<AgentRuntimeSessionInfo> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {RUNTIME_SESSION_SELECT} FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?"
    )))
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("agent runtime session '{id}' not found")))?;
    Ok(session_from_row(row))
}

async fn ensure_runtime_session_exists(db: &SqlitePool, tenant_id: &str, id: &str) -> Result<()> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(id)
    .fetch_one(db)
    .await?;
    if exists > 0 {
        Ok(())
    } else {
        Err(AppError::NotFound(format!(
            "agent runtime session '{id}' not found"
        )))
    }
}

fn build_workspace_root(state: &AppState, input: &RuntimeSessionCreateInput) -> Result<PathBuf> {
    let task_part = input
        .agent_task_id
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| input.workspace_hint.as_deref().unwrap_or("manual"));
    let root = state
        .data_dir
        .join("agent-workspaces")
        .join(safe_path_segment(&input.tenant_id))
        .join(safe_path_segment(&input.user_id))
        .join(safe_path_segment(task_part));
    ensure_workspace_root_safe(&state.data_dir, &root)?;
    Ok(root)
}

fn ensure_workspace_root_safe(data_dir: &Path, workspace_root: &Path) -> Result<()> {
    let base = data_dir.join("agent-workspaces");
    if workspace_root
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(AppError::ValidationError(
            "workspace path must not contain parent directory components".to_string(),
        ));
    }
    if !workspace_root.starts_with(&base) {
        return Err(AppError::ValidationError(
            "workspace path escapes agent workspace root".to_string(),
        ));
    }
    Ok(())
}

fn ensure_workspace_child_safe(workspace_root: &Path, child: &Path) -> Result<()> {
    if child
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(AppError::ValidationError(
            "runtime command cwd must not contain parent directory components".to_string(),
        ));
    }
    if !child.starts_with(workspace_root) {
        return Err(AppError::ValidationError(
            "runtime command cwd escapes runtime workspace".to_string(),
        ));
    }
    let canonical_root = workspace_root.canonicalize().map_err(|error| {
        AppError::ValidationError(format!("runtime workspace is unavailable: {error}"))
    })?;
    let mut existing = child;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| {
            AppError::ValidationError("runtime path has no existing workspace ancestor".into())
        })?;
    }
    let canonical_existing = existing.canonicalize().map_err(|error| {
        AppError::ValidationError(format!("runtime path cannot be resolved safely: {error}"))
    })?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(AppError::ValidationError(
            "runtime path escapes the workspace through a symbolic link".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct RuntimeLaunchPlan {
    program: String,
    args: Vec<String>,
    host_cwd: PathBuf,
    env_redacted_json: Value,
}

fn runtime_isolation_mode(requested: Option<&str>) -> Result<String> {
    let raw = requested
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| std::env::var("AOS_AGENT_RUNTIME_ISOLATION_MODE").ok())
        .unwrap_or_else(|| RUNTIME_ISOLATION_LOCAL_PROCESS.to_string());
    match raw.as_str() {
        RUNTIME_ISOLATION_LOCAL_PROCESS | RUNTIME_ISOLATION_DOCKER_SANDBOX => Ok(raw),
        _ => Err(AppError::ValidationError(format!(
            "unsupported runtime isolation mode: {raw}"
        ))),
    }
}

fn runtime_launch_plan(
    session: &AgentRuntimeSessionInfo,
    cwd: &Path,
    command: &str,
    process_id: &str,
    timeout_secs: u64,
) -> Result<RuntimeLaunchPlan> {
    match session.isolation_mode.as_str() {
        RUNTIME_ISOLATION_LOCAL_PROCESS => {
            let workspace_root = PathBuf::from(&session.workspace_root);
            let launcher = runtime::sandbox::build_confined_command_launcher(
                &workspace_root,
                cwd,
                command,
                Duration::from_secs(timeout_secs.max(1)),
                true,
                &[],
            )
            .map_err(|error| {
                AppError::Unimplemented(format!(
                    "local Agent Runtime requires the probed bwrap+prlimit sandbox backend: {error}"
                ))
            })?;
            Ok(RuntimeLaunchPlan {
                program: launcher.program,
                args: launcher.args,
                host_cwd: workspace_root,
                env_redacted_json: json!({
                    "isolationMode": RUNTIME_ISOLATION_LOCAL_PROCESS,
                    "sandboxBackend": "bwrap_prlimit",
                    "sandboxEnforcement": "full",
                    "networkIsolated": true,
                    "filesystemBoundary": "workspace"
                }),
            })
        }
        RUNTIME_ISOLATION_DOCKER_SANDBOX => docker_launch_plan(session, cwd, command, process_id),
        other => Err(AppError::ValidationError(format!(
            "unsupported runtime isolation mode: {other}"
        ))),
    }
}

fn docker_launch_plan(
    session: &AgentRuntimeSessionInfo,
    cwd: &Path,
    command: &str,
    process_id: &str,
) -> Result<RuntimeLaunchPlan> {
    let workspace_root = PathBuf::from(&session.workspace_root);
    ensure_workspace_child_safe(&workspace_root, cwd)?;
    let relative_cwd = cwd.strip_prefix(&workspace_root).map_err(|_| {
        AppError::ValidationError("docker runtime cwd escapes runtime workspace".to_string())
    })?;
    let container_cwd = Path::new("/workspace").join(relative_cwd);
    let image = std::env::var("AOS_AGENT_RUNTIME_DOCKER_IMAGE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "ubuntu:24.04".to_string());
    let container_name = docker_container_name(&session.id, process_id);
    Ok(RuntimeLaunchPlan {
        program: "docker".to_string(),
        args: vec![
            "run".to_string(),
            "--rm".to_string(),
            "--name".to_string(),
            container_name.clone(),
            "--network".to_string(),
            docker_network_mode(),
            "-v".to_string(),
            format!("{}:/workspace", workspace_root.to_string_lossy()),
            "-w".to_string(),
            container_cwd.to_string_lossy().to_string(),
            image.clone(),
            "sh".to_string(),
            "-lc".to_string(),
            command.to_string(),
        ],
        host_cwd: workspace_root,
        env_redacted_json: json!({
            "isolationMode": RUNTIME_ISOLATION_DOCKER_SANDBOX,
            "dockerImage": image,
            "dockerContainerName": container_name,
            "dockerNetwork": docker_network_mode()
        }),
    })
}

fn docker_network_mode() -> String {
    std::env::var("AOS_AGENT_RUNTIME_DOCKER_NETWORK")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "none".to_string())
}

fn docker_container_name(session_id: &str, process_id: &str) -> String {
    format!(
        "aos-agent-{}-{}",
        safe_path_segment(session_id),
        safe_path_segment(process_id)
    )
    .chars()
    .take(120)
    .collect()
}

fn runtime_process_container_name(env_redacted_json: Option<String>) -> Option<String> {
    let value = env_redacted_json.and_then(|raw| serde_json::from_str::<Value>(&raw).ok())?;
    value
        .get("dockerContainerName")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
}

async fn heartbeat_runtime_session(
    db: &SqlitePool,
    tenant_id: &str,
    session_id: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE agent_runtime_sessions SET heartbeat_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn runtime_command_cancel_was_requested(
    db: &SqlitePool,
    tenant_id: &str,
    session_id: &str,
    process_id: &str,
) -> Result<bool> {
    let row = sqlx::query(
        r"
        SELECT p.status AS process_status, s.cancel_requested AS session_cancel_requested
        FROM agent_runtime_processes p
        INNER JOIN agent_runtime_sessions s
          ON s.tenant_id = p.tenant_id AND s.id = p.runtime_session_id
        WHERE p.tenant_id = ? AND p.runtime_session_id = ? AND p.id = ?
        ",
    )
    .bind(tenant_id)
    .bind(session_id)
    .bind(process_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let process_status: String = row.get("process_status");
    let session_cancel_requested: bool = row.get("session_cancel_requested");
    Ok(session_cancel_requested
        || matches!(
            process_status.as_str(),
            "cancelling" | RUNTIME_PROCESS_STATUS_CANCELLED
        ))
}

async fn write_runtime_text_artifact(
    state: &AppState,
    tenant_id: &str,
    runtime_session_id: &str,
    agent_task_id: Option<&str>,
    artifact_type: &str,
    process_id: &str,
    content: &str,
) -> Result<String> {
    let artifact_id = uuid::Uuid::new_v4().to_string();
    let content_hash = sha256_hex(content);
    let content_text = preview_text(content, 120_000);
    let path = format!("workspace/.aos/artifacts/{process_id}.{artifact_type}.txt");
    let workspace_root: String = sqlx::query_scalar(
        "SELECT workspace_root FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(runtime_session_id)
    .fetch_one(&state.db)
    .await?;
    let absolute_path = PathBuf::from(&workspace_root).join(&path);
    ensure_workspace_child_safe(Path::new(&workspace_root), &absolute_path)?;
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&absolute_path, content).await?;
    let size_bytes = i64::try_from(content.len()).unwrap_or(i64::MAX);
    sqlx::query(
        r"
        INSERT INTO agent_runtime_artifacts
            (id, tenant_id, runtime_session_id, agent_task_id, artifact_type, path,
             content_text, content_hash, size_bytes)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&artifact_id)
    .bind(tenant_id)
    .bind(runtime_session_id)
    .bind(agent_task_id)
    .bind(artifact_type)
    .bind(path)
    .bind(content_text)
    .bind(content_hash)
    .bind(size_bytes)
    .execute(&state.db)
    .await?;
    Ok(artifact_id)
}

async fn read_runtime_artifact_content(
    workspace_root: &str,
    artifact: &AgentRuntimeArtifactInfo,
) -> Result<(Option<String>, bool, String)> {
    if let Some(path) = artifact.path.as_deref().filter(|value| !value.is_empty()) {
        let absolute_path = PathBuf::from(workspace_root).join(path);
        ensure_workspace_child_safe(Path::new(workspace_root), &absolute_path)?;
        if let Ok(file) = fs::File::open(&absolute_path).await {
            let mut bytes = Vec::new();
            file.take(ARTIFACT_DETAIL_MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .await?;
            let truncated = bytes.len() as u64 > ARTIFACT_DETAIL_MAX_BYTES;
            if truncated {
                bytes.truncate(ARTIFACT_DETAIL_MAX_BYTES as usize);
            }
            return Ok((
                Some(String::from_utf8_lossy(&bytes).to_string()),
                truncated,
                "file".to_string(),
            ));
        }
    }
    let content = artifact.content_text.clone();
    let truncated =
        artifact.size_bytes > content.as_ref().map(|value| value.len()).unwrap_or(0) as u64;
    Ok((content, truncated, "database_preview".to_string()))
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn preview_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value.chars().take(max_chars).collect::<String>();
    out.push_str("\n...[truncated]");
    out
}

fn normalize_runtime_artifact_path(path: &str) -> Result<String> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty()
        || Path::new(trimmed)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(AppError::ValidationError(
            "runtime artifact path must stay inside workspace".to_string(),
        ));
    }
    Ok(trimmed.replace('\\', "/"))
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .trim_matches('_')
        .chars()
        .take(96)
        .collect::<String>()
        .if_empty("unknown")
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn session_from_row(row: sqlx::sqlite::SqliteRow) -> AgentRuntimeSessionInfo {
    AgentRuntimeSessionInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        user_id: row.get("user_id"),
        agent_task_id: row.get("agent_task_id"),
        capability_key: row.get("capability_key"),
        workspace_root: row.get("workspace_root"),
        status: row.get("status"),
        isolation_mode: row.get("isolation_mode"),
        pid: row.get("pid"),
        process_group_id: row.get("process_group_id"),
        cancel_requested: row.get("cancel_requested"),
        heartbeat_at: row_datetime_opt(&row, "heartbeat_at"),
        started_at: row_datetime_opt(&row, "started_at"),
        completed_at: row_datetime_opt(&row, "completed_at"),
        created_at: row_datetime(&row, "created_at"),
        updated_at: row_datetime(&row, "updated_at"),
    }
}

fn process_from_row(row: sqlx::sqlite::SqliteRow) -> AgentRuntimeProcessInfo {
    AgentRuntimeProcessInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        runtime_session_id: row.get("runtime_session_id"),
        agent_task_id: row.get("agent_task_id"),
        command: row.get("command"),
        cwd: row.get("cwd"),
        env_redacted_json: parse_json_opt(row.get("env_redacted_json")),
        status: row.get("status"),
        pid: row.get("pid"),
        process_group_id: row.get("process_group_id"),
        exit_code: row.get("exit_code"),
        stdout_preview: row.get("stdout_preview"),
        stderr_preview: row.get("stderr_preview"),
        stdout_artifact_id: row.get("stdout_artifact_id"),
        stderr_artifact_id: row.get("stderr_artifact_id"),
        started_at: row_datetime_opt(&row, "started_at"),
        completed_at: row_datetime_opt(&row, "completed_at"),
        created_at: row_datetime(&row, "created_at"),
        updated_at: row_datetime(&row, "updated_at"),
    }
}

fn artifact_from_row(row: sqlx::sqlite::SqliteRow) -> AgentRuntimeArtifactInfo {
    AgentRuntimeArtifactInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        runtime_session_id: row.get("runtime_session_id"),
        agent_task_id: row.get("agent_task_id"),
        artifact_type: row.get("artifact_type"),
        path: row.get("path"),
        content_text: row.get("content_text"),
        content_hash: row.get("content_hash"),
        size_bytes: row.get::<u64, _>("size_bytes"),
        created_at: row_datetime(&row, "created_at"),
    }
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn row_datetime(row: &sqlx::sqlite::SqliteRow, name: &str) -> String {
    row.try_get::<chrono::NaiveDateTime, _>(name)
        .map(|value| value.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .or_else(|_| row.try_get::<String, _>(name))
        .unwrap_or_default()
}

fn row_datetime_opt(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<String> {
    row.try_get::<Option<chrono::NaiveDateTime>, _>(name)
        .ok()
        .flatten()
        .map(|value| value.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .or_else(|| row.try_get::<Option<String>, _>(name).ok().flatten())
}

async fn add_runtime_event(
    state: &AppState,
    tenant_id: &str,
    task_id: Option<&str>,
    runtime_session_id: Option<&str>,
    runtime_process_id: Option<&str>,
    event_type: &str,
    message: &str,
    metadata: Option<Value>,
) -> Result<()> {
    let Some(task_id) = task_id else {
        return Ok(());
    };
    let (phase, status, severity) = match event_type {
        "runtime.session.stale" => (
            crate::routes::agent_ops::PHASE_FINALIZING,
            crate::routes::agent_ops::STATUS_STALE,
            "warn",
        ),
        "runtime.session.cancel_requested" => (
            crate::routes::agent_ops::PHASE_FINALIZING,
            crate::routes::agent_ops::STATUS_CANCELLING,
            "warn",
        ),
        "runtime.command.cancelled" => (
            crate::routes::agent_ops::PHASE_EXECUTING,
            crate::routes::agent_ops::STATUS_CANCELLING,
            "warn",
        ),
        "runtime.command.failed" => (
            crate::routes::agent_ops::PHASE_EXECUTING,
            crate::routes::agent_ops::STATUS_RUNNING,
            "warn",
        ),
        _ => (
            crate::routes::agent_ops::PHASE_EXECUTING,
            crate::routes::agent_ops::STATUS_RUNNING,
            "info",
        ),
    };
    let metadata_for_trace = metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| {
            AppError::Internal(format!("runtime event metadata encode failed: {error}"))
        })?;
    sqlx::query(
        r"
        INSERT INTO agent_task_events
          (id, tenant_id, task_id, event_type, phase, status, severity, message, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(task_id)
    .bind(event_type)
    .bind(phase)
    .bind(status)
    .bind(severity)
    .bind(message)
    .bind(metadata.unwrap_or(Value::Null))
    .execute(&state.db)
    .await?;
    if let Err(error) = crate::routes::agent_ops::add_trace_event_full(
        state,
        tenant_id,
        task_id,
        event_type,
        Some(phase),
        Some(status),
        severity,
        message,
        metadata_for_trace.as_deref(),
        None,
        runtime_session_id,
        runtime_process_id,
        None,
        None,
        None,
        None,
    )
    .await
    {
        tracing::warn!(
            tenant_id,
            task_id,
            event_type,
            "failed to write runtime event into agent trace timeline: {}",
            error
        );
    }
    Ok(())
}

async fn kill_runtime_session_process_group(
    db: &SqlitePool,
    tenant_id: &str,
    session_id: &str,
) -> Result<()> {
    let rows = sqlx::query(
        r"
        SELECT process_group_id, pid, CAST(env_redacted_json AS TEXT) AS env_redacted_json
        FROM agent_runtime_processes
        WHERE tenant_id = ? AND runtime_session_id = ? AND status IN ('running','cancelling')
        ",
    )
    .bind(tenant_id)
    .bind(session_id)
    .fetch_all(db)
    .await?;
    for row in rows {
        let pgid = row
            .get::<Option<i64>, _>("process_group_id")
            .or_else(|| row.get::<Option<i64>, _>("pid"));
        if let Some(pgid) = pgid {
            signal_process_group_best_effort(pgid, "-TERM");
        }
        if let Some(container_name) = runtime_process_container_name(row.get("env_redacted_json")) {
            kill_docker_container_best_effort(&container_name);
        }
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let rows = sqlx::query(
        "SELECT process_group_id, pid FROM agent_runtime_processes
         WHERE tenant_id = ? AND runtime_session_id = ? AND status IN ('running','cancelling')",
    )
    .bind(tenant_id)
    .bind(session_id)
    .fetch_all(db)
    .await?;
    for row in rows {
        if let Some(pgid) = row
            .get::<Option<i64>, _>("process_group_id")
            .or_else(|| row.get::<Option<i64>, _>("pid"))
        {
            signal_process_group_best_effort(pgid, "-KILL");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn signal_process_group_best_effort(pgid: i64, signal: &str) {
    let _ = std::process::Command::new("kill")
        .arg(signal)
        .arg(format!("-{pgid}"))
        .status();
}

#[cfg(not(unix))]
fn signal_process_group_best_effort(_pgid: i64, _signal: &str) {}

fn kill_docker_container_best_effort(container_name: &str) {
    let _ = std::process::Command::new("docker")
        .arg("kill")
        .arg(container_name)
        .status();
}

fn require_runtime_read(claims: &Claims) -> Result<()> {
    if matches!(
        claims.role.as_str(),
        "viewer" | "developer" | "admin" | "superadmin"
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_runtime_write(claims: &Claims) -> Result<()> {
    if matches!(claims.role.as_str(), "developer" | "admin" | "superadmin") {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_segment_removes_path_separators() {
        assert_eq!(safe_path_segment("../tenant/user"), "tenant_user");
    }

    #[test]
    fn local_runtime_uses_the_probed_sandbox_or_fails_closed() {
        let workspace = std::env::temp_dir().join(format!(
            "aos-agent-runtime-sandbox-test-{}",
            uuid::Uuid::new_v4()
        ));
        let cwd = workspace.join("subdir");
        std::fs::create_dir_all(&cwd).unwrap();
        let session = AgentRuntimeSessionInfo {
            id: "session".into(),
            tenant_id: "tenant".into(),
            user_id: "user".into(),
            agent_task_id: None,
            capability_key: "rd_agent".into(),
            workspace_root: workspace.to_string_lossy().into_owned(),
            status: "running".into(),
            isolation_mode: RUNTIME_ISOLATION_LOCAL_PROCESS.into(),
            pid: None,
            process_group_id: None,
            cancel_requested: false,
            heartbeat_at: None,
            started_at: None,
            completed_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let plan = runtime_launch_plan(&session, &cwd, "pwd", "process", 30);
        if runtime::sandbox_backend_capability() == runtime::sandbox::EnforcementCapability::Full {
            let plan = plan.expect("probed backend must build a launch plan");
            assert_eq!(plan.program, "prlimit");
            assert!(plan.args.iter().any(|arg| arg == "bwrap"));
            assert!(plan.args.iter().any(|arg| arg == "--unshare-all"));
            assert!(plan.args.iter().any(|arg| arg == "/workspace/subdir"));
        } else {
            assert!(matches!(plan, Err(AppError::Unimplemented(_))));
        }
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn workspace_child_validation_rejects_symlink_escape_for_host_artifacts() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "aos-agent-runtime-path-test-{}",
            uuid::Uuid::new_v4()
        ));
        let outside = std::env::temp_dir().join(format!(
            "aos-agent-runtime-outside-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        let error = ensure_workspace_child_safe(&root, &root.join("escape/artifact.txt"))
            .expect_err("nearest existing symlink ancestor must be canonicalized");
        assert!(error.to_string().contains("symbolic link"));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
