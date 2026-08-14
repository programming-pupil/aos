use super::*;
use sqlx::{sqlite::SqliteRow, Row};

#[derive(Debug, Clone)]
pub(super) struct PmTaskRuntimeRow {
    pub(super) task_id: String,
    pub(super) tenant_id: String,
    pub(super) user_id: String,
    pub(super) session_id: String,
    pub(super) message: String,
    pub(super) input_context: Option<PmTaskInputContext>,
    pub(super) status: String,
    pub(super) cancel_requested: bool,
    pub(super) event_seq: u64,
    pub(super) stage: Option<String>,
    pub(super) attempt: Option<usize>,
    pub(super) elapsed_ms: u64,
    pub(super) stage_elapsed_ms: Option<u64>,
    pub(super) detail: Option<serde_json::Value>,
    pub(super) response: Option<serde_json::Value>,
    pub(super) error: Option<String>,
}

fn pm_json_to_opt_string(value: Option<&serde_json::Value>) -> Option<String> {
    value.map(serde_json::Value::to_string).map(|value| {
        runtime::protect_sensitive_text(&value, runtime::configured_data_protection_mode()).value
    })
}

fn pm_input_context_to_opt_string(value: Option<&PmTaskInputContext>) -> Option<String> {
    value
        .and_then(|ctx| serde_json::to_string(ctx).ok())
        .map(|value| {
            runtime::protect_sensitive_text(&value, runtime::configured_data_protection_mode())
                .value
        })
}

pub(super) fn pm_task_worker_id() -> &'static str {
    static WORKER_ID: OnceLock<String> = OnceLock::new();
    WORKER_ID.get_or_init(|| {
        let host = env::var("HOSTNAME")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "aos-single-node".to_string());
        format!("{host}:pid{}", std::process::id())
    })
}

pub(super) fn pm_task_is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

pub(super) fn pm_task_event_is_terminal(evt: &PmResearchTaskEvent) -> bool {
    if matches!(evt.stage.as_deref(), Some("done" | "failed" | "cancelled")) {
        return true;
    }
    evt.status == "completed" && evt.response.is_some()
}

pub(super) fn pm_task_research_config() -> &'static PmResearchTaskConfig {
    static CONFIG: OnceLock<PmResearchTaskConfig> = OnceLock::new();
    CONFIG.get_or_init(PmResearchTaskConfig::from_env)
}

fn build_pm_task_checkpoint(
    evt: &PmResearchTaskEvent,
    cancel_requested: bool,
    done: bool,
) -> serde_json::Value {
    serde_json::json!({
        "status": evt.status,
        "stage": evt.stage,
        "attempt": evt.attempt,
        "elapsedMs": evt.elapsed_ms,
        "stageElapsedMs": evt.stage_elapsed_ms,
        "cancelRequested": cancel_requested,
        "done": done,
        "detail": evt.detail,
        "error": evt.error,
        "hasResponse": evt.response.is_some(),
    })
}

pub(super) async fn persist_pm_task_record_and_event(
    db: &sqlx::SqlitePool,
    telemetry: &PmTelemetrySink,
    rec: &PmResearchTaskRecord,
    evt: &PmResearchTaskEvent,
) {
    let detail_json = pm_json_to_opt_string(evt.detail.as_ref());
    let response_json = pm_json_to_opt_string(evt.response.as_ref());
    let input_context_json = pm_input_context_to_opt_string(rec.input_context.as_ref());
    let checkpoint_json = runtime::protect_sensitive_text(
        &build_pm_task_checkpoint(evt, rec.cancel_requested, rec.done).to_string(),
        runtime::configured_data_protection_mode(),
    )
    .value;
    let attempt_i32 = evt.attempt.and_then(|v| i32::try_from(v).ok());
    let elapsed_i64 = i64::try_from(evt.elapsed_ms).unwrap_or(i64::MAX);
    let stage_elapsed_i64 = evt.stage_elapsed_ms.and_then(|v| i64::try_from(v).ok());
    let seq_i64 = i64::try_from(rec.event_seq).unwrap_or(i64::MAX);
    let task_status = if rec.done {
        evt.status.as_str()
    } else if rec.cancel_requested && evt.status != "cancelled" {
        "cancelling"
    } else if evt.status == "queued" {
        "queued"
    } else {
        "running"
    };
    let upsert_result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO pm_research_tasks
            (task_id, tenant_id, user_id, session_id, message, input_context_json, status, stage, attempt,
             elapsed_ms, stage_elapsed_ms, detail_json, response_json, error_message,
             cancel_requested, event_seq, checkpoint_json, completed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 CASE WHEN ? IN ('completed','failed','cancelled') THEN CURRENT_TIMESTAMP ELSE NULL END)
         ON CONFLICT DO UPDATE SET
            status = excluded.status,
            stage = excluded.stage,
            attempt = excluded.attempt,
            elapsed_ms = excluded.elapsed_ms,
            stage_elapsed_ms = excluded.stage_elapsed_ms,
            input_context_json = excluded.input_context_json,
            detail_json = excluded.detail_json,
            response_json = excluded.response_json,
            error_message = excluded.error_message,
            cancel_requested = excluded.cancel_requested,
            event_seq = excluded.event_seq,
            checkpoint_json = excluded.checkpoint_json,
            completed_at = CASE
                WHEN excluded.status IN ('completed','failed','cancelled') THEN COALESCE(completed_at, CURRENT_TIMESTAMP)
                ELSE NULL
            END,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&evt.task_id)
    .bind(&rec.tenant_id)
    .bind(&rec.user_id)
    .bind(&rec.session_id)
    .bind(&rec.message)
    .bind(input_context_json)
    .bind(task_status)
    .bind(evt.stage.as_deref())
    .bind(attempt_i32)
    .bind(elapsed_i64)
    .bind(stage_elapsed_i64)
    .bind(detail_json.clone())
    .bind(response_json.clone())
    .bind(evt.error.clone())
    .bind(rec.cancel_requested)
    .bind(seq_i64)
    .bind(checkpoint_json)
    .bind(task_status)
    .execute(db)
    .await;

    if let Err(error) = upsert_result {
        tracing::warn!(
            task_id = %evt.task_id,
            "failed to persist pm_research_tasks snapshot: {}",
            error
        );
        return;
    }

    // The PM legacy snapshot remains available for compatibility, but every
    // event also enters the durable semantic-kernel ledger and the stage
    // projection.  A migration race must not make an otherwise usable answer
    // disappear, so legacy persistence is retained as a guarded fallback.
    if let Err(error) = crate::semantic_kernel_store::append_pm_stage_event(
        db,
        &rec.tenant_id,
        &rec.user_id,
        &rec.session_id,
        &evt.task_id,
        rec.event_seq,
        serde_json::to_value(evt).unwrap_or(serde_json::Value::Null),
        evt.stage.as_deref().unwrap_or("running"),
        &evt.status,
        evt.attempt.unwrap_or(1),
        evt.detail.clone(),
    )
    .await
    {
        tracing::warn!(
            task_id = %evt.task_id,
            error = %error,
            "semantic-kernel PM event append failed; legacy PM snapshot remains authoritative for recovery"
        );
    }

    telemetry
        .enqueue(PmTelemetryEvent::TaskEvent {
            tenant_id: rec.tenant_id.clone(),
            user_id: rec.user_id.clone(),
            sequence: rec.event_seq,
            event: evt.clone(),
        })
        .await;
}

fn parse_optional_json(input: Option<String>) -> Option<serde_json::Value> {
    input.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            serde_json::from_str::<serde_json::Value>(trimmed).ok()
        }
    })
}

fn normalize_optional_json_value(input: Option<serde_json::Value>) -> Option<serde_json::Value> {
    input.and_then(|value| match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(trimmed)
                    .ok()
                    .or(Some(serde_json::Value::String(raw)))
            }
        }
        other => Some(other),
    })
}

fn parse_pm_input_context_value(input: Option<serde_json::Value>) -> Option<PmTaskInputContext> {
    input.and_then(|value| serde_json::from_value::<PmTaskInputContext>(value).ok())
}

fn pm_task_runtime_row_from_sql(row: &SqliteRow) -> Result<PmTaskRuntimeRow, sqlx::Error> {
    Ok(PmTaskRuntimeRow {
        task_id: row.try_get::<String, _>(0)?,
        tenant_id: row.try_get::<String, _>(1)?,
        user_id: row.try_get::<String, _>(2)?,
        session_id: row.try_get::<String, _>(3)?,
        message: row.try_get::<String, _>(4)?,
        input_context: parse_pm_input_context_value(try_get_optional_json_field(row, 5)?),
        status: row.try_get::<String, _>(6)?,
        cancel_requested: row.try_get::<bool, _>(7)?,
        event_seq: row.try_get::<u64, _>(8)?,
        stage: row.try_get::<Option<String>, _>(9)?,
        attempt: row
            .try_get::<Option<i32>, _>(10)?
            .and_then(|x| usize::try_from(x).ok()),
        elapsed_ms: row.try_get::<u64, _>(11)?,
        stage_elapsed_ms: row.try_get::<Option<u64>, _>(12)?,
        detail: try_get_optional_json_field(row, 13)?,
        response: try_get_optional_json_field(row, 14)?,
        error: row.try_get::<Option<String>, _>(15)?,
    })
}

