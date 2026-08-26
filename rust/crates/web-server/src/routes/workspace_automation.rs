//! OS-confined command execution and durable personal-workspace schedules.

use axum::extract::{Extension, Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashMap;
use std::path::Path as FsPath;
use std::str::FromStr;
use std::sync::{atomic::AtomicBool, Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use crate::auth::Claims;
use crate::error::{AppError, Result as AppResult};
use crate::state::AppState;

const DEFAULT_CWD: &str = "/projects/session";
const MAX_COMMAND_CHARS: usize = 32_000;
const MAX_NAME_CHARS: usize = 160;
const MAX_STORED_OUTPUT_CHARS: usize = 64_000;
const WORKER_CONCURRENCY: usize = 4;
const MAX_CONCURRENT_COMMANDS: usize = 8;

type ActiveExecutions = HashMap<String, Weak<AtomicBool>>;

fn active_executions() -> &'static Mutex<ActiveExecutions> {
    static ACTIVE: OnceLock<Mutex<ActiveExecutions>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn command_execution_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_COMMANDS)))
}

struct ActiveExecutionRegistration {
    id: String,
    cancellation: Arc<AtomicBool>,
}

struct RequestExecutionCancellation {
    cancellation: Arc<AtomicBool>,
}

impl RequestExecutionCancellation {
    fn new() -> Self {
        Self {
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for RequestExecutionCancellation {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        self.cancellation.store(true, Ordering::Release);
    }
}

impl ActiveExecutionRegistration {
    fn register(id: &str) -> Self {
        let cancellation = Arc::new(AtomicBool::new(false));
        active_executions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), Arc::downgrade(&cancellation));
        Self {
            id: id.to_string(),
            cancellation,
        }
    }
}

impl Drop for ActiveExecutionRegistration {
    fn drop(&mut self) {
        let mut active = active_executions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .get(&self.id)
            .and_then(Weak::upgrade)
            .is_some_and(|value| Arc::ptr_eq(&value, &self.cancellation))
        {
            active.remove(&self.id);
        }
    }
}

