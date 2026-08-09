//! Hooks API — CRUD for tenant-level tool and business lifecycle hooks.
//!
//! Allows tenants to register shell commands or inline Python scripts that run
//! before/after tool execution or around higher-level business stages.
//! Hooks receive structured JSON input via stdin and environment variables
//! (`HOOK_EVENT`, `HOOK_EVENT_KEY`, `HOOK_SUBJECT`, `HOOK_INPUT_JSON`, etc.).
//!
//! The gateway converts these DB records into `runtime::RuntimeHookConfig` at
//! session creation time, so hooks take effect immediately for new sessions.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{delete as routing_delete, get as routing_get, patch, post as routing_post},
    Json, Router,
};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::system_events;
use crate::routes::PaginationParams;
use crate::state::AppState;

use super::hooks_validation::{validate_hook_code, HookValidationRequest, HookValidationResponse};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateHookRequest {
    pub event_type: HookEventType,
    pub name: String,
    pub language: Option<String>,
    pub code: Option<String>,
    pub command: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    #[serde(default = "default_true")]
    pub fail_fast: bool,
    pub description: Option<String>,
    pub scenarios: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u32 {
    30
}

#[derive(Debug, Deserialize)]
pub struct UpdateHookRequest {
    pub event_type: Option<HookEventType>,
    pub name: Option<String>,
    pub language: Option<String>,
    pub code: Option<String>,
    pub command: Option<String>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
    pub timeout_seconds: Option<u32>,
    pub fail_fast: Option<bool>,
    pub description: Option<String>,
    pub scenarios: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct HookInfo {
    pub id: String,
    pub tenant_id: String,
    pub event_type: String,
    pub name: String,
    pub description: Option<String>,
    pub scenarios: Option<Vec<String>>,
    pub language: String,
    pub code: Option<String>,
    pub command: String,
    pub enabled: bool,
    pub priority: i32,
    pub timeout_seconds: u32,
    pub fail_fast: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct HookListResponse {
    pub hooks: Vec<HookInfo>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct DryRunRequest {
    pub event_type: Option<HookEventType>,
    pub scenario: Option<String>,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_output: Option<serde_json::Value>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Serialize)]
pub struct DryRunResponse {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub status: String,
    pub diagnostics: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HookLogEntry {
    pub id: String,
    pub hook_id: String,
    pub event_type: String,
    pub scenario: Option<String>,
    pub tool_name: String,
    pub input_json: Option<String>,
    pub output_json: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u32>,
    pub error_message: Option<String>,
    pub executed_at: String,
}

#[derive(Debug, Serialize)]
pub struct HookLogsResponse {
    pub logs: Vec<HookLogEntry>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventType {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    MessageReceived,
    BeforeModelCall,
    AfterModelCall,
    BeforeRoute,
    AfterRoute,
    BeforeFinalAnswer,
    AfterFinalAnswer,
    TaskCompleted,
    BotMessageReceived,
}

impl HookEventType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::PostToolUseFailure => "post_tool_use_failure",
            Self::MessageReceived => "message_received",
            Self::BeforeModelCall => "before_model_call",
            Self::AfterModelCall => "after_model_call",
            Self::BeforeRoute => "before_route",
            Self::AfterRoute => "after_route",
            Self::BeforeFinalAnswer => "before_final_answer",
            Self::AfterFinalAnswer => "after_final_answer",
            Self::TaskCompleted => "task_completed",
            Self::BotMessageReceived => "bot_message_received",
        }
    }

    #[cfg(any(
        feature = "pm",
        feature = "nl2sql",
        feature = "rd",
        feature = "bot-agents"
    ))]
    fn to_runtime_event(&self) -> runtime::HookEvent {
        runtime::HookEvent::from_db_key(self.as_str()).unwrap_or(runtime::HookEvent::PreToolUse)
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/hooks — list hooks for the current tenant
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<HookListResponse>> {
    let rows = sqlx::query(
        r"
        SELECT id, tenant_id, event_type, name, description, CAST(scenarios AS TEXT) AS scenarios_json,
               language, code, command,
               enabled, priority, timeout_seconds, fail_fast, created_at, updated_at
        FROM tenant_hooks
        WHERE tenant_id = ?
        ORDER BY priority ASC, created_at ASC
        LIMIT ? OFFSET ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(pagination.limit())
    .bind(pagination.offset())
    .fetch_all(&state.db)
    .await?;

    let total_row = sqlx::query("SELECT COUNT(*) as cnt FROM tenant_hooks WHERE tenant_id = ?")
        .bind(&claims.tenant_id)
        .fetch_one(&state.db)
        .await?;

    let total: i64 = total_row.get("cnt");
    let hooks: Vec<HookInfo> = rows.into_iter().map(row_to_hook_info).collect();

    Ok(Json(HookListResponse { hooks, total }))
}

/// POST /api/v1/hooks — create a new hook
async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateHookRequest>,
) -> Result<Json<HookInfo>> {
    let id = uuid::Uuid::new_v4().to_string();
    let language = req.language.as_deref().unwrap_or("shell");
    let code = req.code.as_deref().filter(|c| !c.is_empty());
    let command = req.command.as_deref().unwrap_or("");
    let description = req.description.as_deref().filter(|d| !d.is_empty());
    let scenarios_json = normalize_hook_scenarios(req.scenarios.as_ref())?;

    sqlx::query(
        r"
        INSERT INTO tenant_hooks
            (id, tenant_id, event_type, name, description, scenarios, language, code, command,
             enabled, priority, timeout_seconds, fail_fast)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(req.event_type.as_str())
    .bind(&req.name)
    .bind(description)
    .bind(scenarios_json)
    .bind(language)
    .bind(code)
    .bind(command)
    .bind(req.enabled)
    .bind(req.priority)
    .bind(req.timeout_seconds)
    .bind(req.fail_fast)
    .execute(&state.db)
    .await?;

    system_events::broadcast_hooks_updated(&claims.tenant_id);

    let row = sqlx::query(
        r"
        SELECT id, tenant_id, event_type, name, description, CAST(scenarios AS TEXT) AS scenarios_json,
               language, code, command,
               enabled, priority, timeout_seconds, fail_fast, created_at, updated_at
        FROM tenant_hooks WHERE id = ?
        ",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(row_to_hook_info(row)))
}

/// GET /api/v1/hooks/{id} — get a hook by ID
async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<HookInfo>> {
    let row = sqlx::query(
        r"
        SELECT id, tenant_id, event_type, name, description, CAST(scenarios AS TEXT) AS scenarios_json,
               language, code, command,
               enabled, priority, timeout_seconds, fail_fast, created_at, updated_at
        FROM tenant_hooks
        WHERE id = ? AND tenant_id = ?
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("hook '{id}' not found")))?;

    Ok(Json(row_to_hook_info(row)))
}

/// PATCH /api/v1/hooks/{id} — update a hook
async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateHookRequest>,
) -> Result<Json<HookInfo>> {
    // Verify the hook exists and belongs to this tenant before updating
    let _ = sqlx::query("SELECT id FROM tenant_hooks WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("hook '{id}' not found")))?;

    let name = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let language = req
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let code_provided = req.code.is_some();
    let code = req.code.as_deref().filter(|c| !c.is_empty());
    let command = req
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let enabled = req.enabled;
    let priority = req.priority;
    let timeout_seconds = req.timeout_seconds;
    let fail_fast = req.fail_fast;
    let event_type = req.event_type.as_ref().map(HookEventType::as_str);
    let scenarios_provided = req.scenarios.is_some();
    let scenarios_json = normalize_hook_scenarios(req.scenarios.as_ref())?;

    sqlx::query(
        r"
        UPDATE tenant_hooks
        SET
            event_type      = COALESCE(?, event_type),
            name            = COALESCE(?, name),
            description     = COALESCE(?, description),
            scenarios       = CASE WHEN ? THEN ? ELSE scenarios END,
            language        = COALESCE(?, language),
            code            = CASE WHEN ? THEN ? ELSE code END,
            command         = COALESCE(?, command),
            enabled         = COALESCE(?, enabled),
            priority        = COALESCE(?, priority),
            timeout_seconds = COALESCE(?, timeout_seconds),
            fail_fast       = COALESCE(?, fail_fast)
        WHERE id = ? AND tenant_id = ?
        ",
    )
    .bind(event_type)
    .bind(name)
    .bind(description)
    .bind(scenarios_provided)
    .bind(scenarios_json)
    .bind(language)
    .bind(code_provided)
    .bind(code)
    .bind(command)
    .bind(enabled)
    .bind(priority)
    .bind(timeout_seconds)
    .bind(fail_fast)
    .bind(&id)
    .bind(&claims.tenant_id)
    .execute(&state.db)
    .await?;

    system_events::broadcast_hooks_updated(&claims.tenant_id);

    let row = sqlx::query(
        r"
        SELECT id, tenant_id, event_type, name, description, CAST(scenarios AS TEXT) AS scenarios_json,
               language, code, command,
               enabled, priority, timeout_seconds, fail_fast, created_at, updated_at
        FROM tenant_hooks WHERE id = ?
        ",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(row_to_hook_info(row)))
}

/// DELETE /api/v1/hooks/{id} — delete a hook
async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let result = sqlx::query("DELETE FROM tenant_hooks WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("hook '{id}' not found")));
    }

    system_events::broadcast_hooks_updated(&claims.tenant_id);

    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// POST /api/v1/hooks/validate — validate hook code before saving
async fn validate(
    State(_state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(req): Json<HookValidationRequest>,
) -> Result<Json<HookValidationResponse>> {
    let result = validate_hook_code(&req.code, req.language.as_str());
    Ok(Json(result))
}

/// POST /api/v1/hooks/{id}/dry-run — test a hook with a sample payload.
async fn dry_run(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<DryRunRequest>,
) -> Result<Json<DryRunResponse>> {
    let row = sqlx::query(
        r"
        SELECT event_type, CAST(scenarios AS TEXT) AS scenarios_json,
               language, code, command, timeout_seconds, fail_fast
        FROM tenant_hooks
        WHERE id = ? AND tenant_id = ?
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("hook '{id}' not found")))?;

    let stored_event_type: String = row.get("event_type");
    let scenarios = parse_scenarios_json(row.get::<Option<String>, _>("scenarios_json"));
    let language: String = row.get("language");
    let code: Option<String> = row.get("code");
    let command: String = row.get("command");
    let timeout_seconds: u32 = row.get("timeout_seconds");
    let fail_fast: bool = row.get("fail_fast");
    let scenario = normalize_hook_scenario_opt(req.scenario.as_deref())?;
    if !hook_applies_to_scenario(scenarios.as_deref(), scenario.as_deref()) {
        let input = serde_json::to_string(&req.tool_input).unwrap_or_default();
        let output_json = serde_json::json!({
            "mode": "dry_run",
            "status": "skipped",
            "scenario": scenario.clone(),
            "reason": "hook does not apply to selected scenario",
        })
        .to_string();
        let _ = sqlx::query(
            r"
            INSERT INTO hook_execution_logs
                (id, tenant_id, hook_id, event_type, scenario, tool_name, input_json, output_json,
                 exit_code, duration_ms, error_message)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&claims.tenant_id)
        .bind(&id)
        .bind(
            req.event_type
                .as_ref()
                .map(HookEventType::as_str)
                .unwrap_or(stored_event_type.as_str()),
        )
        .bind(scenario.clone())
        .bind(&req.tool_name)
        .bind(input)
        .bind(output_json)
        .bind(0)
        .bind(0_u32)
        .bind(Option::<String>::None)
        .execute(&state.db)
        .await;

        return Ok(Json(DryRunResponse {
            stdout: Some(format!(
                "SKIPPED: hook does not apply to scenario {}",
                scenario.as_deref().unwrap_or("all")
            )),
            stderr: None,
            exit_code: 0,
            duration_ms: 0,
            status: "skipped".to_string(),
            diagnostics: Vec::new(),
            error: None,
        }));
    }
    let resolved_command = resolve_hook_command(&language, code.as_ref(), &command);
    if resolved_command.trim().is_empty() {
        return Err(AppError::ValidationError(
            "hook has no executable code or command".to_string(),
        ));
    }

    let event_type = req
        .event_type
        .as_ref()
        .map(HookEventType::as_str)
        .unwrap_or(stored_event_type.as_str());
    let entry = runtime::RuntimeHookEntry::db(
        id.clone(),
        claims.tenant_id.clone(),
        resolved_command.clone(),
        (timeout_seconds > 0).then_some(timeout_seconds),
        fail_fast,
    );
    let runtime_event = runtime::HookEvent::from_db_key(event_type).ok_or_else(|| {
        AppError::ValidationError(format!("unsupported hook event_type: {event_type}"))
    })?;
    let hook_config = hook_config_for_single_entry(runtime_event, entry);

    let input = serde_json::to_string(&req.tool_input).unwrap_or_default();
    let output = req
        .tool_output
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .unwrap_or_default();
    let runner = runtime::HookRunner::new(hook_config);
    let mut reporter = DryRunReporter::default();
    let start = std::time::Instant::now();
    let result = runner.run_hook_event_with_context(
        runtime_event,
        &req.tool_name,
        &input,
        output.as_deref(),
        req.is_error || matches!(runtime_event, runtime::HookEvent::PostToolUseFailure),
        None,
        Some(&mut reporter),
    );
    let duration_ms = reporter
        .duration_ms
        .unwrap_or_else(|| start.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
    let status = if result.is_cancelled() {
        "cancelled"
    } else if result.is_denied() {
        "deny"
    } else if result.is_failed() {
        "failed"
    } else {
        "allow"
    }
    .to_string();
    let exit_code = reporter.exit_code.unwrap_or_else(|| {
        if result.is_cancelled() {
            130
        } else if result.is_denied() {
            2
        } else if result.is_failed() {
            1
        } else {
            0
        }
    });
    let stdout = if !reporter.stdout.is_empty() {
        Some(reporter.stdout.clone())
    } else if result.messages().is_empty() {
        Some("ALLOW".to_string())
    } else {
        Some(result.messages().join("\n"))
    };
    let stderr = (!reporter.stderr.is_empty()).then_some(reporter.stderr.clone());
    let error = if result.is_failed() || result.is_cancelled() {
        Some(result.messages().join("\n"))
    } else {
        None
    };
    let diagnostics = hook_script_diagnostics(&resolved_command);
    let output_json = serde_json::json!({
        "mode": "dry_run",
        "stdout": stdout.clone(),
        "stderr": stderr.clone(),
        "status": status,
        "scenario": scenario.clone(),
        "diagnostics": diagnostics.clone(),
    })
    .to_string();
    let _ = sqlx::query(
        r"
        INSERT INTO hook_execution_logs
            (id, tenant_id, hook_id, event_type, scenario, tool_name, input_json, output_json,
             exit_code, duration_ms, error_message)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(event_type)
    .bind(scenario)
    .bind(&req.tool_name)
    .bind(input.clone())
    .bind(output_json)
    .bind(exit_code)
    .bind(u32::try_from(duration_ms).unwrap_or(u32::MAX))
    .bind(error.clone())
    .execute(&state.db)
    .await;

    Ok(Json(DryRunResponse {
        stdout,
        stderr,
        exit_code,
        duration_ms,
        status,
        diagnostics,
        error,
    }))
}

/// GET /api/v1/hooks/{id}/logs — fetch recent execution logs for a hook.
async fn logs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<HookLogsResponse>> {
    let _hook = sqlx::query("SELECT id FROM tenant_hooks WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("hook '{id}' not found")))?;

    let limit = params.limit();
    let offset = params.offset();

    let rows = sqlx::query(
        "SELECT id, hook_id, event_type, scenario, tool_name, input_json, output_json, \
         exit_code, duration_ms, error_message, executed_at \
         FROM hook_execution_logs \
         WHERE hook_id = ? AND tenant_id = ? \
         ORDER BY executed_at DESC LIMIT ? OFFSET ?",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total_row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM hook_execution_logs WHERE hook_id = ? AND tenant_id = ?",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .fetch_one(&state.db)
    .await?;
    let total: i64 = total_row.get("cnt");

    let logs: Vec<HookLogEntry> = rows
        .into_iter()
        .map(|row| HookLogEntry {
            id: row.get("id"),
            hook_id: row.get("hook_id"),
            event_type: row.get("event_type"),
            scenario: row.get("scenario"),
            tool_name: row.get("tool_name"),
            input_json: row.get("input_json"),
            output_json: row.get("output_json"),
            exit_code: row.get("exit_code"),
            duration_ms: row.get("duration_ms"),
            error_message: row.get("error_message"),
            executed_at: row
                .get::<chrono::DateTime<chrono::Utc>, _>("executed_at")
                .to_rfc3339(),
        })
        .collect();

    Ok(Json(HookLogsResponse { logs, total }))
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(
    feature = "pm",
    feature = "nl2sql",
    feature = "rd",
    feature = "bot-agents"
))]
pub(crate) async fn run_lifecycle_hooks(
    state: &AppState,
    tenant_id: &str,
    scenario: &str,
    event_type: HookEventType,
    subject: &str,
    input_json: serde_json::Value,
    output_json: Option<serde_json::Value>,
    is_error: bool,
) -> Result<runtime::HookRunResult> {
    let event_key = event_type.as_str();
    let rows = sqlx::query(
        r"
        SELECT id, tenant_id, event_type, language,
               CAST(code AS TEXT),
               CAST(command AS TEXT),
               timeout_seconds, fail_fast
        FROM tenant_hooks
        WHERE tenant_id = ? AND enabled = 1 AND event_type = ?
          AND (scenarios IS NULL OR json_array_length(scenarios) = 0 OR EXISTS (SELECT 1 FROM json_each(scenarios) WHERE json_each.value = ?))
        ORDER BY priority ASC, created_at ASC
        ",
    )
    .bind(tenant_id)
    .bind(event_key)
    .bind(scenario)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(runtime::HookRunResult::allow(Vec::new()));
    }

    let mut entries = Vec::new();
    for row in rows {
        let id: String = row.get(0);
        let row_tenant_id: String = row.get(1);
        let language: String = row.get(3);
        let code: Option<String> = row.get(4);
        let command: String = row.get(5);
        let timeout_seconds: u32 = row.get(6);
        let fail_fast: bool = row.get(7);
        let resolved = resolve_hook_command(&language, code.as_ref(), &command);
        if resolved.trim().is_empty() {
            continue;
        }
        entries.push(runtime::RuntimeHookEntry::db(
            id,
            row_tenant_id,
            resolved,
            (timeout_seconds > 0).then_some(timeout_seconds),
            fail_fast,
        ));
    }

    if entries.is_empty() {
        return Ok(runtime::HookRunResult::allow(Vec::new()));
    }

    let runtime_event = event_type.to_runtime_event();
    let hook_config = hook_config_for_entries(runtime_event, entries);
    let input = serde_json::to_string(&input_json).unwrap_or_else(|_| "{}".to_string());
    let output = output_json
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .unwrap_or_default();
    let runner = runtime::HookRunner::new(hook_config);
    let mut reporter = LifecycleHookReporter::default();
    let result = runner.run_hook_event_with_context(
        runtime_event,
        subject,
        &input,
        output.as_deref(),
        is_error,
        None,
        Some(&mut reporter),
    );
    persist_hook_progress_events(state, &reporter.events, Some(scenario)).await;
    Ok(result)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hook_config_for_single_entry(
    event: runtime::HookEvent,
    entry: runtime::RuntimeHookEntry,
) -> runtime::RuntimeHookConfig {
    hook_config_for_entries(event, vec![entry])
}

fn hook_config_for_entries(
    event: runtime::HookEvent,
    entries: Vec<runtime::RuntimeHookEntry>,
) -> runtime::RuntimeHookConfig {
    match event {
        runtime::HookEvent::PreToolUse => {
            runtime::RuntimeHookConfig::new_entries(entries, Vec::new(), Vec::new())
        }
        runtime::HookEvent::PostToolUse => {
            runtime::RuntimeHookConfig::new_entries(Vec::new(), entries, Vec::new())
        }
        runtime::HookEvent::PostToolUseFailure => {
            runtime::RuntimeHookConfig::new_entries(Vec::new(), Vec::new(), entries)
        }
        _ => {
            let mut lifecycle = BTreeMap::new();
            lifecycle.insert(event.db_key().to_string(), entries);
            runtime::RuntimeHookConfig::new_entries_with_lifecycle(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                lifecycle,
            )
        }
    }
}

#[derive(Default)]
#[cfg(any(
    feature = "pm",
    feature = "nl2sql",
    feature = "rd",
    feature = "bot-agents"
))]
struct LifecycleHookReporter {
    events: Vec<runtime::HookProgressEvent>,
}

#[cfg(any(
    feature = "pm",
    feature = "nl2sql",
    feature = "rd",
    feature = "bot-agents"
))]
impl runtime::HookProgressReporter for LifecycleHookReporter {
    fn on_event(&mut self, event: &runtime::HookProgressEvent) {
        self.events.push(event.clone());
    }
}