pub(super) fn try_get_optional_json_field(
    row: &SqliteRow,
    index: usize,
) -> Result<Option<serde_json::Value>, sqlx::Error> {
    match row.try_get::<Option<serde_json::Value>, _>(index) {
        Ok(value) => Ok(normalize_optional_json_value(value)),
        Err(sqlx::Error::ColumnDecode { .. }) => {
            let raw = row.try_get::<Option<String>, _>(index)?;
            Ok(parse_optional_json(raw))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn build_pm_task_record_from_runtime_row(
    row: &PmTaskRuntimeRow,
) -> PmResearchTaskRecord {
    let now = Instant::now();
    let last_event = PmResearchTaskEvent {
        task_id: row.task_id.clone(),
        session_id: row.session_id.clone(),
        status: row.status.clone(),
        stage: row.stage.clone(),
        attempt: row.attempt,
        message: None,
        elapsed_ms: row.elapsed_ms,
        stage_elapsed_ms: row.stage_elapsed_ms,
        detail: row.detail.clone(),
        response: row.response.clone(),
        error: row.error.clone(),
    };
    PmResearchTaskRecord {
        tenant_id: row.tenant_id.clone(),
        user_id: row.user_id.clone(),
        session_id: row.session_id.clone(),
        message: row.message.clone(),
        input_context: row.input_context.clone(),
        created_at: now,
        last_update_at: now,
        stage_started_at: now,
        completed_at: if pm_task_is_terminal_status(&row.status) {
            Some(now)
        } else {
            None
        },
        execution_active: false,
        done: pm_task_is_terminal_status(&row.status),
        cancel_requested: row.cancel_requested,
        last_event,
        event_seq: row.event_seq.max(1),
        answer_stream_seq: 0,
    }
}

#[derive(Debug, Clone)]
pub(super) struct PmResumeCheckpoint {
    pub(super) previous_task_id: String,
    pub(super) status: String,
    pub(super) stage: Option<String>,
    pub(super) attempt: usize,
    pub(super) detail: Option<serde_json::Value>,
    pub(super) planner_plan: Option<serde_json::Value>,
}

fn is_pm_resumable_checkpoint_stage(stage: &str) -> bool {
    matches!(
        stage,
        "preflight"
            | "understand"
            | "task_plan"
            | "planner"
            | "retrieve"
            | "verify"
            | "retry_repair"
            | "synthesize"
    )
}

fn parse_pm_resume_checkpoint(
    task_id: &str,
    status: &str,
    checkpoint_value: Option<serde_json::Value>,
    planner_plan: Option<serde_json::Value>,
) -> Option<PmResumeCheckpoint> {
    let checkpoint_value = checkpoint_value?;
    let stage = checkpoint_value
        .get("stage")
        .and_then(|v| v.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| !raw.is_empty())?;
    if !is_pm_resumable_checkpoint_stage(&stage) {
        return None;
    }
    let checkpoint_status = checkpoint_value
        .get("status")
        .and_then(|v| v.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        checkpoint_status.as_str(),
        "queued" | "completed" | "failed" | "cancelled"
    ) {
        return None;
    }
    let done = checkpoint_value
        .get("done")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_response = checkpoint_value
        .get("hasResponse")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let cancel_requested = checkpoint_value
        .get("cancelRequested")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if done || has_response || cancel_requested {
        return None;
    }
    let attempt = checkpoint_value
        .get("attempt")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(1)
        .max(1);
    let detail = checkpoint_value.get("detail").cloned();
    let planner_plan_from_checkpoint = checkpoint_value.get("plannerPlan").cloned();
    Some(PmResumeCheckpoint {
        previous_task_id: task_id.to_string(),
        status: status.to_string(),
        stage: Some(stage),
        attempt,
        detail,
        planner_plan: planner_plan_from_checkpoint.or(planner_plan),
    })
}

pub(super) async fn load_pm_task_snapshot_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<(PmResearchTaskEvent, bool)>, AppError> {
    let row_opt = sqlx::query::<sqlx::Sqlite>(
        "SELECT t.session_id, t.status, t.stage, t.attempt,
                COALESCE(
                    (
                        SELECT e.message
                        FROM pm_research_task_events e
                        WHERE e.task_id = t.task_id
                          AND e.message IS NOT NULL
                          AND TRIM(e.message) <> ''
                        ORDER BY e.seq DESC, e.id DESC
                        LIMIT 1
                    ),
                    t.message
                ) AS status_message,
                CAST(CASE
                    WHEN t.completed_at IS NULL AND t.created_at IS NOT NULL
                    THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(t.created_at)) * 86400000000) / 1000, t.elapsed_ms)
                    ELSE t.elapsed_ms
                END AS INTEGER) AS live_elapsed_ms,
                CAST(CASE
                    WHEN t.completed_at IS NULL
                    THEN COALESCE(
                        (
                            SELECT MAX(
                                ((julianday(CURRENT_TIMESTAMP) - julianday(sa.started_at)) * 86400000000) / 1000,
                                COALESCE(t.stage_elapsed_ms, 0)
                            )
                            FROM pm_research_stage_attempts sa
                            WHERE sa.run_id = t.task_id
                              AND sa.stage = t.stage
                              AND sa.attempt_no = t.attempt
                            ORDER BY sa.id DESC
                            LIMIT 1
                        ),
                        t.stage_elapsed_ms
                    )
                    ELSE t.stage_elapsed_ms
                END AS INTEGER) AS live_stage_elapsed_ms,
                t.detail_json, t.response_json, t.error_message, t.cancel_requested
         FROM pm_research_tasks t
         WHERE t.task_id = ? AND t.tenant_id = ? AND t.user_id = ?",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    let Some(row) = row_opt else {
        return Ok(None);
    };
    let session_id = row.try_get::<String, _>(0)?;
    let status = row.try_get::<String, _>(1)?;
    let stage = row.try_get::<Option<String>, _>(2)?;
    let attempt = row
        .try_get::<Option<i32>, _>(3)?
        .and_then(|v| usize::try_from(v).ok());
    let message = row.try_get::<Option<String>, _>(4)?;
    let elapsed_ms = row.try_get::<u64, _>(5)?;
    let stage_elapsed_ms = row.try_get::<Option<u64>, _>(6)?;
    let detail = try_get_optional_json_field(&row, 7)?;
    let response = try_get_optional_json_field(&row, 8)?;
    let error = row.try_get::<Option<String>, _>(9)?;
    let cancel_requested = row.try_get::<bool, _>(10)?;

    Ok(Some((
        PmResearchTaskEvent {
            task_id: task_id.to_string(),
            session_id,
            status,
            stage,
            attempt,
            message,
            elapsed_ms,
            stage_elapsed_ms,
            detail,
            response,
            error,
        },
        cancel_requested,
    )))
}

pub(super) async fn load_pm_task_runtime_row_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
) -> Result<Option<PmTaskRuntimeRow>, AppError> {
    let row_opt = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, tenant_id, user_id, session_id, message, input_context_json, status, cancel_requested,
                event_seq, stage, attempt, elapsed_ms, stage_elapsed_ms,
                detail_json, response_json, error_message
         FROM pm_research_tasks
         WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row_opt else {
        return Ok(None);
    };
    Ok(Some(pm_task_runtime_row_from_sql(&row)?))
}