fn cancel_active_execution(id: &str) -> bool {
    use std::sync::atomic::Ordering;

    let cancellation = active_executions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(id)
        .and_then(Weak::upgrade);
    cancellation.is_some_and(|cancellation| {
        cancellation.store(true, Ordering::Release);
        true
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceCommandInput {
    pub command: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceCommandResult {
    status: String,
    exit_code: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    duration_ms: u64,
    stdout: String,
    stderr: String,
    cwd: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateWorkspaceScheduleInput {
    pub name: String,
    pub command: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    pub script_path: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateWorkspaceScheduleInput {
    pub name: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub timeout_seconds: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub script_path: Option<Option<String>>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, FromRow)]
struct WorkspaceScheduleRow {
    id: String,
    tenant_id: String,
    user_id: String,
    session_id: Option<String>,
    name: String,
    script_path: Option<String>,
    command: String,
    cwd: String,
    cron_expression: String,
    timezone: String,
    timeout_seconds: i64,
    enabled: i64,
    status: String,
    next_run_at: Option<i64>,
    last_started_at: Option<i64>,
    last_finished_at: Option<i64>,
    last_exit_code: Option<i64>,
    last_stdout: Option<String>,
    last_stderr: Option<String>,
    run_count: i64,
    lease_fencing: i64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceSchedule {
    pub id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub script_path: Option<String>,
    pub command: String,
    pub cwd: String,
    pub cron_expression: String,
    pub timezone: String,
    pub timeout_seconds: u64,
    pub enabled: bool,
    pub status: String,
    pub next_run_at: Option<String>,
    pub last_started_at: Option<String>,
    pub last_finished_at: Option<String>,
    pub last_exit_code: Option<i32>,
    pub last_stdout: Option<String>,
    pub last_stderr: Option<String>,
    pub run_count: u64,
    pub created_at: String,
    pub updated_at: String,
}

impl From<WorkspaceScheduleRow> for WorkspaceSchedule {
    fn from(row: WorkspaceScheduleRow) -> Self {
        Self {
            id: row.id,
            session_id: row.session_id,
            name: row.name,
            script_path: row.script_path,
            command: row.command,
            cwd: row.cwd,
            cron_expression: row.cron_expression,
            timezone: row.timezone,
            timeout_seconds: u64::try_from(row.timeout_seconds).unwrap_or(120),
            enabled: row.enabled != 0,
            status: row.status,
            next_run_at: timestamp_string(row.next_run_at),
            last_started_at: timestamp_string(row.last_started_at),
            last_finished_at: timestamp_string(row.last_finished_at),
            last_exit_code: row
                .last_exit_code
                .and_then(|value| i32::try_from(value).ok()),
            last_stdout: row.last_stdout,
            last_stderr: row.last_stderr,
            run_count: u64::try_from(row.run_count).unwrap_or(0),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn default_cwd() -> String {
    DEFAULT_CWD.to_string()
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn deserialize_nullable_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

const fn default_timeout_seconds() -> u64 {
    120
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/commands", post(execute_command))
        .route("/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/schedules/{schedule_id}",
            patch(update_schedule).delete(cancel_schedule),
        )
}

async fn execute_command(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<WorkspaceCommandInput>,
) -> AppResult<Json<WorkspaceCommandResult>> {
    let request = RequestExecutionCancellation::new();
    let result = execute_workspace_command(
        &state,
        &claims.tenant_id,
        &claims.sub,
        input,
        Some(Arc::clone(&request.cancellation)),
    )
    .await?;
    Ok(Json(result))
}

async fn list_schedules(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> AppResult<Json<Vec<WorkspaceSchedule>>> {
    Ok(Json(
        list_schedules_for_user(&state, &claims.tenant_id, &claims.sub).await?,
    ))
}

async fn create_schedule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(mut input): Json<CreateWorkspaceScheduleInput>,
) -> AppResult<Json<WorkspaceSchedule>> {
    // Browser clients cannot assert a conversation association. The
    // authenticated parent-agent path binds its current session server-side.
    input.session_id = None;
    Ok(Json(
        create_schedule_for_user(&state, &claims.tenant_id, &claims.sub, input).await?,
    ))
}

async fn update_schedule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(schedule_id): Path<String>,
    Json(input): Json<UpdateWorkspaceScheduleInput>,
) -> AppResult<Json<WorkspaceSchedule>> {
    Ok(Json(
        update_schedule_for_user(&state, &claims.tenant_id, &claims.sub, &schedule_id, input)
            .await?,
    ))
}

async fn cancel_schedule(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(schedule_id): Path<String>,
) -> AppResult<Json<WorkspaceSchedule>> {
    Ok(Json(
        cancel_schedule_for_user(&state, &claims.tenant_id, &claims.sub, &schedule_id).await?,
    ))
}

fn validate_command(command: &str) -> AppResult<String> {
    let command = command.trim();
    if command.is_empty() || command.chars().count() > MAX_COMMAND_CHARS {
        return Err(AppError::ValidationError(format!(
            "workspace command must contain 1 to {MAX_COMMAND_CHARS} characters"
        )));
    }
    Ok(command.to_string())
}

fn validate_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(AppError::ValidationError(format!(
            "schedule name must contain 1 to {MAX_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_string())
}

fn validate_timeout(timeout_seconds: u64) -> AppResult<u64> {
    if !(1..=600).contains(&timeout_seconds) {
        return Err(AppError::ValidationError(
            "workspace command timeout must be between 1 and 600 seconds".to_string(),
        ));
    }
    Ok(timeout_seconds)
}

fn parse_schedule(expression: &str) -> AppResult<(String, Schedule)> {
    let expression = expression.trim();
    let fields = expression.split_whitespace().count();
    if fields != 5 {
        return Err(AppError::ValidationError(
            "cron expression must contain exactly 5 fields: minute hour day month weekday"
                .to_string(),
        ));
    }
    let normalized = format!("0 {expression}");
    let schedule = Schedule::from_str(&normalized)
        .map_err(|error| AppError::ValidationError(format!("invalid cron expression: {error}")))?;
    Ok((expression.to_string(), schedule))
}

fn parse_timezone(timezone: &str) -> AppResult<(String, Tz)> {
    let timezone = timezone.trim();
    let parsed = timezone
        .parse::<Tz>()
        .map_err(|_| AppError::ValidationError(format!("unknown IANA timezone: {timezone}")))?;
    Ok((timezone.to_string(), parsed))
}

fn next_run_timestamp(schedule: &Schedule, timezone: Tz, after: DateTime<Utc>) -> AppResult<i64> {
    schedule
        .after(&after.with_timezone(&timezone))
        .next()
        .map(|next| next.with_timezone(&Utc).timestamp())
        .ok_or_else(|| AppError::ValidationError("cron expression has no future run".to_string()))
}

fn timestamp_string(timestamp: Option<i64>) -> Option<String> {
    timestamp.and_then(|timestamp| {
        Utc.timestamp_opt(timestamp, 0)
            .single()
            .map(|value| value.to_rfc3339())
    })
}

fn truncate_command_output(value: &str) -> String {
    let mut chars = value.chars();
    let mut retained = chars
        .by_ref()
        .take(MAX_STORED_OUTPUT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        retained.push_str("\n[AOS output truncated after 64000 characters]");
    }
    retained
}

fn ensure_command_isolation() -> AppResult<()> {
    if runtime::sandbox_backend_capability() != runtime::EnforcementCapability::Full {
        return Err(AppError::Internal(
            "workspace command isolation is unavailable; command was not executed".to_string(),
        ));
    }
    Ok(())
}

async fn execute_workspace_command(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    input: WorkspaceCommandInput,
    cancellation: Option<Arc<AtomicBool>>,
) -> AppResult<WorkspaceCommandResult> {
    let command = validate_command(&input.command)?;
    let timeout_seconds = validate_timeout(input.timeout_seconds)?;
    let (root, cwd, normalized_cwd) =
        super::personal_workspace::resolve_workspace_directory_for_user(
            state, tenant_id, user_id, &input.cwd,
        )?;
    ensure_command_isolation()?;
    let relative_cwd = cwd.strip_prefix(&root).map_err(|_| AppError::Forbidden)?;
    let sandbox_cwd = FsPath::new("/workspace").join(relative_cwd);
    let cancellation = cancellation.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let _execution_slot = Arc::clone(command_execution_slots())
        .try_acquire_owned()
        .map_err(|_| {
            AppError::TooManyRequests(
                "workspace command concurrency limit reached; retry shortly".to_string(),
            )
        })?;
    let output = tokio::task::spawn_blocking(move || {
        runtime::execute_confined_command(
            &root,
            &sandbox_cwd,
            &command,
            Duration::from_secs(timeout_seconds),
            cancellation,
            true,
            &[],
        )
    })
    .await
    .map_err(|error| AppError::Internal(format!("workspace command worker failed: {error}")))?
    .map_err(AppError::Internal)?;
    let status = if output.cancelled {
        "cancelled"
    } else if output.timed_out {
        "timed_out"
    } else if output.exit_code == Some(0) {
        "succeeded"
    } else {
        "failed"
    };
    Ok(WorkspaceCommandResult {
        status: status.to_string(),
        exit_code: output.exit_code,
        timed_out: output.timed_out,
        cancelled: output.cancelled,
        duration_ms: output.duration_ms,
        stdout: truncate_command_output(&output.stdout),
        stderr: truncate_command_output(&output.stderr),
        cwd: normalized_cwd,
    })
}

const SCHEDULE_COLUMNS: &str = "id, tenant_id, user_id, session_id, name, script_path, command, cwd, cron_expression, timezone, timeout_seconds, enabled, status, next_run_at, last_started_at, last_finished_at, last_exit_code, last_stdout, last_stderr, run_count, lease_fencing, created_at, updated_at";

pub(crate) async fn list_schedules_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
) -> AppResult<Vec<WorkspaceSchedule>> {
    let query = format!(
        "SELECT {SCHEDULE_COLUMNS} FROM workspace_scheduled_jobs
         WHERE tenant_id = ? AND user_id = ? ORDER BY enabled DESC, updated_at DESC, id DESC"
    );
    let rows = sqlx::query_as::<_, WorkspaceScheduleRow>(sqlx::AssertSqlSafe(query))
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().map(WorkspaceSchedule::from).collect())
}

pub(crate) async fn create_schedule_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    input: CreateWorkspaceScheduleInput,
) -> AppResult<WorkspaceSchedule> {
    ensure_command_isolation()?;
    let name = validate_name(&input.name)?;
    let command = validate_command(&input.command)?;
    let timeout_seconds = validate_timeout(input.timeout_seconds)?;
    let (_, _, cwd) = super::personal_workspace::resolve_workspace_directory_for_user(
        state, tenant_id, user_id, &input.cwd,
    )?;
    let script_path = input
        .script_path
        .as_deref()
        .map(|path| {
            super::personal_workspace::validate_workspace_file_for_user(
                state, tenant_id, user_id, path,
            )
        })
        .transpose()?;
    let (cron_expression, schedule) = parse_schedule(&input.cron_expression)?;
    let (timezone, parsed_timezone) = parse_timezone(&input.timezone)?;
    let next_run_at = next_run_timestamp(&schedule, parsed_timezone, Utc::now())?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO workspace_scheduled_jobs
         (id, tenant_id, user_id, session_id, name, script_path, command, cwd,
          cron_expression, timezone, timeout_seconds, next_run_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(input.session_id.as_deref())
    .bind(name)
    .bind(script_path)
    .bind(command)
    .bind(cwd)
    .bind(cron_expression)
    .bind(timezone)
    .bind(i64::try_from(timeout_seconds).unwrap_or(120))
    .bind(next_run_at)
    .execute(&state.db)
    .await?;
    load_owned_schedule(&state.db, tenant_id, user_id, &id)
        .await
        .map(WorkspaceSchedule::from)
}

async fn load_owned_schedule(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    schedule_id: &str,
) -> AppResult<WorkspaceScheduleRow> {
    let query = format!(
        "SELECT {SCHEDULE_COLUMNS} FROM workspace_scheduled_jobs
         WHERE id = ? AND tenant_id = ? AND user_id = ?"
    );
    sqlx::query_as::<_, WorkspaceScheduleRow>(sqlx::AssertSqlSafe(query))
        .bind(schedule_id)
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::NotFound("workspace schedule not found".to_string()))
}

pub(crate) async fn update_schedule_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    schedule_id: &str,
    input: UpdateWorkspaceScheduleInput,
) -> AppResult<WorkspaceSchedule> {
    let current = load_owned_schedule(&state.db, tenant_id, user_id, schedule_id).await?;
    let name = input
        .name
        .as_deref()
        .map(validate_name)
        .transpose()?
        .unwrap_or(current.name);
    let command = input
        .command
        .as_deref()
        .map(validate_command)
        .transpose()?
        .unwrap_or(current.command);
    let cwd_input = input.cwd.unwrap_or(current.cwd);
    let (_, _, cwd) = super::personal_workspace::resolve_workspace_directory_for_user(
        state, tenant_id, user_id, &cwd_input,
    )?;
    let script_path = match input.script_path {
        Some(Some(path)) => Some(super::personal_workspace::validate_workspace_file_for_user(
            state, tenant_id, user_id, &path,
        )?),
        Some(None) => None,
        None => current.script_path,
    };
    let cron_expression = input.cron_expression.unwrap_or(current.cron_expression);
    let timezone = input.timezone.unwrap_or(current.timezone);
    let (cron_expression, schedule) = parse_schedule(&cron_expression)?;
    let (timezone, parsed_timezone) = parse_timezone(&timezone)?;
    let timeout_seconds = validate_timeout(
        input
            .timeout_seconds
            .unwrap_or_else(|| u64::try_from(current.timeout_seconds).unwrap_or(120)),
    )?;
    let enabled = input.enabled.unwrap_or(current.enabled != 0);
    if enabled {
        ensure_command_isolation()?;
    }
    let next_run_at = enabled
        .then(|| next_run_timestamp(&schedule, parsed_timezone, Utc::now()))
        .transpose()?;
    let result = sqlx::query(
        "UPDATE workspace_scheduled_jobs
         SET name = ?, script_path = ?, command = ?, cwd = ?, cron_expression = ?,
             timezone = ?, timeout_seconds = ?, enabled = ?,
             status = CASE WHEN ? = 1 THEN 'scheduled' ELSE 'cancelled' END,
             next_run_at = ?, lease_owner = NULL, lease_expires_at = NULL,
             lease_fencing = lease_fencing + 1, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(name)
    .bind(script_path)
    .bind(command)
    .bind(cwd)
    .bind(cron_expression)
    .bind(timezone)
    .bind(i64::try_from(timeout_seconds).unwrap_or(120))
    .bind(i64::from(enabled))
    .bind(i64::from(enabled))
    .bind(next_run_at)
    .bind(schedule_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::NotFound(
            "workspace schedule not found".to_string(),
        ));
    }
    cancel_active_execution(schedule_id);
    load_owned_schedule(&state.db, tenant_id, user_id, schedule_id)
        .await
        .map(WorkspaceSchedule::from)
}

pub(crate) async fn cancel_schedule_for_user(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    schedule_id: &str,
) -> AppResult<WorkspaceSchedule> {
    let result = sqlx::query(
        "UPDATE workspace_scheduled_jobs
         SET enabled = 0, status = 'cancelled', next_run_at = NULL,
             lease_owner = NULL, lease_expires_at = NULL,
             lease_fencing = lease_fencing + 1, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(schedule_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() != 1 {
        return Err(AppError::NotFound(
            "workspace schedule not found".to_string(),
        ));
    }
    cancel_active_execution(schedule_id);
    load_owned_schedule(&state.db, tenant_id, user_id, schedule_id)
        .await
        .map(WorkspaceSchedule::from)
}

pub fn start_workspace_schedule_worker(state: AppState) {
    if ensure_command_isolation().is_err() {
        tracing::warn!(
            "workspace schedule worker disabled because OS command isolation is unavailable"
        );
        return;
    }
    let worker_id = format!("workspace-schedule:{}", uuid::Uuid::new_v4());
    let slots = Arc::new(tokio::sync::Semaphore::new(WORKER_CONCURRENCY));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            loop {
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    break;
                };
                let claimed = match claim_due_schedule(&state, &worker_id).await {
                    Ok(claimed) => claimed,
                    Err(error) => {
                        tracing::warn!(%error, "workspace schedule claim failed");
                        break;
                    }
                };
                let Some(claimed) = claimed else {
                    break;
                };
                let state = state.clone();
                let worker_id = worker_id.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    run_claimed_schedule(state, worker_id, claimed).await;
                });
            }
        }
    });
}

async fn claim_due_schedule(
    state: &AppState,
    worker_id: &str,
) -> AppResult<Option<WorkspaceScheduleRow>> {
    let query = format!(
        "UPDATE workspace_scheduled_jobs
         SET status = 'running', lease_owner = ?,
             lease_expires_at = unixepoch() + timeout_seconds + 60,
             lease_fencing = lease_fencing + 1, last_started_at = unixepoch(),
             updated_at = CURRENT_TIMESTAMP
         WHERE id = (
           SELECT id FROM workspace_scheduled_jobs
           WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= unixepoch()
             AND (lease_owner IS NULL OR lease_expires_at IS NULL OR lease_expires_at < unixepoch())
           ORDER BY next_run_at ASC, id ASC LIMIT 1
         )
         RETURNING {SCHEDULE_COLUMNS}"
    );
    sqlx::query_as::<_, WorkspaceScheduleRow>(sqlx::AssertSqlSafe(query))
        .bind(worker_id)
        .fetch_optional(state.control_db())
        .await
        .map_err(AppError::Database)
}

async fn claimed_schedule_is_current(
    state: &AppState,
    schedule_id: &str,
    worker_id: &str,
    lease_fencing: i64,
) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
           SELECT 1 FROM workspace_scheduled_jobs
           WHERE id = ? AND enabled = 1 AND status = 'running'
             AND lease_owner = ? AND lease_fencing = ?
             AND lease_expires_at >= unixepoch()
         )",
    )
    .bind(schedule_id)
    .bind(worker_id)
    .bind(lease_fencing)
    .fetch_one(state.control_db())
    .await
    .map_err(AppError::Database)
}