#[cfg(any(
    feature = "pm",
    feature = "nl2sql",
    feature = "rd",
    feature = "bot-agents"
))]
async fn persist_hook_progress_events(
    state: &AppState,
    events: &[runtime::HookProgressEvent],
    scenario: Option<&str>,
) {
    for event in events {
        let (
            event_type,
            subject,
            hook_id,
            tenant_id,
            command,
            status,
            exit_code,
            duration_ms,
            stdout,
            stderr,
            input_json,
            output_json,
        ) = match event {
            runtime::HookProgressEvent::Completed {
                event,
                tool_name,
                command,
                hook_id,
                tenant_id,
                status,
                exit_code,
                duration_ms,
                stdout,
                stderr,
                tool_input,
                tool_output,
            } => (
                event.db_key(),
                tool_name.as_str(),
                hook_id.as_deref(),
                tenant_id.as_deref(),
                command.as_str(),
                status.as_str(),
                *exit_code,
                Some(*duration_ms),
                stdout.as_str(),
                stderr.as_str(),
                Some(tool_input.clone()),
                tool_output.clone(),
            ),
            runtime::HookProgressEvent::Cancelled {
                event,
                tool_name,
                command,
                hook_id,
                tenant_id,
                duration_ms,
                tool_input,
                tool_output,
            } => (
                event.db_key(),
                tool_name.as_str(),
                hook_id.as_deref(),
                tenant_id.as_deref(),
                command.as_str(),
                "cancelled",
                Some(130),
                Some(*duration_ms),
                "",
                "",
                Some(tool_input.clone()),
                tool_output.clone(),
            ),
            runtime::HookProgressEvent::Started { .. } => continue,
        };
        let Some(hook_id) = hook_id else {
            continue;
        };
        let Some(tenant_id) = tenant_id else {
            continue;
        };
        let output = serde_json::json!({
            "status": status,
            "command": command,
            "stdout": stdout,
            "stderr": stderr,
            "output": output_json,
        })
        .to_string();
        let error_message = if status == "failed" || status == "cancelled" || !stderr.is_empty() {
            Some(if stderr.is_empty() { status } else { stderr }.to_string())
        } else {
            None
        };
        if let Err(error) = sqlx::query(
            r"
            INSERT INTO hook_execution_logs
                (id, tenant_id, hook_id, event_type, scenario, tool_name, input_json, output_json,
                 exit_code, duration_ms, error_message)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(hook_id)
        .bind(event_type)
        .bind(scenario)
        .bind(subject)
        .bind(input_json)
        .bind(output)
        .bind(exit_code)
        .bind(duration_ms.and_then(|v| u32::try_from(v).ok()))
        .bind(error_message)
        .execute(&state.db)
        .await
        {
            tracing::warn!(
                hook_id,
                tenant_id,
                error = %error,
                "failed to persist lifecycle hook execution log"
            );
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn row_to_hook_info(row: sqlx::sqlite::SqliteRow) -> HookInfo {
    let scenarios_json: Option<String> = row.get("scenarios_json");
    HookInfo {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        event_type: row.get("event_type"),
        name: row.get("name"),
        description: row.get("description"),
        scenarios: parse_scenarios_json(scenarios_json),
        language: row.get("language"),
        code: row.get("code"),
        command: row.get("command"),
        enabled: row.get("enabled"),
        priority: row.get("priority"),
        timeout_seconds: row.get("timeout_seconds"),
        fail_fast: row.get("fail_fast"),
        created_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .to_rfc3339(),
        updated_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .to_rfc3339(),
    }
}

fn parse_scenarios_json(value: Option<String>) -> Option<Vec<String>> {
    let scenarios = value
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|scenario| normalize_hook_scenario_opt(Some(&scenario)).ok().flatten())
        .fold(Vec::<String>::new(), |mut acc, scenario| {
            if !acc.iter().any(|existing| existing == &scenario) {
                acc.push(scenario);
            }
            acc
        });
    (!scenarios.is_empty()).then_some(scenarios)
}

fn normalize_hook_scenarios(scenarios: Option<&Vec<String>>) -> Result<Option<String>> {
    let Some(scenarios) = scenarios else {
        return Ok(None);
    };
    let mut normalized = Vec::<String>::new();
    for scenario in scenarios {
        if let Some(scenario) = normalize_hook_scenario_opt(Some(scenario))? {
            if !normalized.iter().any(|existing| existing == &scenario) {
                normalized.push(scenario);
            }
        }
    }
    if normalized.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(&normalized)
            .map(Some)
            .map_err(|e| AppError::ValidationError(format!("invalid hook scenarios: {e}")))
    }
}

fn normalize_hook_scenario_opt(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let normalized = match value.to_ascii_lowercase().as_str() {
        "chat" | "ai_chat" | "conversation" => "chat",
        "pm" | "operations" | "operations_copilot" | "pm_assistant" => "pm",
        "materials" | "material" | "material_factory" | "material_workshop" => "materials",
        "nl2sql" | "data_explorer" | "data_exploration" | "analytics" => "nl2sql",
        "rd" | "dev" | "development" | "agent" | "agent_dev" => "rd",
        "bot" | "bot_gateway" | "bot_agent" | "robot" => "bot",
        other => {
            return Err(AppError::ValidationError(format!(
                "unsupported hook scenario: {other}"
            )));
        }
    };
    Ok(Some(normalized.to_string()))
}

fn hook_applies_to_scenario(hook_scenarios: Option<&[String]>, scenario: Option<&str>) -> bool {
    let Some(hook_scenarios) = hook_scenarios.filter(|items| !items.is_empty()) else {
        return true;
    };
    let Some(scenario) = scenario else {
        return true;
    };
    hook_scenarios.iter().any(|item| item == scenario)
}

fn resolve_hook_command(language: &str, code: Option<&String>, command: &str) -> String {
    match language {
        "python" => {
            if let Some(code) = code {
                if !code.trim().is_empty() {
                    let escaped = code.replace('\'', "'\\''");
                    return format!("python3 -c '{escaped}'");
                }
            }
            command.to_string()
        }
        "shell" | "bash" | "sh" => {
            if let Some(code) = code {
                if !code.trim().is_empty() {
                    return code.clone();
                }
            }
            command.to_string()
        }
        _ => command.to_string(),
    }
}

#[derive(Default)]
struct DryRunReporter {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

impl runtime::HookProgressReporter for DryRunReporter {
    fn on_event(&mut self, event: &runtime::HookProgressEvent) {
        match event {
            runtime::HookProgressEvent::Completed {
                stdout,
                stderr,
                exit_code,
                duration_ms,
                ..
            } => {
                self.stdout = stdout.clone();
                self.stderr = stderr.clone();
                self.exit_code = *exit_code;
                self.duration_ms = Some(*duration_ms);
            }
            runtime::HookProgressEvent::Cancelled { duration_ms, .. } => {
                self.exit_code = Some(130);
                self.duration_ms = Some(*duration_ms);
            }
            runtime::HookProgressEvent::Started { .. } => {}
        }
    }
}

fn hook_script_diagnostics(command: &str) -> Vec<String> {
    let normalized = command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let mut diagnostics = Vec::new();

    if normalized.contains("curl")
        && (normalized.contains("> /dev/null 2>&1") || normalized.contains(">/dev/null 2>&1"))
    {
        diagnostics.push(
            "hook stdout/stderr is redirected to /dev/null; provider response cannot be shown"
                .to_string(),
        );
    }
    if normalized.contains("curl") && normalized.contains("|| true") {
        diagnostics.push(
            "curl failure is ignored by `|| true`; the test can pass even when the notification was not delivered"
                .to_string(),
        );
    }

    diagnostics
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/", routing_post(create))
        .route("/validate", routing_post(validate))
        .route("/{id}", routing_get(get))
        .route("/{id}", patch(update))
        .route("/{id}", routing_delete(delete))
        .route("/{id}/dry-run", routing_post(dry_run))
        .route("/{id}/logs", routing_get(logs))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}