pub(super) async fn load_claimable_pm_task_runtime_rows(
    db: &sqlx::SqlitePool,
    _worker_id: &str,
    limit: usize,
    per_tenant_limit: usize,
) -> Result<Vec<PmTaskRuntimeRow>, AppError> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        "WITH active_by_tenant AS (
           SELECT tenant_id, COUNT(*) AS active_count
           FROM pm_research_tasks
           WHERE completed_at IS NULL
             AND cancel_requested = 0
             AND status IN ('running','cancelling','interrupted')
             AND lease_owner IS NOT NULL
             AND lease_expires_at IS NOT NULL
             AND lease_expires_at >= CURRENT_TIMESTAMP
           GROUP BY tenant_id
         )
         SELECT task_id, tenant_id, user_id, session_id, message, input_context_json, status,
                cancel_requested, event_seq, stage, attempt, elapsed_ms, stage_elapsed_ms,
                detail_json, response_json, error_message
         FROM (
           SELECT task_id, tenant_id, user_id, session_id, message, input_context_json, status,
                  cancel_requested, event_seq, stage, attempt, elapsed_ms, stage_elapsed_ms,
                  detail_json, response_json, error_message, updated_at,
                  ROW_NUMBER() OVER (
                    PARTITION BY tenant_id ORDER BY updated_at ASC, task_id ASC
                  ) AS tenant_rank
           FROM pm_research_tasks
           WHERE completed_at IS NULL
             AND cancel_requested = 0
             AND status IN ('queued','running','cancelling','interrupted')
             AND (lease_expires_at IS NULL OR lease_expires_at < CURRENT_TIMESTAMP)
         ) ranked
         LEFT JOIN active_by_tenant active USING (tenant_id)
         WHERE tenant_rank + COALESCE(active.active_count, 0) <= ?
         ORDER BY updated_at ASC, task_id ASC
         LIMIT ?",
    )
    .bind(i64::try_from(per_tenant_limit.max(1)).unwrap_or(i64::MAX))
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(db)
    .await?;

    let candidates = rows
        .iter()
        .map(|row| pm_task_runtime_row_from_sql(row).map_err(AppError::Database))
        .collect::<Result<Vec<_>, _>>()?;
    let mut claimable = Vec::with_capacity(candidates.len());
    for row in candidates {
        match crate::semantic_kernel_store::repair_ledger_thread(db, &row.tenant_id, &row.task_id)
            .await
        {
            Ok(_) => claimable.push(row),
            Err(error) => {
                tracing::error!(
                    task_id = %row.task_id,
                    tenant_id = %row.tenant_id,
                    error = %error,
                    "quarantining PM task with a corrupt semantic-kernel ledger"
                );
                sqlx::query::<sqlx::Sqlite>(
                    "UPDATE pm_research_tasks
                     SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                         error_message = ?, updated_at = CURRENT_TIMESTAMP
                     WHERE task_id = ? AND tenant_id = ?",
                )
                .bind(format!("semantic-kernel ledger corruption: {error}"))
                .bind(&row.task_id)
                .bind(&row.tenant_id)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(claimable)
}

pub(super) async fn try_claim_pm_task_lease(
    db: &sqlx::SqlitePool,
    row: &PmTaskRuntimeRow,
    worker_id: &str,
    lease_secs: u64,
) -> Result<bool, AppError> {
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE pm_research_tasks
         SET lease_owner = ?,
             lease_expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)),
             heartbeat_at = CURRENT_TIMESTAMP,
             lock_version = lock_version + 1,
             status = CASE WHEN status = 'queued' THEN 'running' ELSE status END,
             updated_at = CURRENT_TIMESTAMP
         WHERE task_id = ?
           AND tenant_id = ?
           AND user_id = ?
           AND completed_at IS NULL
           AND cancel_requested = 0
           AND status IN ('queued','running','cancelling','interrupted')
           AND (lease_expires_at IS NULL OR lease_expires_at < CURRENT_TIMESTAMP)",
    )
    .bind(worker_id)
    .bind(i64::try_from(lease_secs).unwrap_or(i64::MAX))
    .bind(&row.task_id)
    .bind(&row.tenant_id)
    .bind(&row.user_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(super) async fn has_running_pm_task_for_session(
    db: &sqlx::SqlitePool,
    row: &PmTaskRuntimeRow,
) -> Result<bool, AppError> {
    let count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COUNT(*)
         FROM pm_research_tasks
         WHERE tenant_id = ?
           AND user_id = ?
           AND session_id = ?
           AND task_id <> ?
           AND completed_at IS NULL
           AND status IN ('running','cancelling','interrupted')
           AND lease_owner IS NOT NULL
           AND lease_expires_at IS NOT NULL
           AND lease_expires_at >= CURRENT_TIMESTAMP",
    )
    .bind(&row.tenant_id)
    .bind(&row.user_id)
    .bind(&row.session_id)
    .bind(&row.task_id)
    .fetch_one(db)
    .await?;
    Ok(count > 0)
}

pub(super) async fn touch_pm_task_lease(
    db: &sqlx::SqlitePool,
    task_id: &str,
    worker_id: &str,
    lease_secs: u64,
) -> Result<bool, AppError> {
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE pm_research_tasks
         SET lease_expires_at = datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)),
             heartbeat_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP,
             elapsed_ms = CASE
                WHEN created_at IS NOT NULL
                THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400000000) / 1000, elapsed_ms)
                ELSE elapsed_ms
             END
         WHERE task_id = ?
           AND completed_at IS NULL
           AND cancel_requested = 0
           AND lease_owner = ?
           AND (
                NOT EXISTS (
                    SELECT 1 FROM pm_research_runs r
                    WHERE r.run_id = pm_research_tasks.task_id
                )
                OR EXISTS (
                    SELECT 1 FROM pm_research_runs r
                    WHERE r.run_id = pm_research_tasks.task_id
                      AND (
                          r.deadline_at IS NULL
                          OR r.status IN ('completed','failed','cancelled')
                          OR r.deadline_at > CURRENT_TIMESTAMP
                      )
                )
           )",
    )
    .bind(i64::try_from(lease_secs).unwrap_or(i64::MAX))
    .bind(task_id)
    .bind(worker_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(super) async fn pm_task_deadline_elapsed(
    db: &sqlx::SqlitePool,
    task_id: &str,
) -> Result<bool, AppError> {
    let elapsed = sqlx::query_scalar::<sqlx::Sqlite, bool>(
        "SELECT COALESCE(
            MAX(
                status = 'running'
                AND ended_at IS NULL
                AND deadline_at IS NOT NULL
                AND deadline_at <= CURRENT_TIMESTAMP
            ),
            0
         )
         FROM pm_research_runs
         WHERE run_id = ?",
    )
    .bind(task_id)
    .fetch_one(db)
    .await?;
    Ok(elapsed)
}

pub(super) async fn force_finish_elapsed_pm_task_deadline(
    state: &AppState,
    task_id: &str,
    error_message: &str,
) -> Result<bool, AppError> {
    let Some(row) = load_pm_task_runtime_row_from_db(state.control_db(), task_id).await? else {
        return Ok(false);
    };
    complete_pm_task_with_local_recovery_row(
        state,
        &row,
        error_message,
        "deadline_soft_delivered",
        true,
    )
    .await
}

pub(super) async fn complete_pm_task_with_local_recovery(
    state: &AppState,
    task_id: &str,
    reason: &str,
    recovery_code: &str,
) -> Result<bool, AppError> {
    let Some(row) = load_pm_task_runtime_row_from_db(state.control_db(), task_id).await? else {
        return Ok(false);
    };
    complete_pm_task_with_local_recovery_row(state, &row, reason, recovery_code, false).await
}

async fn complete_pm_task_with_local_recovery_row(
    state: &AppState,
    row: &PmTaskRuntimeRow,
    reason: &str,
    recovery_code: &str,
    require_elapsed_deadline: bool,
) -> Result<bool, AppError> {
    let db = state.control_db();
    let response = build_pm_local_recovery_response(row, reason, recovery_code);
    let response_json = serde_json::to_string(&response).ok();
    let detail = serde_json::json!({
        "event": "pm.task.local_recovery_delivery",
        "recoveryMode": "local_first_party_synthesis",
        "recoveryCode": recovery_code,
        "reason": reason,
    });
    let detail_json = detail.to_string();
    let checkpoint_json = serde_json::json!({
        "status": "completed",
        "stage": "done",
        "attempt": row.attempt,
        "elapsedMs": row.elapsed_ms,
        "stageElapsedMs": row.stage_elapsed_ms,
        "cancelRequested": row.cancel_requested,
        "done": true,
        "detail": detail,
        "error": null,
        "hasResponse": response_json.is_some(),
    })
    .to_string();
    let run_metadata = serde_json::json!({
        "backgroundRecovery": true,
        "recoveryMode": recovery_code,
        "answerTextPresent": !response.text.trim().is_empty(),
        "citationCount": response
            .pm_quality
            .as_ref()
            .map(|quality| quality.citation_count)
            .unwrap_or(0),
        "domainCount": response
            .pm_quality
            .as_ref()
            .map(|quality| quality.domain_count)
            .unwrap_or(0),
    })
    .to_string();
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE pm_research_tasks t
         SET t.status = 'completed',
             t.stage = 'done',
             t.detail_json = ?,
             t.response_json = ?,
             t.error_message = NULL,
             t.event_seq = t.event_seq + 1,
             t.checkpoint_json = ?,
             t.completed_at = COALESCE(t.completed_at, CURRENT_TIMESTAMP),
             t.lease_owner = NULL,
             t.lease_expires_at = NULL,
             t.heartbeat_at = CURRENT_TIMESTAMP,
             t.updated_at = CURRENT_TIMESTAMP
         WHERE t.task_id = ?
           AND t.completed_at IS NULL
           AND (
                ? = 0
                OR EXISTS (
                    SELECT 1
                    FROM pm_research_runs r
                    WHERE r.run_id = t.task_id
                      AND r.status = 'running'
                      AND r.ended_at IS NULL
                      AND r.deadline_at IS NOT NULL
                      AND r.deadline_at <= CURRENT_TIMESTAMP
                )
           )",
    )
    .bind(&detail_json)
    .bind(response_json.as_deref())
    .bind(&checkpoint_json)
    .bind(&row.task_id)
    .bind(require_elapsed_deadline)
    .execute(db)
    .await?;
    let updated = result.rows_affected() > 0;
    if updated {
        let _ = sqlx::query::<sqlx::Sqlite>(
            "UPDATE pm_research_runs
             SET status = 'completed',
                 current_stage = 'synthesize',
                 error_code = ?,
                 error_message = ?,
                 final_quality_score = ?,
                 metadata_json = ?,
                 ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?
               AND status NOT IN ('completed','cancelled')",
        )
        .bind(recovery_code)
        .bind(reason)
        .bind(response.pm_quality.as_ref().map(pm_quality_delivery_score))
        .bind(&run_metadata)
        .bind(&row.task_id)
        .execute(db)
        .await;

        let seq_i64 = i64::try_from(row.event_seq.saturating_add(1)).unwrap_or(i64::MAX);
        let attempt_i32 = row.attempt.and_then(|value| i32::try_from(value).ok());
        let elapsed_i64 = i64::try_from(row.elapsed_ms).unwrap_or(i64::MAX);
        let stage_elapsed_i64 = row
            .stage_elapsed_ms
            .and_then(|value| i64::try_from(value).ok());
        let _ = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_task_events
                (task_id, tenant_id, user_id, seq, status, stage, attempt, message,
                 elapsed_ms, stage_elapsed_ms, detail_json, response_json, error_message)
             VALUES (?, ?, ?, ?, 'completed', 'done', ?, ?, ?, ?, ?, ?, NULL)
             ON CONFLICT DO UPDATE SET
                status = excluded.status,
                stage = excluded.stage,
                attempt = excluded.attempt,
                message = excluded.message,
                elapsed_ms = excluded.elapsed_ms,
                stage_elapsed_ms = excluded.stage_elapsed_ms,
                detail_json = excluded.detail_json,
                response_json = excluded.response_json,
                error_message = NULL",
        )
        .bind(&row.task_id)
        .bind(&row.tenant_id)
        .bind(&row.user_id)
        .bind(seq_i64)
        .bind(attempt_i32)
        .bind("已生成可用结论")
        .bind(elapsed_i64)
        .bind(stage_elapsed_i64)
        .bind(&detail_json)
        .bind(response_json.as_deref())
        .execute(db)
        .await;

        let live_response = build_pm_local_recovery_response(row, reason, recovery_code);
        pm_research_task_manager()
            .publish_completed(db, state.pm_telemetry(), &row.task_id, live_response)
            .await;
        super::agent_pm_task_api::persist_pm_terminal_answer_to_runtime_session(
            get_agent_manager(state),
            &row.tenant_id,
            &row.user_id,
            &row.session_id,
            &row.task_id,
            &response.text,
        )
        .await;
        let archive_user_message = super::agent_pm_task_api::pm_task_archival_user_message(
            row.input_context.as_ref(),
            &row.message,
        );
        crate::routes::memory_continuity::persist_agent_turn_exact_archive(
            db,
            &row.tenant_id,
            &row.user_id,
            &row.session_id,
            "pm",
            &row.task_id,
            &archive_user_message,
            None,
            &response.text,
            &[],
            Some(serde_json::json!({
                "source": "pm_local_recovery",
                "taskId": row.task_id,
                "recoveryMode": recovery_code,
                "reason": reason
            })),
        )
        .await;
    }
    Ok(updated)
}