async fn monitor_claimed_schedule(
    state: AppState,
    schedule_id: String,
    worker_id: String,
    lease_fencing: i64,
    cancellation: Arc<AtomicBool>,
    mut completed: tokio::sync::oneshot::Receiver<()>,
) {
    use std::sync::atomic::Ordering;

    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut consecutive_errors = 0_u8;
    loop {
        tokio::select! {
            _ = &mut completed => return,
            _ = ticker.tick() => {
                match claimed_schedule_is_current(
                    &state,
                    &schedule_id,
                    &worker_id,
                    lease_fencing,
                ).await {
                    Ok(true) => consecutive_errors = 0,
                    Ok(false) => {
                        cancellation.store(true, Ordering::Release);
                        return;
                    }
                    Err(error) => {
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        if consecutive_errors >= 3 {
                            tracing::warn!(
                                %schedule_id,
                                %error,
                                "workspace schedule lease monitor failed closed"
                            );
                            cancellation.store(true, Ordering::Release);
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn run_claimed_schedule(state: AppState, worker_id: String, row: WorkspaceScheduleRow) {
    let registration = ActiveExecutionRegistration::register(&row.id);
    match claimed_schedule_is_current(&state, &row.id, &worker_id, row.lease_fencing).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::warn!(
                schedule_id = %row.id,
                %error,
                "workspace schedule claim could not be verified"
            );
            return;
        }
    }
    let (monitor_done, completed) = tokio::sync::oneshot::channel();
    let monitor = tokio::spawn(monitor_claimed_schedule(
        state.clone(),
        row.id.clone(),
        worker_id.clone(),
        row.lease_fencing,
        Arc::clone(&registration.cancellation),
        completed,
    ));
    let result = execute_workspace_command(
        &state,
        &row.tenant_id,
        &row.user_id,
        WorkspaceCommandInput {
            command: row.command.clone(),
            cwd: row.cwd.clone(),
            timeout_seconds: u64::try_from(row.timeout_seconds).unwrap_or(120),
        },
        Some(Arc::clone(&registration.cancellation)),
    )
    .await;
    let _ = monitor_done.send(());
    let _ = monitor.await;
    let (status, exit_code, stdout, stderr) = match result {
        Ok(result) => (
            result.status,
            result.exit_code,
            result.stdout,
            result.stderr,
        ),
        Err(error) => ("failed".to_string(), None, String::new(), error.to_string()),
    };
    let next_run_at = parse_schedule(&row.cron_expression)
        .and_then(|(_, schedule)| {
            parse_timezone(&row.timezone)
                .and_then(|(_, timezone)| next_run_timestamp(&schedule, timezone, Utc::now()))
        })
        .ok();
    let update = sqlx::query(
        "UPDATE workspace_scheduled_jobs
         SET status = ?, next_run_at = ?, last_finished_at = unixepoch(), last_exit_code = ?,
             last_stdout = ?, last_stderr = ?, run_count = run_count + 1,
             lease_owner = NULL, lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE id = ? AND enabled = 1 AND lease_owner = ? AND lease_fencing = ?",
    )
    .bind(status)
    .bind(next_run_at)
    .bind(exit_code.map(i64::from))
    .bind(stdout)
    .bind(stderr)
    .bind(&row.id)
    .bind(&worker_id)
    .bind(row.lease_fencing)
    .execute(state.control_db())
    .await;
    if let Err(error) = update {
        tracing::warn!(schedule_id = %row.id, %error, "workspace schedule result commit failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn sqlite_test_state(db: sqlx::SqlitePool, data_dir: std::path::PathBuf) -> AppState {
        AppState {
            data_dir,
            platform_lifecycle: None,
            control_db: db.clone(),
            telemetry_db: db.clone(),
            #[cfg(feature = "pm")]
            pm_telemetry: crate::routes::agent::PmTelemetrySink::for_test(),
            db,
            jwt_secret: Arc::new(tokio::sync::RwLock::new("test".repeat(8))),
            base_url: "http://localhost".to_string(),
            default_model: "test-model".to_string(),
            setup_initialized_cache: Arc::new(AtomicBool::new(false)),
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
            nl2sql_pool_cache: Arc::new(crate::nl2sql::datasource_pool::PoolCache::new()),
            #[cfg(feature = "nl2sql")]
            nl2sql_rate_limiter: Arc::new(crate::nl2sql::rate_limiter::TenantRateLimiter::default()),
        }
    }

    #[test]
    fn five_field_cron_has_a_future_run_in_requested_timezone() {
        let (_, schedule) = parse_schedule("*/5 * * * *").expect("valid schedule");
        let (_, timezone) = parse_timezone("Asia/Shanghai").expect("valid timezone");
        let after = Utc.timestamp_opt(1_787_653_200, 0).unwrap();
        let next = next_run_timestamp(&schedule, timezone, after).expect("future occurrence");
        assert!(next > after.timestamp());
        assert_eq!(next % 300, 0);
    }

    #[test]
    fn cron_and_timezone_validation_fail_closed() {
        assert!(parse_schedule("* * * *").is_err());
        assert!(parse_schedule("invalid * * * *").is_err());
        assert!(parse_timezone("Local/Maybe").is_err());
    }

    #[test]
    fn command_output_is_bounded_for_api_and_schedule_rendering() {
        let oversized = "x".repeat(MAX_STORED_OUTPUT_CHARS + 1);
        let retained = truncate_command_output(&oversized);

        assert!(retained.starts_with(&"x".repeat(MAX_STORED_OUTPUT_CHARS)));
        assert!(retained.ends_with("[AOS output truncated after 64000 characters]"));
    }

    #[test]
    fn dropping_an_http_command_request_requests_process_cancellation() {
        let request = RequestExecutionCancellation::new();
        let cancellation = Arc::clone(&request.cancellation);

        drop(request);

        assert!(cancellation.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn durable_schedule_executes_updates_and_cancels_inside_owned_workspace() {
        if runtime::sandbox_backend_capability() != runtime::EnforcementCapability::Full {
            return;
        }
        let db = crate::test_sqlite_pool().await;
        let data_dir = std::env::temp_dir().join(format!(
            "aos-workspace-schedule-test-{}",
            uuid::Uuid::new_v4()
        ));
        let state = sqlite_test_state(db.clone(), data_dir.clone());
        let workspace = super::super::personal_workspace::ensure_workspace_root_for_user(
            &state, "tenant-a", "user-a",
        )
        .expect("create workspace");

        let created = create_schedule_for_user(
            &state,
            "tenant-a",
            "user-a",
            CreateWorkspaceScheduleInput {
                name: "minute report".to_string(),
                command: "printf 'first-run' > scheduled-output.txt".to_string(),
                cwd: DEFAULT_CWD.to_string(),
                cron_expression: "* * * * *".to_string(),
                timezone: "UTC".to_string(),
                timeout_seconds: 10,
                script_path: None,
                session_id: Some("session-a".to_string()),
            },
        )
        .await
        .expect("create schedule");
        sqlx::query(
            "UPDATE workspace_scheduled_jobs SET next_run_at = unixepoch() - 1 WHERE id = ?",
        )
        .bind(&created.id)
        .execute(state.control_db())
        .await
        .expect("make schedule due");

        let claimed = claim_due_schedule(&state, "test-worker")
            .await
            .expect("claim due schedule")
            .expect("due schedule");
        assert_eq!(claimed.id, created.id);
        run_claimed_schedule(state.clone(), "test-worker".to_string(), claimed).await;
        assert_eq!(
            std::fs::read_to_string(workspace.join("scheduled-output.txt"))
                .expect("scheduled output"),
            "first-run"
        );
        let executed = load_owned_schedule(&db, "tenant-a", "user-a", &created.id)
            .await
            .expect("load executed schedule");
        assert_eq!(executed.status, "succeeded");
        assert_eq!(executed.run_count, 1);
        assert!(executed.next_run_at.is_some());

        let cancellable = create_schedule_for_user(
            &state,
            "tenant-a",
            "user-a",
            CreateWorkspaceScheduleInput {
                name: "cancellable report".to_string(),
                command: "printf started > cancellation-started.txt; sleep 30".to_string(),
                cwd: DEFAULT_CWD.to_string(),
                cron_expression: "* * * * *".to_string(),
                timezone: "UTC".to_string(),
                timeout_seconds: 60,
                script_path: None,
                session_id: Some("session-a".to_string()),
            },
        )
        .await
        .expect("create cancellable schedule");
        sqlx::query(
            "UPDATE workspace_scheduled_jobs SET next_run_at = unixepoch() - 1 WHERE id = ?",
        )
        .bind(&cancellable.id)
        .execute(state.control_db())
        .await
        .expect("make cancellable schedule due");
        let cancellable_claim = claim_due_schedule(&state, "remote-worker")
            .await
            .expect("claim cancellable schedule")
            .expect("cancellable schedule is due");
        let execution = tokio::spawn(run_claimed_schedule(
            state.clone(),
            "remote-worker".to_string(),
            cancellable_claim,
        ));
        let marker = workspace.join("cancellation-started.txt");
        tokio::time::timeout(Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("scheduled command should start");
        sqlx::query(
            "UPDATE workspace_scheduled_jobs
             SET enabled = 0, status = 'cancelled', next_run_at = NULL,
                 lease_owner = NULL, lease_expires_at = NULL,
                 lease_fencing = lease_fencing + 1
             WHERE id = ?",
        )
        .bind(&cancellable.id)
        .execute(state.control_db())
        .await
        .expect("simulate cancellation from another server instance");
        let cancellation_started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(2), execution)
            .await
            .expect("revoked lease should terminate the command within two seconds")
            .expect("schedule execution task should not panic");
        assert!(cancellation_started.elapsed() < Duration::from_secs(2));
        let cancelled_remotely = load_owned_schedule(&db, "tenant-a", "user-a", &cancellable.id)
            .await
            .expect("load remotely cancelled schedule");
        assert_eq!(cancelled_remotely.status, "cancelled");
        assert_eq!(cancelled_remotely.run_count, 0);

        let registration = ActiveExecutionRegistration::register(&created.id);
        let unauthorized =
            cancel_schedule_for_user(&state, "tenant-b", "user-b", &created.id).await;
        assert!(matches!(unauthorized, Err(AppError::NotFound(_))));
        assert!(!registration.cancellation.load(Ordering::Acquire));
        drop(registration);

        let updated = update_schedule_for_user(
            &state,
            "tenant-a",
            "user-a",
            &created.id,
            UpdateWorkspaceScheduleInput {
                cron_expression: Some("*/5 * * * *".to_string()),
                timezone: Some("Asia/Shanghai".to_string()),
                ..UpdateWorkspaceScheduleInput::default()
            },
        )
        .await
        .expect("update schedule");
        assert_eq!(updated.cron_expression, "*/5 * * * *");
        assert_eq!(updated.timezone, "Asia/Shanghai");
        assert!(updated.enabled);

        let cancelled = cancel_schedule_for_user(&state, "tenant-a", "user-a", &created.id)
            .await
            .expect("cancel schedule");
        assert!(!cancelled.enabled);
        assert_eq!(cancelled.status, "cancelled");
        assert!(cancelled.next_run_at.is_none());

        db.close().await;
        let _ = std::fs::remove_dir_all(data_dir);
    }
}