fn build_pm_local_recovery_response(
    row: &PmTaskRuntimeRow,
    reason: &str,
    recovery_code: &str,
) -> RunTurnResponse {
    let model = "pm-local-first-party-synthesis";
    let turn = build_pm_local_strategy_synthesis_turn(
        &row.session_id,
        model,
        &row.message,
        reason,
        row.attempt.unwrap_or(1).max(1),
        &[],
    );
    let mut quality = degrade_pm_quality_with_reason(
        evaluate_pm_answer_quality(&turn),
        recovery_code,
        "AOS delivered a first-party grounded synthesis instead of leaving the PM task without a useful answer.",
    );
    quality.deliverable = true;
    let visible_message = super::agent_pm_task_api::pm_task_archival_user_message(
        row.input_context.as_ref(),
        &row.message,
    );
    turn_to_run_turn_response(turn, Some(quality), Some(&visible_message))
}

pub(super) async fn release_pm_task_lease(
    db: &sqlx::SqlitePool,
    task_id: &str,
    worker_id: &str,
    keep_owner: bool,
) -> Result<(), AppError> {
    let query = if keep_owner {
        "UPDATE pm_research_tasks
         SET lease_expires_at = NULL,
             heartbeat_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE task_id = ? AND lease_owner = ?"
    } else {
        "UPDATE pm_research_tasks
         SET lease_owner = NULL,
             lease_expires_at = NULL,
             heartbeat_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE task_id = ? AND lease_owner = ?"
    };
    let _ = sqlx::query::<sqlx::Sqlite>(query)
        .bind(task_id)
        .bind(worker_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn load_pm_resume_planner_plan_from_db(
    db: &sqlx::SqlitePool,
    run_id: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    let row_opt = sqlx::query::<sqlx::Sqlite>(
        "SELECT detail_json
         FROM pm_research_stage_attempts
         WHERE run_id = ? AND stage = 'planner' AND status = 'completed' AND detail_json IS NOT NULL
         ORDER BY attempt_no DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row_opt else {
        return Ok(None);
    };
    let Some(detail_value) = try_get_optional_json_field(&row, 0)? else {
        return Ok(None);
    };
    Ok(detail_value.get("plan").cloned())
}

async fn load_pm_resume_checkpoint_from_stage_attempts(
    db: &sqlx::SqlitePool,
    run_id: &str,
    status: &str,
    planner_plan: Option<serde_json::Value>,
) -> Result<Option<PmResumeCheckpoint>, AppError> {
    let row_opt = sqlx::query::<sqlx::Sqlite>(
        "SELECT stage, attempt_no, status, detail_json
         FROM pm_research_stage_attempts
         WHERE run_id = ?
           AND stage IN ('preflight','understand','task_plan','planner','retrieve','verify','retry_repair','synthesize')
         ORDER BY id DESC
         LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row_opt else {
        return Ok(None);
    };

    let stage = row.try_get::<String, _>(0)?.trim().to_ascii_lowercase();
    if !is_pm_resumable_checkpoint_stage(&stage) {
        return Ok(None);
    }
    let attempt = row
        .try_get::<i32, _>(1)
        .ok()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .max(1);
    let detail = try_get_optional_json_field(&row, 3)?;

    Ok(Some(PmResumeCheckpoint {
        previous_task_id: run_id.to_string(),
        status: status.to_string(),
        stage: Some(stage),
        attempt,
        detail,
        planner_plan,
    }))
}

pub(super) async fn load_pm_task_resume_context_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<
    Option<(
        String,
        String,
        Option<PmTaskInputContext>,
        String,
        Option<PmResumeCheckpoint>,
    )>,
    AppError,
> {
    let row_opt = sqlx::query::<sqlx::Sqlite>(
        "SELECT session_id, message, input_context_json, status, checkpoint_json
         FROM pm_research_tasks
         WHERE task_id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    let Some(row) = row_opt else {
        return Ok(None);
    };

    let session_id = row.try_get::<String, _>(0)?;
    let message = row.try_get::<String, _>(1)?;
    let input_context = parse_pm_input_context_value(try_get_optional_json_field(&row, 2)?);
    let status = row.try_get::<String, _>(3)?;
    let checkpoint_value = try_get_optional_json_field(&row, 4)?;
    let planner_plan = load_pm_resume_planner_plan_from_db(db, task_id).await?;
    let resume_checkpoint =
        parse_pm_resume_checkpoint(task_id, &status, checkpoint_value, planner_plan.clone());
    let resume_checkpoint = if resume_checkpoint.is_some() {
        resume_checkpoint
    } else if matches!(
        status.to_ascii_lowercase().as_str(),
        "failed" | "cancelled" | "interrupted"
    ) {
        load_pm_resume_checkpoint_from_stage_attempts(db, task_id, &status, planner_plan).await?
    } else {
        None
    };

    Ok(Some((
        session_id,
        message,
        input_context,
        status,
        resume_checkpoint,
    )))
}

pub(super) async fn seed_pm_task_resume_checkpoint(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    checkpoint: &PmResumeCheckpoint,
) -> Result<(), AppError> {
    let stage = checkpoint
        .stage
        .clone()
        .unwrap_or_else(|| "retrieve".to_string());
    let checkpoint_json = serde_json::json!({
        "status": "running",
        "stage": stage,
        "attempt": checkpoint.attempt.max(1),
        "elapsedMs": 0,
        "stageElapsedMs": 0,
        "cancelRequested": false,
        "done": false,
        "detail": checkpoint.detail.clone(),
        "error": serde_json::Value::Null,
        "hasResponse": false,
        "resumeFromTaskId": checkpoint.previous_task_id.clone(),
        "plannerPlan": checkpoint.planner_plan.clone(),
    });
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE pm_research_tasks
         SET checkpoint_json = ?, status = 'queued', stage = NULL, attempt = NULL,
             detail_json = NULL, response_json = NULL, error_message = NULL,
             completed_at = NULL, cancel_requested = 0, event_seq = MAX(event_seq, 1), updated_at = CURRENT_TIMESTAMP
         WHERE task_id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(checkpoint_json.to_string())
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

pub(super) async fn load_pm_task_events_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    last_id_exclusive: u64,
    limit: i64,
) -> Result<Vec<(u64, PmResearchTaskEvent)>, AppError> {
    let limit = limit.clamp(1, 512);
    let id_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id
         FROM pm_research_task_events
         WHERE tenant_id = ? AND user_id = ? AND task_id = ? AND id > ?
         ORDER BY id ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .bind(crate::sqlite_i64(last_id_exclusive))
    .bind(limit)
    .fetch_all(db)
    .await?;

    let ids = id_rows
        .into_iter()
        .map(|row| row.try_get::<u64, _>(0))
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, status, stage, attempt, message, elapsed_ms, stage_elapsed_ms,
                detail_json, response_json, error_message
         FROM pm_research_task_events
         WHERE tenant_id = ",
    );
    builder
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id)
        .push(" AND task_id = ")
        .push_bind(task_id)
        .push(" AND id IN (");
    let mut separated = builder.separated(", ");
    for id in &ids {
        separated.push_bind(crate::sqlite_i64(*id));
    }
    separated.push_unseparated(")");
    let rows = builder.build().fetch_all(db).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.try_get::<u64, _>(0)?;
        let status = row.try_get::<String, _>(1)?;
        let stage = row.try_get::<Option<String>, _>(2)?;
        let attempt = row
            .try_get::<Option<i32>, _>(3)?
            .and_then(|v| usize::try_from(v).ok());
        let message = row.try_get::<Option<String>, _>(4)?;
        let elapsed_ms = row.try_get::<u64, _>(5)?;
        let stage_elapsed_ms = row.try_get::<Option<u64>, _>(6)?;
        let detail = try_get_optional_json_field(&row, 7)?;
        let response = try_get_optional_json_field(&row, 8)?;
        let error = row.try_get::<Option<String>, _>(9)?;
        out.push((
            id,
            PmResearchTaskEvent {
                task_id: task_id.to_string(),
                session_id: session_id.to_string(),
                status,
                stage,
                attempt,
                message,
                elapsed_ms,
                stage_elapsed_ms,
                detail,
                response,
                error,
            },
        ));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok(out)
}

async fn load_pm_task_recent_events_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    limit: i64,
) -> Result<Vec<(u64, PmResearchTaskEvent)>, AppError> {
    let limit = limit.clamp(1, 128);
    let id_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id
         FROM pm_research_task_events
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
         ORDER BY id DESC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let ids = id_rows
        .into_iter()
        .map(|row| row.try_get::<u64, _>(0))
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, status, stage, attempt, message, elapsed_ms, stage_elapsed_ms,
                detail_json, response_json, error_message
         FROM pm_research_task_events
         WHERE tenant_id = ",
    );
    builder
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id)
        .push(" AND task_id = ")
        .push_bind(task_id)
        .push(" AND id IN (");
    let mut separated = builder.separated(", ");
    for id in &ids {
        separated.push_bind(crate::sqlite_i64(*id));
    }
    separated.push_unseparated(")");
    let rows = builder.build().fetch_all(db).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.try_get::<u64, _>(0)?;
        let status = row.try_get::<String, _>(1)?;
        let stage = row.try_get::<Option<String>, _>(2)?;
        let attempt = row
            .try_get::<Option<i32>, _>(3)?
            .and_then(|v| usize::try_from(v).ok());
        let message = row.try_get::<Option<String>, _>(4)?;
        let elapsed_ms = row.try_get::<u64, _>(5)?;
        let stage_elapsed_ms = row.try_get::<Option<u64>, _>(6)?;
        let detail = try_get_optional_json_field(&row, 7)?;
        let response = try_get_optional_json_field(&row, 8)?;
        let error = row.try_get::<Option<String>, _>(9)?;
        out.push((
            id,
            PmResearchTaskEvent {
                task_id: task_id.to_string(),
                session_id: session_id.to_string(),
                status,
                stage,
                attempt,
                message,
                elapsed_ms,
                stage_elapsed_ms,
                detail,
                response,
                error,
            },
        ));
    }
    out.sort_by_key(|(id, _)| *id);
    Ok(out)
}

pub(super) async fn load_pm_task_stream_events_from_db(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    last_id_exclusive: u64,
    limit: i64,
) -> Result<Vec<(u64, PmResearchTaskStreamEvent)>, AppError> {
    let limit = limit.clamp(1, 512);
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, session_id, stage, seq, delta
         FROM pm_research_task_stream_events
         WHERE tenant_id = ? AND user_id = ? AND task_id = ? AND id > ?
         ORDER BY id ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .bind(crate::sqlite_i64(last_id_exclusive))
    .bind(limit)
    .fetch_all(db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.try_get::<u64, _>(0)?;
        let session_id = row.try_get::<String, _>(1)?;
        let stage = row.try_get::<String, _>(2)?;
        let sequence = row.try_get::<u64, _>(3)?;
        let delta = row.try_get::<String, _>(4)?;
        out.push((
            id,
            PmResearchTaskStreamEvent {
                task_id: task_id.to_string(),
                session_id,
                stage,
                sequence,
                delta,
            },
        ));
    }
    Ok(out)
}

pub(super) async fn load_pm_session_history_replay_from_db(
    db: &sqlx::SqlitePool,
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<PmSessionHistoryReplayDto>, AppError> {
    let latest_task = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, status, session_id
         FROM pm_research_tasks
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?
         ORDER BY updated_at DESC, task_id DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(db)
    .await
    .map_err(|error| {
        tracing::warn!(
            session_id = %session_id,
            tenant_id = %tenant_id,
            user_id = %user_id,
            error = %error,
            error_debug = ?error,
            "load_pm_session_history_replay_from_db: query latest task failed"
        );
        AppError::Database(error)
    })?;

    let Some(row) = latest_task else {
        return Ok(None);
    };
    let task_id = row.try_get::<String, _>(0).map_err(|error| {
        tracing::warn!(
            session_id = %session_id,
            tenant_id = %tenant_id,
            user_id = %user_id,
            error = %error,
            error_debug = ?error,
            "load_pm_session_history_replay_from_db: decode task_id failed"
        );
        AppError::Database(error)
    })?;
    let status = row.try_get::<String, _>(1).map_err(|error| {
        tracing::warn!(
            session_id = %session_id,
            tenant_id = %tenant_id,
            user_id = %user_id,
            task_id = %task_id,
            error = %error,
            error_debug = ?error,
            "load_pm_session_history_replay_from_db: decode status failed"
        );
        AppError::Database(error)
    })?;
    let replay_session_id = row
        .try_get::<String, _>(2)
        .unwrap_or_else(|_| session_id.to_string());
    let mut events = load_pm_task_recent_events_from_db(
        db,
        &task_id,
        tenant_id,
        user_id,
        &replay_session_id,
        48,
    )
    .await
    .map_err(|error| {
        tracing::warn!(
            session_id = %session_id,
            tenant_id = %tenant_id,
            user_id = %user_id,
            task_id = %task_id,
            error = %error,
            error_debug = ?error,
            "load_pm_session_history_replay_from_db: load events failed"
        );
        error
    })?
    .into_iter()
    .map(|(_, event)| event)
    .collect::<Vec<_>>();
    if events.is_empty() {
        if let Some((snapshot_event, _cancel_requested)) =
            load_pm_task_snapshot_from_db(db, &task_id, tenant_id, user_id)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        session_id = %session_id,
                        tenant_id = %tenant_id,
                        user_id = %user_id,
                        task_id = %task_id,
                        error = %error,
                        error_debug = ?error,
                        "load_pm_session_history_replay_from_db: load task snapshot failed"
                    );
                    error
                })?
        {
            events.push(snapshot_event);
        }
    }

    let task_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, status, response_json FROM pm_research_tasks
         WHERE tenant_id = ? AND user_id = ? AND session_id = ?
         ORDER BY created_at ASC, task_id ASC",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_all(db)
    .await?;
    let mut delivery_artifacts = Vec::new();
    for task_row in task_rows {
        let task_id = task_row.try_get::<String, _>(0)?;
        let task_status = task_row.try_get::<String, _>(1)?;
        let response = try_get_optional_json_field(&task_row, 2)?;
        let mut artifact =
            crate::semantic_kernel_store::load_pm_final_delivery(db, &task_id, tenant_id, user_id)
                .await
                .map_err(|error| AppError::Database(sqlx::Error::Protocol(error.to_string())))?;
        // Backfill pre-0018 completed tasks lazily. This keeps upgrades
        // lossless without rewriting every tenant during startup.
        if artifact.is_none() && pm_task_is_terminal_status(&task_status) {
            artifact = Some(
                crate::semantic_kernel_store::persist_pm_final_delivery(
                    db,
                    tenant_id,
                    user_id,
                    session_id,
                    &task_id,
                    &task_status,
                    response.as_ref(),
                )
                .await
                .map_err(|error| AppError::Database(sqlx::Error::Protocol(error.to_string())))?,
            );
        }
        if let Some(artifact) = artifact {
            delivery_artifacts.push(artifact);
        }
    }

    Ok(Some(PmSessionHistoryReplayDto {
        task_id,
        status,
        events,
        delivery_artifacts,
    }))
}

#[derive(Debug, Clone)]
pub(super) struct PmSessionTaskBinding {
    task_id: String,
    status: String,
    message: String,
    response_text: Option<String>,
    error: Option<String>,
}

pub(super) async fn load_pm_session_task_bindings_from_db(
    db: &sqlx::SqlitePool,
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<PmSessionTaskBinding>, AppError> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, status, message, input_context_json
         FROM pm_research_tasks
         WHERE session_id = ? AND tenant_id = ? AND user_id = ?
         ORDER BY created_at ASC, updated_at ASC, task_id ASC",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let raw_message = row.try_get::<String, _>(2)?;
        let visible_message = parse_pm_input_context_value(try_get_optional_json_field(&row, 3)?)
            .and_then(|context| context.visible_message)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or(raw_message);
        out.push(PmSessionTaskBinding {
            task_id: row.try_get::<String, _>(0)?,
            status: row.try_get::<String, _>(1)?,
            message: pm_task_visible_user_message(&visible_message),
            response_text: None,
            error: None,
        });
    }

    if out.is_empty() {
        return Ok(out);
    }

    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT task_id,
                CAST(JSON_EXTRACT(response_json, '$.text') AS TEXT) AS response_text,
                error_message
         FROM pm_research_tasks
         WHERE tenant_id = ",
    );
    builder
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id)
        .push(" AND session_id = ")
        .push_bind(session_id)
        .push(" AND task_id IN (");
    let mut separated = builder.separated(", ");
    for binding in &out {
        separated.push_bind(&binding.task_id);
    }
    separated.push_unseparated(")");
    let payload_rows = builder.build().fetch_all(db).await?;

    let mut payloads = std::collections::HashMap::with_capacity(payload_rows.len());
    for row in payload_rows {
        let task_id = row.try_get::<String, _>(0)?;
        payloads.insert(
            task_id,
            (
                row.try_get::<Option<String>, _>(1)?
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                row.try_get::<Option<String>, _>(2)?,
            ),
        );
    }
    for binding in &mut out {
        if let Some((response_text, error)) = payloads.remove(&binding.task_id) {
            binding.response_text = response_text;
            binding.error = error;
        }
    }

    Ok(out)
}

pub(super) async fn load_latest_completed_pm_task_answer_from_db(
    db: &sqlx::SqlitePool,
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<String>, AppError> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT response_json
         FROM pm_research_tasks
         WHERE session_id = ?
           AND tenant_id = ?
           AND user_id = ?
           AND status = 'completed'
           AND response_json IS NOT NULL
         ORDER BY completed_at DESC, updated_at DESC, created_at DESC
         LIMIT 5",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(db)
    .await?;

    for row in rows {
        let Some(response) = try_get_optional_json_field(&row, 0)? else {
            continue;
        };
        let answer = response
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(std::string::ToString::to_string);
        if answer.is_some() {
            return Ok(answer);
        }
    }

    Ok(None)
}

fn normalize_pm_task_match_text(input: &str) -> String {
    input
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn super_assistant_history_prefix_is_internal(prefix: &str) -> bool {
    let trimmed = prefix.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with("以下是与当前问题相关的历史记忆与上下文")
        || lower.starts_with("relevant recalled conversation memory and archive context")
        || lower.starts_with("[hook] pretooluse")
        || lower.starts_with("input: {")
        || lower.contains("native_model_search")
        || lower.contains("native search")
        || lower.contains("\"toolname\"")
        || lower.contains("\"tool_name\"")
        || lower.contains("\"sourcename\"")
        || lower.contains("\"source_name\"")
        || lower.contains("\"tool_use_id\"")
        || lower.contains("memory_search")
        || lower.contains("web_search")
        || (trimmed.starts_with('{')
            && (lower.contains("\"tool")
                || lower.contains("\"input\"")
                || lower.contains("\"query\"")
                || lower.contains("\"source")))
        || (trimmed.starts_with('[')
            && (lower.contains("\"tool")
                || lower.contains("\"input\"")
                || lower.contains("\"query\"")
                || lower.contains("\"source")))
}

fn strip_super_assistant_recall_preamble(message: &str) -> String {
    let mut current = message.trim_start().to_string();
    for _ in 0..8 {
        let trimmed = current.trim_start();
        let Some((prefix, rest)) = trimmed.split_once("\n\n") else {
            break;
        };
        if !super_assistant_history_prefix_is_internal(prefix) {
            break;
        }
        let rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        current = rest.to_string();
    }
    current
}

fn pm_task_visible_user_message(message: &str) -> String {
    strip_super_assistant_recall_preamble(message)
        .trim()
        .to_string()
}

pub(super) fn assign_pm_task_bindings_to_history_messages(
    messages: &mut [MessageDto],
    bindings: &[PmSessionTaskBinding],
) {
    if messages.is_empty() || bindings.is_empty() {
        return;
    }

    let bindable = bindings
        .iter()
        .filter(|row| {
            !messages
                .iter()
                .any(|msg| msg.pm_task_id.as_deref() == Some(row.task_id.as_str()))
        })
        .collect::<Vec<_>>();
    if bindable.is_empty() {
        return;
    }

    let mut binding_cursor: usize = 0;
    let mut pending_task: Option<(String, String)> = None;
    for msg in messages.iter_mut() {
        if msg.role == "user" {
            let user_norm = normalize_pm_task_match_text(&msg.content);
            let mut matched_idx: Option<usize> = None;
            if !user_norm.is_empty() {
                for (idx, row) in bindable.iter().enumerate().skip(binding_cursor) {
                    let row_norm =
                        normalize_pm_task_match_text(&pm_task_visible_user_message(&row.message));
                    if !row_norm.is_empty() && row_norm == user_norm {
                        matched_idx = Some(idx);
                        break;
                    }
                }
            }

            if let Some(idx) = matched_idx {
                let row = bindable[idx];
                pending_task = Some((row.task_id.clone(), row.status.clone()));
                binding_cursor = idx.saturating_add(1);
            } else {
                pending_task = None;
            }
            continue;
        }

        if msg.role == "assistant" {
            if let Some((task_id, status)) = pending_task.take() {
                msg.pm_task_id = Some(task_id);
                msg.pm_task_status = Some(status);
            }
        }
    }
}

fn pm_task_binding_assistant_text(binding: &PmSessionTaskBinding) -> Option<String> {
    if let Some(response_text) = binding.response_text.as_deref() {
        let response_text = response_text.trim();
        if !response_text.is_empty() {
            return Some(response_text.to_string());
        }
    }

    let status = binding.status.to_ascii_lowercase();
    if status == "cancelled" {
        return Some("研究任务已取消。".to_string());
    }
    if status == "failed" {
        let reason = binding
            .error
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("未知错误");
        let turn = build_pm_local_strategy_synthesis_turn(
            "",
            "pm-local-history-recovery",
            &pm_task_visible_user_message(&binding.message),
            reason,
            1,
            &[],
        );
        return Some(turn.text);
    }

    None
}

fn pm_history_is_internal_research_message(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.contains(PM_POLICY_BEGIN)
        || trimmed.contains(PM_POLICY_END)
        || trimmed.contains(PM_ORCH_INTERNAL_BEGIN)
        || trimmed.contains(PM_ORCH_INTERNAL_END)
        || trimmed.contains("AOS_PM_RESEARCH_POLICY_BEGIN")
        || trimmed.contains("AOS_PM_RESEARCH_POLICY_END")
        || trimmed.contains("PM_LIGHTWEIGHT_CHAT_ENGINE_ROUTE")
        || trimmed.contains("PM_INTERNAL_TRANSIENT_SESSION_SOURCE")
        || trimmed.starts_with("深度分析已启动")
        || trimmed
            .to_ascii_lowercase()
            .starts_with("deep analysis started")
}

fn pm_task_binding_has_terminal_assistant(binding: &PmSessionTaskBinding) -> bool {
    pm_task_binding_assistant_text(binding).is_some()
}

fn pm_message_plain_norm(message: &MessageDto) -> String {
    normalize_pm_task_match_text(&message.content)
}

fn pm_history_user_matches_binding(msg: &MessageDto, binding_user_norm: &str) -> bool {
    if msg.role != "user" || binding_user_norm.is_empty() {
        return false;
    }
    let message_norm = pm_message_plain_norm(msg);
    if message_norm.is_empty() {
        return false;
    }
    message_norm == binding_user_norm || message_norm.contains(binding_user_norm)
}

fn pm_history_has_same_visible_message(messages: &[MessageDto], candidate: &MessageDto) -> bool {
    if pm_history_is_internal_research_message(&candidate.content) {
        return true;
    }
    let candidate_norm = pm_message_plain_norm(candidate);
    !candidate_norm.is_empty()
        && messages
            .iter()
            .any(|msg| msg.role == candidate.role && pm_message_plain_norm(msg) == candidate_norm)
}

fn pm_history_any_shadow_assistant_after_user(
    original: &[MessageDto],
    consumed: &[bool],
    user_idx: usize,
) -> Option<usize> {
    original
        .iter()
        .enumerate()
        .skip(user_idx.saturating_add(1))
        .take_while(|(_, msg)| msg.role != "user")
        .find_map(|(idx, msg)| {
            if !consumed[idx] && msg.role == "assistant" && msg.pm_task_id.is_none() {
                Some(idx)
            } else {
                None
            }
        })
}

pub(super) fn reconcile_pm_task_history_turns(
    messages: &mut Vec<MessageDto>,
    bindings: &[PmSessionTaskBinding],
) {
    if bindings.is_empty() {
        return;
    }

    let original = messages.clone();
    let mut consumed = vec![false; original.len()];
    let mut rebuilt = Vec::new();

    for binding in bindings {
        let visible_user_message = pm_task_visible_user_message(&binding.message);
        let user_text = visible_user_message.trim();
        if user_text.is_empty() {
            continue;
        }
        let user_norm = normalize_pm_task_match_text(user_text);
        if user_norm.is_empty() {
            continue;
        }

        let existing_user_idx = original.iter().enumerate().find_map(|(idx, msg)| {
            if !consumed[idx] && pm_history_user_matches_binding(msg, &user_norm) {
                Some(idx)
            } else {
                None
            }
        });
        let user_message = if let Some(idx) = existing_user_idx {
            consumed[idx] = true;
            original[idx].clone()
        } else {
            MessageDto {
                role: "user".to_string(),
                content: user_text.to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            }
        };
        rebuilt.push(user_message);

        let assistant_text = pm_task_binding_assistant_text(binding);
        let assistant_norm = assistant_text
            .as_deref()
            .map(normalize_pm_task_match_text)
            .unwrap_or_default();
        let existing_assistant_idx = if pm_task_binding_has_terminal_assistant(binding) {
            original.iter().enumerate().find_map(|(idx, msg)| {
                if consumed[idx]
                    || msg.role != "assistant"
                    || pm_history_is_internal_research_message(&msg.content)
                {
                    return None;
                }
                let text_match =
                    !assistant_norm.is_empty() && pm_message_plain_norm(msg) == assistant_norm;
                if text_match {
                    Some(idx)
                } else {
                    None
                }
            })
        } else {
            None
        };
        let assistant_message = if let Some(idx) = existing_assistant_idx {
            consumed[idx] = true;
            let mut msg = original[idx].clone();
            if msg.pm_task_id.is_none() {
                msg.pm_task_id = Some(binding.task_id.clone());
            }
            if msg.pm_task_status.is_none() {
                msg.pm_task_status = Some(binding.status.clone());
            }
            Some(msg)
        } else {
            assistant_text.map(|content| MessageDto {
                role: "assistant".to_string(),
                content,
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: Some(binding.task_id.clone()),
                pm_task_status: Some(binding.status.clone()),
            })
        };
        if let Some(assistant_message) = assistant_message {
            if existing_assistant_idx.is_none() {
                if let Some(user_idx) = existing_user_idx {
                    if let Some(shadow_idx) =
                        pm_history_any_shadow_assistant_after_user(&original, &consumed, user_idx)
                    {
                        consumed[shadow_idx] = true;
                    }
                }
                for (idx, msg) in original.iter().enumerate() {
                    if !consumed[idx]
                        && msg.role == "assistant"
                        && msg.pm_task_id.as_deref() == Some(binding.task_id.as_str())
                    {
                        consumed[idx] = true;
                    }
                }
            }
            rebuilt.push(assistant_message);
        } else if let Some(user_idx) = existing_user_idx {
            if let Some(shadow_idx) =
                pm_history_any_shadow_assistant_after_user(&original, &consumed, user_idx)
            {
                consumed[shadow_idx] = true;
            }
        }
    }

    for (idx, msg) in original.into_iter().enumerate() {
        let stale_bound_assistant = msg.role == "assistant"
            && msg.pm_task_id.as_ref().is_some_and(|task_id| {
                bindings
                    .iter()
                    .any(|binding| binding.task_id.as_str() == task_id.as_str())
            });
        if consumed[idx] || pm_history_has_same_visible_message(&rebuilt, &msg) {
            continue;
        }
        if stale_bound_assistant {
            continue;
        }
        rebuilt.push(msg);
    }

    *messages = rebuilt;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_pm_lease_heartbeat_honors_run_deadlines() {
        let db = crate::test_sqlite_pool().await;
        let task_id = format!("pm-task-{}", uuid::Uuid::new_v4());
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_tasks
               (task_id, tenant_id, user_id, session_id, message, status, lease_owner)
             VALUES (?, 'tenant', 'user', 'session', 'test', 'running', 'worker')",
        )
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert PM task fixture");

        assert!(touch_pm_task_lease(&db, &task_id, "worker", 60)
            .await
            .expect("heartbeat task without run"));
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO pm_research_runs
               (run_id, tenant_id, user_id, session_id, status, deadline_at)
             VALUES (?, 'tenant', 'user', 'session', 'running', datetime(CURRENT_TIMESTAMP, '+1 hour'))",
        )
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("insert active PM run");
        assert!(touch_pm_task_lease(&db, &task_id, "worker", 60)
            .await
            .expect("heartbeat active run"));

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE pm_research_runs
             SET deadline_at = datetime(CURRENT_TIMESTAMP, '-1 second') WHERE run_id = ?",
        )
        .bind(&task_id)
        .execute(&db)
        .await
        .expect("expire PM run");
        assert!(!touch_pm_task_lease(&db, &task_id, "worker", 60)
            .await
            .expect("reject heartbeat after deadline"));
        db.close().await;
    }

    #[tokio::test]
    async fn pm_history_backfills_delivery_for_every_completed_task_in_session() {
        let db = crate::test_sqlite_pool().await;
        for (task_id, message, answer) in [
            ("pm-history-1", "first question", "first answer"),
            ("pm-history-2", "second question", "second answer"),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO pm_research_tasks
                    (task_id, tenant_id, user_id, session_id, message, status,
                     stage, event_seq, response_json, completed_at)
                 VALUES (?, 'tenant', 'user', 'session', ?, 'completed', 'done', 1, ?, CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(message)
            .bind(
                serde_json::json!({
                    "text": answer,
                    "pm_quality": {"passed": true}
                })
                .to_string(),
            )
            .execute(&db)
            .await
            .unwrap();
        }

        let replay = load_pm_session_history_replay_from_db(&db, "session", "tenant", "user")
            .await
            .unwrap()
            .expect("PM history replay");
        assert_eq!(replay.delivery_artifacts.len(), 2);
        assert_eq!(replay.delivery_artifacts[0].task_id, "pm-history-1");
        assert_eq!(replay.delivery_artifacts[1].task_id, "pm-history-2");
        assert_eq!(
            replay.delivery_artifacts[0]
                .response
                .as_ref()
                .and_then(|value| value.get("text"))
                .and_then(serde_json::Value::as_str),
            Some("first answer")
        );
        let persisted_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pm_final_delivery_artifacts
             WHERE tenant_id = 'tenant' AND user_id = 'user' AND session_id = 'session'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(persisted_count, 2);
    }

    #[test]
    fn parse_pm_resume_checkpoint_rejects_fresh_queue_checkpoint() {
        let checkpoint = serde_json::json!({
            "status": "queued",
            "stage": "queued",
            "attempt": 1,
            "done": false,
            "cancelRequested": false,
            "hasResponse": false
        });
        let parsed = parse_pm_resume_checkpoint("task-1", "running", Some(checkpoint), None);
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_pm_resume_checkpoint_accepts_mid_pipeline_checkpoint() {
        let checkpoint = serde_json::json!({
            "status": "running",
            "stage": "retrieve",
            "attempt": 2,
            "done": false,
            "cancelRequested": false,
            "hasResponse": false,
            "detail": {"route":"web.search.general"}
        });
        let parsed = parse_pm_resume_checkpoint("task-2", "interrupted", Some(checkpoint), None);
        let parsed = parsed.expect("checkpoint should be resumable");
        assert_eq!(parsed.stage.as_deref(), Some("retrieve"));
        assert_eq!(parsed.attempt, 2);
        assert_eq!(parsed.previous_task_id, "task-2");
    }

    fn completed_binding(task_id: &str, message: &str, answer: &str) -> PmSessionTaskBinding {
        PmSessionTaskBinding {
            task_id: task_id.to_string(),
            status: "completed".to_string(),
            message: message.to_string(),
            response_text: Some(answer.to_string()),
            error: None,
        }
    }

    fn running_binding(task_id: &str, message: &str) -> PmSessionTaskBinding {
        PmSessionTaskBinding {
            task_id: task_id.to_string(),
            status: "running".to_string(),
            message: message.to_string(),
            response_text: None,
            error: None,
        }
    }

    fn failed_binding(task_id: &str, message: &str, error: &str) -> PmSessionTaskBinding {
        PmSessionTaskBinding {
            task_id: task_id.to_string(),
            status: "failed".to_string(),
            message: message.to_string(),
            response_text: None,
            error: Some(error.to_string()),
        }
    }

    #[test]
    fn failed_pm_task_binding_replays_recovery_answer_not_failure_template() {
        let binding = failed_binding(
            "pm-research-task-failed",
            "请基于我提供的用户分层数据给出提升 ROI 的策略。",
            "quality gate blocked output",
        );
        let text = pm_task_binding_assistant_text(&binding).expect("recovery answer");

        assert!(text.contains("## 先给可执行结论"));
        assert!(text.contains("ROI"));
        assert!(!text.contains("研究任务失败"));
        assert!(!text.contains("quality gate"));
        assert!(!text.contains("blocked output"));
    }

    #[test]
    fn reconcile_pm_task_history_turns_builds_full_turn_when_user_missing() {
        let mut messages = vec![MessageDto {
            role: "assistant".to_string(),
            content: "核心结论：可以做，但要控制成本。".to_string(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: None,
            pm_task_status: None,
        }];
        let bindings = vec![completed_binding(
            "pm-research-task-1",
            "今晚世界杯期间投广告赚金币游戏是否值得做？",
            "核心结论：可以做，但要控制成本。",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(
            messages[0].content,
            "今晚世界杯期间投广告赚金币游戏是否值得做？"
        );
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(
            messages[1].pm_task_id.as_deref(),
            Some("pm-research-task-1")
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_does_not_duplicate_existing_user() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "研究世界杯广告赚金币游戏".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "结论如下".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![completed_binding(
            "pm-research-task-2",
            "研究世界杯广告赚金币游戏",
            "结论如下",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(
            messages[1].pm_task_id.as_deref(),
            Some("pm-research-task-2")
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_keeps_multiple_missing_turns_ordered() {
        let mut messages = vec![
            MessageDto {
                role: "assistant".to_string(),
                content: "第一个结论".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "第二个结论".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![
            completed_binding("pm-research-task-1", "第一个问题", "第一个结论"),
            completed_binding("pm-research-task-2", "第二个问题", "第二个结论"),
        ];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let roles = messages
            .iter()
            .map(|msg| msg.role.as_str())
            .collect::<Vec<_>>();
        let contents = messages
            .iter()
            .map(|msg| msg.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
        assert_eq!(
            contents,
            vec!["第一个问题", "第一个结论", "第二个问题", "第二个结论"]
        );
        assert_eq!(
            messages[1].pm_task_id.as_deref(),
            Some("pm-research-task-1")
        );
        assert_eq!(
            messages[3].pm_task_id.as_deref(),
            Some("pm-research-task-2")
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_rebuilds_all_task_turns_from_task_rows() {
        let mut messages = vec![MessageDto {
            role: "assistant".to_string(),
            content: "最新结论".to_string(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: None,
            pm_task_status: None,
        }];
        let bindings = vec![
            completed_binding("pm-research-task-1", "第一个历史问题", "第一个回答"),
            completed_binding("pm-research-task-2", "第二个历史问题", "第二个回答"),
            completed_binding("pm-research-task-3", "最新问题", "最新结论"),
        ];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let roles = messages
            .iter()
            .map(|msg| msg.role.as_str())
            .collect::<Vec<_>>();
        let contents = messages
            .iter()
            .map(|msg| msg.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![
                "user",
                "assistant",
                "user",
                "assistant",
                "user",
                "assistant"
            ]
        );
        assert_eq!(
            contents,
            vec![
                "第一个历史问题",
                "第一个回答",
                "第二个历史问题",
                "第二个回答",
                "最新问题",
                "最新结论"
            ]
        );
        assert_eq!(
            messages[5].pm_task_id.as_deref(),
            Some("pm-research-task-3")
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_builds_from_response_when_runtime_empty() {
        let mut messages = Vec::new();
        let bindings = vec![
            completed_binding("pm-research-task-1", "旧问题", "旧回答"),
            completed_binding("pm-research-task-2", "最新问题", "最新回答"),
        ];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let contents = messages
            .iter()
            .map(|msg| msg.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(contents, vec!["旧问题", "旧回答", "最新问题", "最新回答"]);
    }

    #[test]
    fn reconcile_pm_task_history_turns_drops_shadow_assistants_after_queue_rebuild() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "1".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "实时回答 1".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "user".to_string(),
                content: "2".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "实时回答 2".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "user".to_string(),
                content: "3".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "实时回答 3".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![
            completed_binding("pm-research-task-1", "1", "最终回答 1"),
            completed_binding("pm-research-task-2", "2", "最终回答 2"),
            completed_binding("pm-research-task-3", "3", "最终回答 3"),
        ];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let roles = messages
            .iter()
            .map(|msg| msg.role.as_str())
            .collect::<Vec<_>>();
        let contents = messages
            .iter()
            .map(|msg| msg.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![
                "user",
                "assistant",
                "user",
                "assistant",
                "user",
                "assistant"
            ]
        );
        assert_eq!(
            contents,
            vec!["1", "最终回答 1", "2", "最终回答 2", "3", "最终回答 3"]
        );
        assert_eq!(
            messages
                .iter()
                .filter(|msg| msg.role == "assistant")
                .filter(|msg| msg.content.starts_with("实时回答"))
                .count(),
            0
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_replaces_stale_bound_shadow_answer() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "北京和上海天气咋样？天津呢".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "## 可执行保守结论\n指标：re=1...".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: Some("pm-research-task-weather".to_string()),
                pm_task_status: Some("completed".to_string()),
            },
        ];
        let bindings = vec![completed_binding(
            "pm-research-task-weather",
            "北京和上海天气咋样？天津呢",
            "北京、上海、天津天气结论如下。",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);
        assign_pm_task_bindings_to_history_messages(&mut messages, &bindings);

        let assistant_messages = messages
            .iter()
            .filter(|msg| msg.role == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(
            assistant_messages[0].content,
            "北京、上海、天津天气结论如下。"
        );
        assert_eq!(
            assistant_messages[0].pm_task_id.as_deref(),
            Some("pm-research-task-weather")
        );
        assert!(!messages
            .iter()
            .any(|msg| msg.content.contains("可执行保守结论")));
    }

    #[test]
    fn reconcile_pm_task_history_turns_does_not_pair_binding_with_wrong_user_by_order() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "把“我爱你、你好”翻译成英文、西班牙语、印尼语，直接给表格。".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "## 核心结论\n\n天气回答".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: Some("pm-research-task-weather".to_string()),
                pm_task_status: Some("completed".to_string()),
            },
        ];
        let bindings = vec![
            completed_binding(
                "pm-research-task-weather",
                "北京和上海天气咋样？天津呢",
                "## 核心结论\n\n天气回答",
            ),
            completed_binding(
                "pm-research-task-translation",
                "把“我爱你、你好”翻译成英文、西班牙语、印尼语，直接给表格。",
                "## 核心结论\n\n翻译回答",
            ),
        ];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        assert!(messages.windows(2).any(
            |pair| pair[0].content.contains("天气咋样") && pair[1].content.contains("天气回答")
        ));
        assert!(
            messages
                .windows(2)
                .any(|pair| pair[0].content.contains("我爱你")
                    && pair[1].content.contains("翻译回答"))
        );
        assert!(
            !messages
                .windows(2)
                .any(|pair| pair[0].content.contains("我爱你")
                    && pair[1].content.contains("天气回答"))
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_consumes_shadow_when_user_text_is_wrapped() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "PM_INTERNAL\n原始问题：研究印尼激励产品 ROI".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "目录\n1. 核心结论\n2. 优先策略".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![completed_binding(
            "pm-research-task-wrapped",
            "研究印尼激励产品 ROI",
            "最终可用报告",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let assistant_messages = messages
            .iter()
            .filter(|msg| msg.role == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "最终可用报告");
        assert_eq!(
            assistant_messages[0].pm_task_id.as_deref(),
            Some("pm-research-task-wrapped")
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_strips_super_assistant_recall_preamble() {
        let visible_question = "先针对不同用户下发不同 posid，给我几套方案";
        let task_message = format!(
            "以下是与当前问题相关的历史记忆与上下文（压缩后经检索注入），回答时请参考：\n- [原文] 之前讨论过 posid 分层\n\n{visible_question}"
        );
        let mut messages = vec![MessageDto {
            role: "user".to_string(),
            content: visible_question.to_string(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: None,
            pm_task_status: None,
        }];
        let bindings = vec![completed_binding(
            "pm-research-task-recall",
            &task_message,
            "最终方案：按用户价值、广告收益和疲劳度分层。",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);
        assign_pm_task_bindings_to_history_messages(&mut messages, &bindings);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, visible_question);
        assert!(!messages[0].content.contains("以下是与当前问题相关"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(
            messages[1].pm_task_id.as_deref(),
            Some("pm-research-task-recall")
        );
    }

    #[test]
    fn pm_task_visible_user_message_strips_super_assistant_tool_json_prefix() {
        let visible_question = "海淀今天会下雨吗？顺便查一下 IBM 最新新闻";
        let task_message = format!(
            "{{\"toolName\":\"native_model_search\",\"input\":{{\"query\":\"海淀 天气 IBM\"}},\"sourceName\":\"native\"}}\n\n{visible_question}"
        );

        assert_eq!(
            pm_task_visible_user_message(&task_message),
            visible_question
        );
    }

    #[test]
    fn reconcile_pm_task_history_turns_drops_deep_analysis_launch_placeholder() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "继续研究 ROI 下降原因".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "深度分析已启动（任务 pm-research-task-1，状态 queued）。进度和证据会继续写入本会话。".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![running_binding(
            "pm-research-task-1",
            "继续研究 ROI 下降原因",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert!(!messages
            .iter()
            .any(|msg| msg.content.contains("深度分析已启动")));
    }

    #[test]
    fn reconcile_pm_task_history_turns_does_not_bind_running_shadow_answer() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "研究印尼激励产品 ROI".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: "中间检索草稿，不应作为最终回答展示。".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![running_binding(
            "pm-research-task-running",
            "研究印尼激励产品 ROI",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert!(messages.iter().all(|msg| msg.role != "assistant"));
    }

    #[test]
    fn reconcile_pm_task_history_turns_filters_internal_policy_messages() {
        let mut messages = vec![
            MessageDto {
                role: "user".to_string(),
                content: "研究印尼激励产品 ROI".to_string(),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
            MessageDto {
                role: "assistant".to_string(),
                content: format!("{PM_POLICY_BEGIN}\ninternal\n{PM_POLICY_END}"),
                tool_calls: None,
                tool_result: None,
                usage: None,
                thinking: None,
                pm_task_id: None,
                pm_task_status: None,
            },
        ];
        let bindings = vec![completed_binding(
            "pm-research-task-clean",
            "研究印尼激励产品 ROI",
            "最终可见报告",
        )];

        reconcile_pm_task_history_turns(&mut messages, &bindings);

        let contents = messages
            .iter()
            .map(|msg| msg.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(contents, vec!["研究印尼激励产品 ROI", "最终可见报告"]);
    }
}
