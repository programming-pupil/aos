use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::BTreeMap;

fn protected_persistence_text(value: &str) -> String {
    runtime::protect_sensitive_text(value, runtime::configured_data_protection_mode()).value
}

fn protected_persistence_json(value: Option<&Value>) -> Option<String> {
    value
        .map(Value::to_string)
        .map(|value| protected_persistence_text(&value))
}

#[derive(Debug, Clone)]
pub struct PmRunConfigSnapshot {
    pub budget_profile: String,
    pub pipeline_timeout_secs: u64,
    pub deadline_timeout_secs: u64,
    pub max_attempts: usize,
    pub source_slot_search_secs: u64,
    pub source_slot_browser_secs: u64,
    pub source_slot_api_fetch_secs: u64,
    pub retrieve_max_tool_calls: usize,
    pub max_calls_per_source: usize,
}

#[derive(Debug, Clone)]
pub struct PmBudgetProfileConfigRow {
    pub profile_key: String,
    pub pipeline_timeout_secs: u64,
    pub max_attempts: usize,
    pub retrieve_max_tool_calls: usize,
    pub max_calls_per_source: usize,
    pub source_slot_search_secs: u64,
    pub source_slot_browser_secs: u64,
    pub source_slot_api_fetch_secs: u64,
    pub preflight_model_timeout_secs: u64,
    pub preflight_probe_timeout_secs: u64,
    pub preflight_overall_timeout_secs: u64,
    pub retry_step_budget_secs: u64,
    pub retry_total_budget_secs: u64,
}

#[derive(Debug, Clone)]
pub struct PmRunFinishPayload {
    pub status: String,
    pub current_stage: Option<String>,
    pub attempt: Option<usize>,
    pub total_elapsed_ms: Option<u64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub final_quality_score: Option<f64>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PmSourceSlotUpsertPayload {
    pub run_id: String,
    pub stage_attempt_id: Option<u64>,
    pub slot_seq: usize,
    pub route_key: Option<String>,
    pub channel: Option<String>,
    pub variant: Option<String>,
    pub source_key: Option<String>,
    pub source_url: Option<String>,
    pub status: String,
    pub tool_call_count: usize,
    pub elapsed_ms: Option<u64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PmToolCallLedgerRow {
    pub run_id: String,
    pub stage_attempt_id: Option<u64>,
    pub source_slot_id: Option<u64>,
    pub call_seq: usize,
    pub tool_name: String,
    pub tool_use_id: Option<String>,
    pub input_preview: Option<String>,
    pub output_preview: Option<String>,
    pub input_raw: Option<String>,
    pub output_raw: Option<String>,
    pub is_error: bool,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub http_status: Option<i64>,
    pub latency_ms: Option<u64>,
    pub route_key: Option<String>,
    pub channel: Option<String>,
    pub provider: Option<String>,
    pub provider_trace: Option<String>,
    pub url: Option<String>,
    pub domain: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PmClaimVerdictRow {
    pub tenant_id: String,
    pub run_id: String,
    pub claim_key: String,
    pub claim_text: String,
    pub verdict: String,
    pub confidence: f64,
    pub evidence_excerpt: Option<String>,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PmConflictCaseRow {
    pub tenant_id: String,
    pub run_id: String,
    pub topic_key: String,
    pub topic: String,
    pub source_a: Option<String>,
    pub claim_a: Option<String>,
    pub source_b: Option<String>,
    pub claim_b: Option<String>,
    pub verdict: Option<String>,
    pub confidence: f64,
    pub reason: Option<String>,
    pub support_urls: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct PmSubtaskRunUpsertPayload {
    pub run_id: String,
    pub task_id: Option<String>,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub subtask_key: String,
    pub subtask_id: Option<String>,
    pub title: String,
    pub goal: Option<String>,
    pub deliverable: Option<String>,
    pub required_evidence_type: Option<String>,
    pub priority: String,
    pub status: String,
    pub probe_candidate_count: usize,
    pub probe_completed_count: usize,
    pub citation_count: usize,
    pub domain_count: usize,
    pub tool_call_count: usize,
    pub quality_score: Option<f64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct PmSubtaskAttemptUpsertPayload {
    pub subtask_run_id: u64,
    pub run_id: String,
    pub subtask_key: String,
    pub attempt_no: usize,
    pub attempt_key: String,
    pub variant: Option<String>,
    pub route_key: Option<String>,
    pub route_channel: Option<String>,
    pub status: String,
    pub elapsed_ms: Option<u64>,
    pub citation_count: usize,
    pub domain_count: usize,
    pub tool_call_count: usize,
    pub quality_score: Option<f64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PmSubtaskRuntimeRow {
    pub id: u64,
    pub run_id: String,
    pub task_id: Option<String>,
    pub subtask_key: String,
    pub subtask_id: Option<String>,
    pub title: String,
    pub goal: Option<String>,
    pub deliverable: Option<String>,
    pub required_evidence_type: Option<String>,
    pub priority: String,
    pub status: String,
    pub probe_candidate_count: usize,
    pub probe_completed_count: usize,
    pub citation_count: usize,
    pub domain_count: usize,
    pub tool_call_count: usize,
    pub quality_score: Option<f64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PmSubtaskAttemptRow {
    pub id: u64,
    pub subtask_run_id: u64,
    pub run_id: String,
    pub subtask_key: String,
    pub attempt_no: usize,
    pub attempt_key: String,
    pub variant: Option<String>,
    pub route_key: Option<String>,
    pub route_channel: Option<String>,
    pub status: String,
    pub elapsed_ms: Option<u64>,
    pub citation_count: usize,
    pub domain_count: usize,
    pub tool_call_count: usize,
    pub quality_score: Option<f64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub detail: Option<Value>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: Option<String>,
}

async fn reserve_pm_resource_budget(
    db: &sqlx::SqlitePool,
    run_id: &str,
    tenant_id: &str,
    user_id: &str,
    config: &PmRunConfigSnapshot,
) -> Result<(), sqlx::Error> {
    let owner_scope = format!("user:{user_id}:pm:{run_id}");
    let reservations = [
        (
            "wall_time_ms",
            config.pipeline_timeout_secs.saturating_mul(1_000),
        ),
        (
            "tool_calls",
            u64::try_from(config.retrieve_max_tool_calls).unwrap_or(u64::MAX),
        ),
        (
            "web_queries",
            u64::try_from(config.retrieve_max_tool_calls).unwrap_or(u64::MAX),
        ),
    ];
    let mut transaction = db.begin().await?;
    for (dimension, amount) in reservations {
        let amount = i64::try_from(amount).unwrap_or(i64::MAX);
        sqlx::query::<Sqlite>(
            "INSERT INTO resource_budget_accounts
                (tenant_id, owner_scope, dimension, available, reserved, committed)
             VALUES (?, ?, ?, 0, ?, 0)
             ON CONFLICT(tenant_id, owner_scope, dimension) DO UPDATE SET
                 available = 0,
                 reserved = excluded.reserved,
                 committed = 0",
        )
        .bind(tenant_id)
        .bind(&owner_scope)
        .bind(dimension)
        .bind(amount)
        .execute(&mut *transaction)
        .await?;
        sqlx::query::<Sqlite>(
            "INSERT INTO resource_budget_entries
                (id, tenant_id, owner_scope, reservation_id, dimension, amount, state, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'reserved', CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                 amount = excluded.amount,
                 state = 'reserved'",
        )
        .bind(format!("pm-budget:{run_id}:{dimension}:reserve"))
        .bind(tenant_id)
        .bind(&owner_scope)
        .bind(run_id)
        .bind(dimension)
        .bind(amount)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn settle_pm_resource_budget(
    db: &sqlx::SqlitePool,
    run_id: &str,
    payload: &PmRunFinishPayload,
) -> Result<(), sqlx::Error> {
    let Some(run) = sqlx::query::<Sqlite>(
        "SELECT tenant_id, user_id, total_elapsed_ms
         FROM pm_research_runs WHERE run_id = ? LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await?
    else {
        return Ok(());
    };
    let tenant_id = run.try_get::<String, _>(0)?;
    let user_id = run.try_get::<String, _>(1)?;
    let elapsed_ms = payload
        .total_elapsed_ms
        .or_else(|| {
            run.try_get::<Option<i64>, _>(2)
                .ok()
                .flatten()
                .and_then(|value| u64::try_from(value).ok())
        })
        .unwrap_or_default();
    let tool_calls = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COUNT(*) FROM pm_research_tool_call_ledger WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .unwrap_or_default()
    .max(0) as u64;
    let web_queries = sqlx::query_scalar::<Sqlite, i64>(
        "SELECT COUNT(*) FROM pm_research_tool_call_ledger
         WHERE run_id = ? AND (
             LOWER(tool_name) LIKE '%search%' OR LOWER(tool_name) LIKE '%fetch%'
             OR LOWER(tool_name) LIKE '%browser%' OR LOWER(tool_name) LIKE '%web%'
         )",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .unwrap_or_default()
    .max(0) as u64;
    let owner_scope = format!("user:{user_id}:pm:{run_id}");
    let actuals = [
        ("wall_time_ms", elapsed_ms),
        ("tool_calls", tool_calls),
        ("web_queries", web_queries),
    ];
    let mut transaction = db.begin().await?;
    for (dimension, actual) in actuals {
        let reserved = sqlx::query_scalar::<Sqlite, i64>(
            "SELECT reserved FROM resource_budget_accounts
             WHERE tenant_id = ? AND owner_scope = ? AND dimension = ?",
        )
        .bind(&tenant_id)
        .bind(&owner_scope)
        .bind(dimension)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or_default()
        .max(0);
        let actual_i64 = i64::try_from(actual).unwrap_or(i64::MAX);
        let committed = actual_i64.min(reserved);
        let available = reserved.saturating_sub(committed);
        let settlement_state = if actual_i64 > reserved {
            "overrun_blocked"
        } else if payload.status == "cancelled" && committed == 0 {
            "released"
        } else {
            "committed"
        };
        sqlx::query::<Sqlite>(
            "UPDATE resource_budget_accounts
             SET available = ?, reserved = 0, committed = ?
             WHERE tenant_id = ? AND owner_scope = ? AND dimension = ?",
        )
        .bind(available)
        .bind(committed)
        .bind(&tenant_id)
        .bind(&owner_scope)
        .bind(dimension)
        .execute(&mut *transaction)
        .await?;
        sqlx::query::<Sqlite>(
            "INSERT INTO resource_budget_entries
                (id, tenant_id, owner_scope, reservation_id, dimension, amount, state, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                 amount = excluded.amount,
                 state = excluded.state",
        )
        .bind(format!("pm-budget:{run_id}:{dimension}:settle"))
        .bind(&tenant_id)
        .bind(&owner_scope)
        .bind(run_id)
        .bind(dimension)
        .bind(actual_i64)
        .bind(settlement_state)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

pub async fn persist_pm_run_start(
    db: &sqlx::SqlitePool,
    run_id: &str,
    task_id: Option<&str>,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    source: &str,
    message: &str,
    config: &PmRunConfigSnapshot,
) {
    if let Err(error) = sqlx::query(
        "INSERT INTO pm_research_runs
            (run_id, task_id, tenant_id, user_id, session_id, source, status, current_stage, attempt,
             budget_profile, pipeline_timeout_secs, max_attempts,
             source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
             retrieve_max_tool_calls, max_calls_per_source, user_message, started_at, deadline_at)
         VALUES (?, ?, ?, ?, ?, ?, 'running', 'queued', 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)))
         ON CONFLICT DO UPDATE SET
            status = 'running',
            current_stage = 'queued',
            attempt = 1,
            budget_profile = excluded.budget_profile,
            pipeline_timeout_secs = excluded.pipeline_timeout_secs,
            max_attempts = excluded.max_attempts,
            source_slot_search_secs = excluded.source_slot_search_secs,
            source_slot_browser_secs = excluded.source_slot_browser_secs,
            source_slot_api_fetch_secs = excluded.source_slot_api_fetch_secs,
            retrieve_max_tool_calls = excluded.retrieve_max_tool_calls,
            max_calls_per_source = excluded.max_calls_per_source,
            user_message = excluded.user_message,
            started_at = CURRENT_TIMESTAMP,
            deadline_at = excluded.deadline_at,
            ended_at = NULL,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(run_id)
    .bind(task_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(source)
    .bind(&config.budget_profile)
    .bind(i64::try_from(config.pipeline_timeout_secs).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.max_attempts).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.source_slot_search_secs).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.source_slot_browser_secs).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.source_slot_api_fetch_secs).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.retrieve_max_tool_calls).unwrap_or(i64::MAX))
    .bind(i64::try_from(config.max_calls_per_source).unwrap_or(i64::MAX))
    .bind(protected_persistence_text(message))
    .bind(i64::try_from(config.deadline_timeout_secs).unwrap_or(i64::MAX))
    .execute(db)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            tenant_id = %tenant_id,
            user_id = %user_id,
            session_id = %session_id,
            error = %error,
            "persist_pm_run_start failed"
        );
    }
    if let Err(error) = reserve_pm_resource_budget(db, run_id, tenant_id, user_id, config).await {
        tracing::warn!(
            run_id = %run_id,
            tenant_id = %tenant_id,
            error = %error,
            "failed to reserve PM semantic-kernel resource budget"
        );
    }
}

pub async fn persist_pm_stage_attempt(
    db: &sqlx::SqlitePool,
    run_id: &str,
    stage: &str,
    attempt: usize,
    status: &str,
    detail: Option<&Value>,
    elapsed_ms: Option<u64>,
    strategy: Option<&str>,
    route: Option<&str>,
    channel: Option<&str>,
    variant: Option<&str>,
) {
    if let Err(error) = persist_pm_stage_attempt_result(
        db, run_id, stage, attempt, status, detail, elapsed_ms, strategy, route, channel, variant,
    )
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            stage = %stage,
            status = %status,
            attempt,
            error = %error,
            "persist_pm_stage_attempt failed"
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn persist_pm_stage_attempt_result(
    db: &sqlx::SqlitePool,
    run_id: &str,
    stage: &str,
    attempt: usize,
    status: &str,
    detail: Option<&Value>,
    elapsed_ms: Option<u64>,
    strategy: Option<&str>,
    route: Option<&str>,
    channel: Option<&str>,
    variant: Option<&str>,
) -> Result<(), sqlx::Error> {
    persist_pm_stage_attempt_batch_result(
        db,
        &[PmStageAttemptWrite {
            run_id,
            stage,
            attempt,
            status,
            detail,
            elapsed_ms,
            strategy,
            route,
            channel,
            variant,
        }],
    )
    .await
}

#[derive(Debug, Clone, Copy)]
pub struct PmStageAttemptWrite<'a> {
    pub run_id: &'a str,
    pub stage: &'a str,
    pub attempt: usize,
    pub status: &'a str,
    pub detail: Option<&'a Value>,
    pub elapsed_ms: Option<u64>,
    pub strategy: Option<&'a str>,
    pub route: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub variant: Option<&'a str>,
}

#[derive(Debug)]
struct PreparedPmStageAttempt<'a> {
    run_id: &'a str,
    stage: &'a str,
    attempt: i64,
    status: &'a str,
    detail_raw: Option<String>,
    elapsed_ms: Option<i64>,
    strategy: Option<String>,
    route: Option<String>,
    channel: Option<String>,
    variant: Option<String>,
}

fn prepare_pm_stage_attempt<'a>(row: &PmStageAttemptWrite<'a>) -> PreparedPmStageAttempt<'a> {
    let detail_raw = protected_persistence_json(row.detail);
    let detail_obj = row.detail.and_then(Value::as_object);
    let detail_text = |keys: &[&str]| -> Option<String> {
        let obj = detail_obj?;
        for key in keys {
            let Some(value) = obj.get(*key) else {
                continue;
            };
            let Some(raw) = value.as_str() else {
                continue;
            };
            let normalized = raw.trim();
            if !normalized.is_empty() {
                return Some(normalized.to_string());
            }
        }
        None
    };
    let strategy = row
        .strategy
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .or_else(|| detail_text(&["strategy"]));
    let route = row
        .route
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .or_else(|| detail_text(&["selectedRoute", "nextRoute", "route"]));
    let channel = row
        .channel
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .or_else(|| detail_text(&["selectedRouteChannel", "nextRouteChannel", "channel"]));
    let variant = row
        .variant
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string)
        .or_else(|| detail_text(&["selectedVariant", "nextVariant", "variant"]));
    let elapsed_ms = row.elapsed_ms.or_else(|| {
        let parse_ms = |value: &Value| -> Option<u64> {
            if let Some(v) = value.as_u64() {
                return Some(v);
            }
            if let Some(v) = value.as_i64() {
                if v >= 0 {
                    return u64::try_from(v).ok();
                }
            }
            if let Some(v) = value.as_f64() {
                if v.is_finite() && v >= 0.0 {
                    return Some(v.round() as u64);
                }
            }
            value.as_str().and_then(|raw| raw.parse::<u64>().ok())
        };
        let obj = row.detail.and_then(Value::as_object)?;
        for key in ["elapsedMs", "durationMs", "delayMs"] {
            let Some(value) = obj.get(key) else {
                continue;
            };
            if let Some(v) = parse_ms(value) {
                return Some(v);
            }
        }
        // Fallback for preflight-style payloads that report component latencies.
        let model_ms = obj.get("modelLatencyMs").and_then(parse_ms);
        let retrieval_ms = obj.get("retrievalLatencyMs").and_then(parse_ms);
        if model_ms.is_some() || retrieval_ms.is_some() {
            let total = model_ms
                .unwrap_or(0)
                .saturating_add(retrieval_ms.unwrap_or(0));
            if total > 0 {
                return Some(total);
            }
            return model_ms.or(retrieval_ms);
        }
        let search_ms = obj.get("retrievalSearchLatencyMs").and_then(parse_ms);
        let browser_ms = obj.get("retrievalBrowserLatencyMs").and_then(parse_ms);
        match (search_ms, browser_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    });

    PreparedPmStageAttempt {
        run_id: row.run_id,
        stage: row.stage,
        attempt: i64::try_from(row.attempt).unwrap_or(i64::MAX),
        status: row.status,
        detail_raw,
        elapsed_ms: elapsed_ms.and_then(|value| i64::try_from(value).ok()),
        strategy,
        route,
        channel,
        variant,
    }
}

pub async fn persist_pm_stage_attempt_batch_result(
    db: &sqlx::SqlitePool,
    rows: &[PmStageAttemptWrite<'_>],
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }

    let prepared = rows
        .iter()
        .map(prepare_pm_stage_attempt)
        .collect::<Vec<_>>();
    for chunk in prepared.chunks(100) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_research_stage_attempts
            (run_id, stage, attempt_no, status, detail_json, elapsed_ms, strategy,
             route_key, channel, variant, started_at, ended_at) ",
        );
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(row.run_id)
                .push_bind(row.stage)
                .push_bind(row.attempt)
                .push_bind(row.status)
                .push_bind(row.detail_raw.as_deref())
                .push_bind(row.elapsed_ms)
                .push_bind(row.strategy.as_deref())
                .push_bind(row.route.as_deref())
                .push_bind(row.channel.as_deref())
                .push_bind(row.variant.as_deref())
                .push("CURRENT_TIMESTAMP")
                .push(
                    if matches!(row.status, "completed" | "failed" | "cancelled") {
                        "CURRENT_TIMESTAMP"
                    } else {
                        "NULL"
                    },
                );
        });
        query.push(
            " ON CONFLICT DO UPDATE SET
            status = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN status
                ELSE excluded.status
            END,
            detail_json = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN detail_json
                ELSE excluded.detail_json
            END,
            elapsed_ms = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN elapsed_ms
                ELSE COALESCE(
                    excluded.elapsed_ms,
                    elapsed_ms,
                    CASE
                        WHEN excluded.status IN ('completed','failed','cancelled') AND started_at IS NOT NULL
                        THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(started_at)) * 86400000000) / 1000, 0)
                        ELSE NULL
                    END
                )
            END,
            strategy = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN strategy
                ELSE COALESCE(excluded.strategy, strategy)
            END,
            route_key = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN route_key
                ELSE COALESCE(excluded.route_key, route_key)
            END,
            channel = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN channel
                ELSE COALESCE(excluded.channel, channel)
            END,
            variant = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN variant
                ELSE COALESCE(excluded.variant, variant)
            END,
            ended_at = CASE
                WHEN status IN ('completed','failed','cancelled') AND excluded.status = 'running'
                THEN ended_at
                WHEN excluded.status IN ('completed','failed','cancelled')
                THEN CURRENT_TIMESTAMP
                ELSE ended_at
            END,
            updated_at = CURRENT_TIMESTAMP",
        );
        query.build().execute(db).await?;
    }

    // A batch commonly contains several transitions for the same run. Keep
    // only the strongest stale-attempt cleanup and the latest run projection.
    let mut terminal_updates = BTreeMap::<(&str, &str), (&str, i64)>::new();
    let mut latest_run_updates = BTreeMap::<&str, (&str, i64)>::new();
    for row in &prepared {
        if matches!(row.status, "completed" | "failed" | "cancelled") {
            terminal_updates
                .entry((row.run_id, row.stage))
                .and_modify(|current| {
                    if row.attempt >= current.1 {
                        *current = (row.status, row.attempt);
                    }
                })
                .or_insert((row.status, row.attempt));
        }
        latest_run_updates
            .entry(row.run_id)
            .and_modify(|current| {
                current.0 = row.stage;
                current.1 = current.1.max(row.attempt);
            })
            .or_insert((row.stage, row.attempt));
    }

    for ((run_id, stage), (status, attempt)) in terminal_updates {
        sqlx::query(
            "UPDATE pm_research_stage_attempts
             SET status = ?,
                 elapsed_ms = COALESCE(
                    elapsed_ms,
                    CASE
                        WHEN started_at IS NOT NULL
                        THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(started_at)) * 86400000000) / 1000, 0)
                        ELSE NULL
                    END
                 ),
                 ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
                 updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?
               AND stage = ?
               AND attempt_no < ?
               AND status = 'running'",
        )
        .bind(status)
        .bind(run_id)
        .bind(stage)
        .bind(attempt)
        .execute(db)
        .await?;
    }

    for (run_id, (stage, attempt)) in latest_run_updates {
        sqlx::query(
            "UPDATE pm_research_runs
         SET current_stage = ?,
             attempt = MAX(COALESCE(attempt, 1), ?),
             total_elapsed_ms = CASE
                WHEN status IN ('completed','failed','cancelled') THEN total_elapsed_ms
                WHEN started_at IS NOT NULL
                THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(started_at)) * 86400000000) / 1000, 0)
                ELSE total_elapsed_ms
             END,
             ended_at = CASE
                WHEN status IN ('completed','failed','cancelled') THEN ended_at
                ELSE NULL
             END,
             updated_at = CURRENT_TIMESTAMP
         WHERE run_id = ?
           AND status NOT IN ('completed','failed','cancelled')",
        )
        .bind(stage)
        .bind(attempt)
        .bind(run_id)
        .execute(db)
        .await?;
    }
    Ok(())
}

pub async fn persist_pm_run_finish(
    db: &sqlx::SqlitePool,
    run_id: &str,
    payload: &PmRunFinishPayload,
) {
    let metadata_raw = payload.metadata.as_ref().map(Value::to_string);
    let run_finished = matches!(
        payload.status.as_str(),
        "completed" | "failed" | "cancelled"
    );
    let terminal_stage_status = match payload.status.as_str() {
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => "completed",
    };
    if let Err(error) = sqlx::query(
        "UPDATE pm_research_runs
         SET status = ?,
             current_stage = ?,
             attempt = ?,
             total_elapsed_ms = COALESCE(
                ?,
                CASE
                    WHEN started_at IS NOT NULL
                    THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(started_at)) * 86400000000) / 1000, 0)
                    ELSE NULL
                END
             ),
             error_code = ?,
             error_message = ?,
             final_quality_score = ?,
             metadata_json = ?,
             ended_at = CURRENT_TIMESTAMP,
             updated_at = CURRENT_TIMESTAMP
         WHERE run_id = ?",
    )
    .bind(&payload.status)
    .bind(payload.current_stage.as_deref())
    .bind(payload.attempt.and_then(|x| i64::try_from(x).ok()))
    .bind(payload.total_elapsed_ms.and_then(|x| i64::try_from(x).ok()))
    .bind(payload.error_code.as_deref())
    .bind(payload.error_message.as_deref())
    .bind(payload.final_quality_score)
    .bind(metadata_raw)
    .bind(run_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            status = %payload.status,
            stage = ?payload.current_stage,
            error = %error,
            "persist_pm_run_finish failed"
        );
    }

    if run_finished {
        if let Err(error) = sqlx::query(
            "UPDATE pm_research_stage_attempts
         SET status = ?,
             elapsed_ms = COALESCE(
                elapsed_ms,
                CASE
                    WHEN started_at IS NOT NULL
                    THEN MAX(((julianday(CURRENT_TIMESTAMP) - julianday(started_at)) * 86400000000) / 1000, 0)
                    ELSE NULL
                END
             ),
             ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
             updated_at = CURRENT_TIMESTAMP
         WHERE run_id = ?
           AND status = 'running'",
        )
        .bind(terminal_stage_status)
        .bind(run_id)
        .execute(db)
        .await
        {
            tracing::warn!(
                run_id = %run_id,
                status = %payload.status,
                error = %error,
                "persist_pm_run_finish failed to close stale running stage attempts"
            );
        }

        let child_status = match payload.status.as_str() {
            "cancelled" => "cancelled",
            "failed" => "failed",
            _ => "skipped",
        };
        let child_error_code = match payload.status.as_str() {
            "cancelled" => "parent_cancelled",
            "failed" => "parent_failed",
            _ => "parent_completed_without_execution",
        };
        for (table, statement) in [
            (
                "pm_subtask_runs",
                "UPDATE pm_subtask_runs
                 SET status = ?, error_code = ?,
                     error_message = CASE
                         WHEN error_message IS NULL OR TRIM(error_message) = ''
                         THEN 'Parent PM run reached a terminal state'
                         ELSE error_message || ' | Parent PM run reached a terminal state'
                     END,
                     ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE run_id = ? AND status IN ('queued','running')",
            ),
            (
                "pm_subtask_attempts",
                "UPDATE pm_subtask_attempts
                 SET status = ?, error_code = ?,
                     error_message = CASE
                         WHEN error_message IS NULL OR TRIM(error_message) = ''
                         THEN 'Parent PM run reached a terminal state'
                         ELSE error_message || ' | Parent PM run reached a terminal state'
                     END,
                     ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE run_id = ? AND status IN ('queued','running')",
            ),
        ] {
            if let Err(error) = sqlx::query(statement)
                .bind(child_status)
                .bind(child_error_code)
                .bind(run_id)
                .execute(db)
                .await
            {
                tracing::warn!(
                    run_id = %run_id,
                    status = %payload.status,
                    table,
                    error = %error,
                    "persist_pm_run_finish failed to close unresolved child work"
                );
            }
        }
    }
    if let Err(error) = settle_pm_resource_budget(db, run_id, payload).await {
        tracing::warn!(
            run_id = %run_id,
            status = %payload.status,
            error = %error,
            "failed to settle PM semantic-kernel resource budget"
        );
    }
}

pub async fn upsert_pm_source_slot(
    db: &sqlx::SqlitePool,
    payload: &PmSourceSlotUpsertPayload,
) -> Option<u64> {
    upsert_pm_source_slot_result(db, payload).await.ok()
}

pub async fn upsert_pm_source_slot_result(
    db: &sqlx::SqlitePool,
    payload: &PmSourceSlotUpsertPayload,
) -> Result<u64, sqlx::Error> {
    let detail_raw = protected_persistence_json(payload.detail.as_ref());
    let protected_source_url = payload
        .source_url
        .as_deref()
        .map(protected_persistence_text);
    let protected_error_message = payload
        .error_message
        .as_deref()
        .map(protected_persistence_text);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pm_research_source_slots
            (run_id, stage_attempt_id, slot_seq, route_key, channel, variant, source_key, source_url,
             status, tool_call_count, elapsed_ms, error_code, error_message, detail_json, started_at, ended_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP,
                 CASE WHEN ? IN ('completed','failed','timed_out','skipped') THEN CURRENT_TIMESTAMP ELSE NULL END)
         ON CONFLICT DO UPDATE SET
            stage_attempt_id = COALESCE(excluded.stage_attempt_id, pm_research_source_slots.stage_attempt_id),
            route_key = COALESCE(excluded.route_key, pm_research_source_slots.route_key),
            channel = COALESCE(excluded.channel, pm_research_source_slots.channel),
            variant = COALESCE(excluded.variant, pm_research_source_slots.variant),
            source_key = COALESCE(excluded.source_key, pm_research_source_slots.source_key),
            source_url = COALESCE(excluded.source_url, pm_research_source_slots.source_url),
            status = excluded.status,
            tool_call_count = excluded.tool_call_count,
            elapsed_ms = excluded.elapsed_ms,
            error_code = excluded.error_code,
            error_message = excluded.error_message,
            detail_json = excluded.detail_json,
            started_at = COALESCE(pm_research_source_slots.started_at, CURRENT_TIMESTAMP),
            ended_at = CASE
                WHEN excluded.status IN ('completed','failed','timed_out','skipped') THEN CURRENT_TIMESTAMP
                ELSE pm_research_source_slots.ended_at
            END,
            updated_at = CURRENT_TIMESTAMP
         RETURNING id",
    )
    .bind(&payload.run_id)
    .bind(payload.stage_attempt_id.and_then(|x| i64::try_from(x).ok()))
    .bind(i64::try_from(payload.slot_seq).unwrap_or(i64::MAX))
    .bind(payload.route_key.as_deref())
    .bind(payload.channel.as_deref())
    .bind(payload.variant.as_deref())
    .bind(payload.source_key.as_deref())
    .bind(protected_source_url)
    .bind(&payload.status)
    .bind(i64::try_from(payload.tool_call_count).unwrap_or(i64::MAX))
    .bind(payload.elapsed_ms.and_then(|x| i64::try_from(x).ok()))
    .bind(payload.error_code.as_deref())
    .bind(protected_error_message)
    .bind(detail_raw)
    .bind(&payload.status)
    .fetch_one(db)
    .await?;
    u64::try_from(id).map_err(|_| sqlx::Error::Protocol("negative SQLite rowid".to_string()))
}

pub async fn upsert_pm_tool_call_ledger(db: &sqlx::SqlitePool, row: &PmToolCallLedgerRow) {
    let mut row = row.clone();
    for value in [
        &mut row.input_preview,
        &mut row.output_preview,
        &mut row.input_raw,
        &mut row.output_raw,
        &mut row.error_message,
        &mut row.provider_trace,
        &mut row.url,
    ] {
        protect_pm_ledger_value(value);
    }

    let insert_with_raw = sqlx::query(
        "INSERT INTO pm_research_tool_call_ledger
            (run_id, stage_attempt_id, source_slot_id, call_seq, tool_name, tool_use_id,
             input_preview, output_preview, input_raw, output_raw,
             is_error, error_code, error_message, http_status,
             latency_ms, route_key, channel, provider, provider_trace, url, domain)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            stage_attempt_id = COALESCE(excluded.stage_attempt_id, stage_attempt_id),
            source_slot_id = COALESCE(excluded.source_slot_id, source_slot_id),
            tool_name = excluded.tool_name,
            tool_use_id = excluded.tool_use_id,
            input_preview = excluded.input_preview,
            output_preview = excluded.output_preview,
            input_raw = COALESCE(excluded.input_raw, input_raw),
            output_raw = COALESCE(excluded.output_raw, output_raw),
            is_error = excluded.is_error,
            error_code = excluded.error_code,
            error_message = excluded.error_message,
            http_status = excluded.http_status,
            latency_ms = excluded.latency_ms,
            route_key = COALESCE(excluded.route_key, route_key),
            channel = COALESCE(excluded.channel, channel),
            provider = COALESCE(excluded.provider, provider),
            provider_trace = COALESCE(excluded.provider_trace, provider_trace),
            url = COALESCE(excluded.url, url),
            domain = COALESCE(excluded.domain, domain)",
    )
    .bind(&row.run_id)
    .bind(row.stage_attempt_id.and_then(|x| i64::try_from(x).ok()))
    .bind(row.source_slot_id.and_then(|x| i64::try_from(x).ok()))
    .bind(i64::try_from(row.call_seq).unwrap_or(i64::MAX))
    .bind(&row.tool_name)
    .bind(row.tool_use_id.as_deref())
    .bind(row.input_preview.as_deref())
    .bind(row.output_preview.as_deref())
    .bind(row.input_raw.as_deref())
    .bind(row.output_raw.as_deref())
    .bind(if row.is_error { 1i64 } else { 0i64 })
    .bind(row.error_code.as_deref())
    .bind(row.error_message.as_deref())
    .bind(row.http_status)
    .bind(row.latency_ms.and_then(|x| i64::try_from(x).ok()))
    .bind(row.route_key.as_deref())
    .bind(row.channel.as_deref())
    .bind(row.provider.as_deref())
    .bind(row.provider_trace.as_deref())
    .bind(row.url.as_deref())
    .bind(row.domain.as_deref())
    .execute(db)
    .await;

    if insert_with_raw.is_ok() {
        return;
    }

    let insert_with_provider = sqlx::query(
        "INSERT INTO pm_research_tool_call_ledger
            (run_id, stage_attempt_id, source_slot_id, call_seq, tool_name, tool_use_id,
             input_preview, output_preview, is_error, error_code, error_message, http_status,
             latency_ms, route_key, channel, provider, provider_trace, url, domain)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            stage_attempt_id = COALESCE(excluded.stage_attempt_id, stage_attempt_id),
            source_slot_id = COALESCE(excluded.source_slot_id, source_slot_id),
            tool_name = excluded.tool_name,
            tool_use_id = excluded.tool_use_id,
            input_preview = excluded.input_preview,
            output_preview = excluded.output_preview,
            is_error = excluded.is_error,
            error_code = excluded.error_code,
            error_message = excluded.error_message,
            http_status = excluded.http_status,
            latency_ms = excluded.latency_ms,
            route_key = COALESCE(excluded.route_key, route_key),
            channel = COALESCE(excluded.channel, channel),
            provider = COALESCE(excluded.provider, provider),
            provider_trace = COALESCE(excluded.provider_trace, provider_trace),
            url = COALESCE(excluded.url, url),
            domain = COALESCE(excluded.domain, domain)",
    )
    .bind(&row.run_id)
    .bind(row.stage_attempt_id.and_then(|x| i64::try_from(x).ok()))
    .bind(row.source_slot_id.and_then(|x| i64::try_from(x).ok()))
    .bind(i64::try_from(row.call_seq).unwrap_or(i64::MAX))
    .bind(&row.tool_name)
    .bind(row.tool_use_id.as_deref())
    .bind(row.input_preview.as_deref())
    .bind(row.output_preview.as_deref())
    .bind(if row.is_error { 1i64 } else { 0i64 })
    .bind(row.error_code.as_deref())
    .bind(row.error_message.as_deref())
    .bind(row.http_status)
    .bind(row.latency_ms.and_then(|x| i64::try_from(x).ok()))
    .bind(row.route_key.as_deref())
    .bind(row.channel.as_deref())
    .bind(row.provider.as_deref())
    .bind(row.provider_trace.as_deref())
    .bind(row.url.as_deref())
    .bind(row.domain.as_deref())
    .execute(db)
    .await;

    if insert_with_provider.is_ok() {
        return;
    }

    if let Err(error) = insert_with_provider {
        let message = error.to_string().to_ascii_lowercase();
        let schema_missing_provider = message.contains("unknown column")
            && (message.contains("provider") || message.contains("provider_trace"));
        if schema_missing_provider {
            let _ = sqlx::query(
                "INSERT INTO pm_research_tool_call_ledger
                    (run_id, stage_attempt_id, source_slot_id, call_seq, tool_name, tool_use_id,
                     input_preview, output_preview, is_error, error_code, error_message, http_status,
                     latency_ms, route_key, channel, url, domain)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT DO UPDATE SET
                    stage_attempt_id = COALESCE(excluded.stage_attempt_id, stage_attempt_id),
                    source_slot_id = COALESCE(excluded.source_slot_id, source_slot_id),
                    tool_name = excluded.tool_name,
                    tool_use_id = excluded.tool_use_id,
                    input_preview = excluded.input_preview,
                    output_preview = excluded.output_preview,
                    is_error = excluded.is_error,
                    error_code = excluded.error_code,
                    error_message = excluded.error_message,
                    http_status = excluded.http_status,
                    latency_ms = excluded.latency_ms,
                    route_key = COALESCE(excluded.route_key, route_key),
                    channel = COALESCE(excluded.channel, channel),
                    url = COALESCE(excluded.url, url),
                    domain = COALESCE(excluded.domain, domain)",
            )
            .bind(&row.run_id)
            .bind(row.stage_attempt_id.and_then(|x| i64::try_from(x).ok()))
            .bind(row.source_slot_id.and_then(|x| i64::try_from(x).ok()))
            .bind(i64::try_from(row.call_seq).unwrap_or(i64::MAX))
            .bind(&row.tool_name)
            .bind(row.tool_use_id.as_deref())
            .bind(row.input_preview.as_deref())
            .bind(row.output_preview.as_deref())
            .bind(if row.is_error { 1i64 } else { 0i64 })
            .bind(row.error_code.as_deref())
            .bind(row.error_message.as_deref())
            .bind(row.http_status)
            .bind(row.latency_ms.and_then(|x| i64::try_from(x).ok()))
            .bind(row.route_key.as_deref())
            .bind(row.channel.as_deref())
            .bind(row.url.as_deref())
            .bind(row.domain.as_deref())
            .execute(db)
            .await;
        }
    }
}

pub async fn upsert_pm_tool_call_ledger_batch(db: &sqlx::SqlitePool, rows: &[PmToolCallLedgerRow]) {
    if let Err(error) = upsert_pm_tool_call_ledger_batch_result(db, rows).await {
        tracing::error!(
            row_count = rows.len(),
            %error,
            "batch tool ledger upsert failed; skipping ledger batch without row-by-row retry"
        );
    }
}

pub async fn upsert_pm_tool_call_ledger_batch_result(
    db: &sqlx::SqlitePool,
    rows: &[PmToolCallLedgerRow],
) -> Result<(), sqlx::Error> {
    const BATCH_SIZE: usize = 100;

    for chunk in rows.chunks(BATCH_SIZE) {
        let protected = chunk
            .iter()
            .cloned()
            .map(|mut row| {
                for value in [
                    &mut row.input_preview,
                    &mut row.output_preview,
                    &mut row.input_raw,
                    &mut row.output_raw,
                    &mut row.error_message,
                    &mut row.provider_trace,
                    &mut row.url,
                ] {
                    protect_pm_ledger_value(value);
                }
                row
            })
            .collect::<Vec<_>>();

        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_research_tool_call_ledger
                (run_id, stage_attempt_id, source_slot_id, call_seq, tool_name, tool_use_id,
                 input_preview, output_preview, input_raw, output_raw,
                 is_error, error_code, error_message, http_status,
                 latency_ms, route_key, channel, provider, provider_trace, url, domain) ",
        );
        query.push_values(&protected, |mut values, row| {
            values
                .push_bind(&row.run_id)
                .push_bind(row.stage_attempt_id.and_then(|x| i64::try_from(x).ok()))
                .push_bind(row.source_slot_id.and_then(|x| i64::try_from(x).ok()))
                .push_bind(i64::try_from(row.call_seq).unwrap_or(i64::MAX))
                .push_bind(&row.tool_name)
                .push_bind(row.tool_use_id.as_deref())
                .push_bind(row.input_preview.as_deref())
                .push_bind(row.output_preview.as_deref())
                .push_bind(row.input_raw.as_deref())
                .push_bind(row.output_raw.as_deref())
                .push_bind(if row.is_error { 1i64 } else { 0i64 })
                .push_bind(row.error_code.as_deref())
                .push_bind(row.error_message.as_deref())
                .push_bind(row.http_status)
                .push_bind(row.latency_ms.and_then(|x| i64::try_from(x).ok()))
                .push_bind(row.route_key.as_deref())
                .push_bind(row.channel.as_deref())
                .push_bind(row.provider.as_deref())
                .push_bind(row.provider_trace.as_deref())
                .push_bind(row.url.as_deref())
                .push_bind(row.domain.as_deref());
        });
        query.push(
            " ON CONFLICT DO UPDATE SET
                stage_attempt_id = COALESCE(excluded.stage_attempt_id, stage_attempt_id),
                source_slot_id = COALESCE(excluded.source_slot_id, source_slot_id),
                tool_name = excluded.tool_name, tool_use_id = excluded.tool_use_id,
                input_preview = excluded.input_preview, output_preview = excluded.output_preview,
                input_raw = COALESCE(excluded.input_raw, input_raw),
                output_raw = COALESCE(excluded.output_raw, output_raw),
                is_error = excluded.is_error, error_code = excluded.error_code,
                error_message = excluded.error_message, http_status = excluded.http_status,
                latency_ms = excluded.latency_ms,
                route_key = COALESCE(excluded.route_key, route_key),
                channel = COALESCE(excluded.channel, channel),
                provider = COALESCE(excluded.provider, provider),
                provider_trace = COALESCE(excluded.provider_trace, provider_trace),
                url = COALESCE(excluded.url, url), domain = COALESCE(excluded.domain, domain)",
        );

        query.build().execute(db).await?;
    }
    Ok(())
}

fn protect_pm_ledger_value(value: &mut Option<String>) {
    if let Some(text) = value.as_mut() {
        *text =
            runtime::protect_sensitive_text(text, runtime::configured_data_protection_mode()).value;
    }
}

pub async fn upsert_pm_claim_verdict(db: &sqlx::SqlitePool, row: &PmClaimVerdictRow) {
    let result = sqlx::query(
        "INSERT INTO pm_claim_verdicts
            (tenant_id, run_id, claim_key, claim_text, verdict, confidence, evidence_excerpt, url, domain, reason)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            claim_text = excluded.claim_text,
            verdict = excluded.verdict,
            confidence = excluded.confidence,
            evidence_excerpt = excluded.evidence_excerpt,
            url = excluded.url,
            domain = excluded.domain,
            reason = excluded.reason,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&row.tenant_id)
    .bind(&row.run_id)
    .bind(&row.claim_key)
    .bind(&row.claim_text)
    .bind(&row.verdict)
    .bind(row.confidence.clamp(0.0, 1.0))
    .bind(row.evidence_excerpt.as_deref())
    .bind(row.url.as_deref())
    .bind(row.domain.as_deref())
    .bind(row.reason.as_deref())
    .execute(db)
    .await;
    if let Err(error) = result {
        tracing::warn!(
            tenant_id = %row.tenant_id,
            run_id = %row.run_id,
            claim_key = %row.claim_key,
            error = %error,
            "claim verdict upsert failed"
        );
    }
}

pub async fn upsert_pm_claim_verdict_batch(db: &sqlx::SqlitePool, rows: &[PmClaimVerdictRow]) {
    for chunk in rows.chunks(100) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_claim_verdicts
                (tenant_id, run_id, claim_key, claim_text, verdict, confidence,
                 evidence_excerpt, url, domain, reason) ",
        );
        query.push_values(chunk, |mut values, row| {
            values
                .push_bind(&row.tenant_id)
                .push_bind(&row.run_id)
                .push_bind(&row.claim_key)
                .push_bind(&row.claim_text)
                .push_bind(&row.verdict)
                .push_bind(row.confidence.clamp(0.0, 1.0))
                .push_bind(row.evidence_excerpt.as_deref())
                .push_bind(row.url.as_deref())
                .push_bind(row.domain.as_deref())
                .push_bind(row.reason.as_deref());
        });
        query.push(
            " ON CONFLICT DO UPDATE SET claim_text = excluded.claim_text,
                verdict = excluded.verdict, confidence = excluded.confidence,
                evidence_excerpt = excluded.evidence_excerpt, url = excluded.url,
                domain = excluded.domain, reason = excluded.reason, updated_at = CURRENT_TIMESTAMP",
        );
        if let Err(error) = query.build().execute(db).await {
            tracing::warn!(row_count = chunk.len(), %error, "batch claim verdict upsert failed");
        }
    }
}

pub async fn upsert_pm_conflict_case(db: &sqlx::SqlitePool, row: &PmConflictCaseRow) {
    let support_urls_raw = row.support_urls.as_ref().map(Value::to_string);
    let _ = sqlx::query(
        "INSERT INTO pm_conflict_cases
            (tenant_id, run_id, topic_key, topic, source_a, claim_a, source_b, claim_b, verdict, confidence, reason, support_urls_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            topic = excluded.topic,
            source_a = excluded.source_a,
            claim_a = excluded.claim_a,
            source_b = excluded.source_b,
            claim_b = excluded.claim_b,
            verdict = excluded.verdict,
            confidence = excluded.confidence,
            reason = excluded.reason,
            support_urls_json = excluded.support_urls_json,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&row.tenant_id)
    .bind(&row.run_id)
    .bind(&row.topic_key)
    .bind(&row.topic)
    .bind(row.source_a.as_deref())
    .bind(row.claim_a.as_deref())
    .bind(row.source_b.as_deref())
    .bind(row.claim_b.as_deref())
    .bind(row.verdict.as_deref())
    .bind(row.confidence.clamp(0.0, 1.0))
    .bind(row.reason.as_deref())
    .bind(support_urls_raw)
    .execute(db)
    .await;
}

pub async fn upsert_pm_conflict_case_batch(db: &sqlx::SqlitePool, rows: &[PmConflictCaseRow]) {
    for chunk in rows.chunks(100) {
        let encoded = chunk
            .iter()
            .map(|row| (row, row.support_urls.as_ref().map(Value::to_string)))
            .collect::<Vec<_>>();
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO pm_conflict_cases
                (tenant_id, run_id, topic_key, topic, source_a, claim_a, source_b,
                 claim_b, verdict, confidence, reason, support_urls_json) ",
        );
        query.push_values(&encoded, |mut values, (row, support_urls)| {
            values
                .push_bind(&row.tenant_id)
                .push_bind(&row.run_id)
                .push_bind(&row.topic_key)
                .push_bind(&row.topic)
                .push_bind(row.source_a.as_deref())
                .push_bind(row.claim_a.as_deref())
                .push_bind(row.source_b.as_deref())
                .push_bind(row.claim_b.as_deref())
                .push_bind(row.verdict.as_deref())
                .push_bind(row.confidence.clamp(0.0, 1.0))
                .push_bind(row.reason.as_deref())
                .push_bind(support_urls.as_deref());
        });
        query.push(
            " ON CONFLICT DO UPDATE SET topic = excluded.topic, source_a = excluded.source_a,
                claim_a = excluded.claim_a, source_b = excluded.source_b,
                claim_b = excluded.claim_b, verdict = excluded.verdict,
                confidence = excluded.confidence, reason = excluded.reason,
                support_urls_json = excluded.support_urls_json, updated_at = CURRENT_TIMESTAMP",
        );
        if let Err(error) = query.build().execute(db).await {
            tracing::warn!(row_count = chunk.len(), %error, "batch conflict case upsert failed");
        }
    }
}

pub async fn upsert_pm_subtask_run(
    db: &sqlx::SqlitePool,
    payload: &PmSubtaskRunUpsertPayload,
) -> Option<u64> {
    let detail_raw = protected_persistence_json(payload.detail.as_ref());
    let quality_score = payload.quality_score.map(|value| value.clamp(0.0, 1.0));
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pm_subtask_runs
            (run_id, task_id, tenant_id, user_id, session_id, subtask_key, subtask_id,
             title, goal, deliverable, required_evidence_type, priority, status,
             probe_candidate_count, probe_completed_count, citation_count, domain_count, tool_call_count,
             quality_score, error_code, error_message, detail_json, started_at, ended_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 CASE WHEN ? IN ('running','completed','failed','skipped') THEN CURRENT_TIMESTAMP ELSE NULL END,
                 CASE WHEN ? IN ('completed','failed','skipped') THEN CURRENT_TIMESTAMP ELSE NULL END)
         ON CONFLICT DO UPDATE SET
            task_id = COALESCE(excluded.task_id, pm_subtask_runs.task_id),
            subtask_id = COALESCE(excluded.subtask_id, pm_subtask_runs.subtask_id),
            title = COALESCE(excluded.title, pm_subtask_runs.title),
            goal = COALESCE(excluded.goal, pm_subtask_runs.goal),
            deliverable = COALESCE(excluded.deliverable, pm_subtask_runs.deliverable),
            required_evidence_type = COALESCE(excluded.required_evidence_type, pm_subtask_runs.required_evidence_type),
            priority = excluded.priority,
            status = excluded.status,
            probe_candidate_count = MAX(pm_subtask_runs.probe_candidate_count, excluded.probe_candidate_count),
            probe_completed_count = MAX(pm_subtask_runs.probe_completed_count, excluded.probe_completed_count),
            citation_count = MAX(pm_subtask_runs.citation_count, excluded.citation_count),
            domain_count = MAX(pm_subtask_runs.domain_count, excluded.domain_count),
            tool_call_count = MAX(pm_subtask_runs.tool_call_count, excluded.tool_call_count),
            quality_score = CASE
              WHEN excluded.quality_score IS NULL THEN pm_subtask_runs.quality_score
              WHEN pm_subtask_runs.quality_score IS NULL THEN excluded.quality_score
              ELSE MAX(pm_subtask_runs.quality_score, excluded.quality_score)
            END,
            error_code = excluded.error_code,
            error_message = excluded.error_message,
            detail_json = excluded.detail_json,
            started_at = COALESCE(pm_subtask_runs.started_at, excluded.started_at, CURRENT_TIMESTAMP),
            ended_at = CASE
              WHEN excluded.status IN ('completed','failed','skipped') THEN CURRENT_TIMESTAMP
              ELSE pm_subtask_runs.ended_at
            END,
            updated_at = CURRENT_TIMESTAMP
         RETURNING id",
    )
    .bind(&payload.run_id)
    .bind(payload.task_id.as_deref())
    .bind(&payload.tenant_id)
    .bind(&payload.user_id)
    .bind(&payload.session_id)
    .bind(&payload.subtask_key)
    .bind(payload.subtask_id.as_deref())
    .bind(protected_persistence_text(&payload.title))
    .bind(payload.goal.as_deref().map(protected_persistence_text))
    .bind(
        payload
            .deliverable
            .as_deref()
            .map(protected_persistence_text),
    )
    .bind(payload.required_evidence_type.as_deref())
    .bind(&payload.priority)
    .bind(&payload.status)
    .bind(i64::try_from(payload.probe_candidate_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.probe_completed_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.citation_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.domain_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.tool_call_count).unwrap_or(i64::MAX))
    .bind(quality_score)
    .bind(payload.error_code.as_deref())
    .bind(
        payload
            .error_message
            .as_deref()
            .map(protected_persistence_text),
    )
    .bind(detail_raw)
    .bind(&payload.status)
    .bind(&payload.status)
    .fetch_one(db)
    .await
    .ok()?;
    u64::try_from(id).ok()
}

pub async fn upsert_pm_subtask_attempt(
    db: &sqlx::SqlitePool,
    payload: &PmSubtaskAttemptUpsertPayload,
) -> Option<u64> {
    let detail_raw = protected_persistence_json(payload.detail.as_ref());
    let quality_score = payload.quality_score.map(|value| value.clamp(0.0, 1.0));
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO pm_subtask_attempts
            (subtask_run_id, run_id, subtask_key, attempt_no, attempt_key, variant,
             route_key, route_channel, status, elapsed_ms, citation_count, domain_count,
             tool_call_count, quality_score, error_code, error_message, detail_json, started_at, ended_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 CASE WHEN ? IN ('running','completed','failed','timed_out','skipped') THEN CURRENT_TIMESTAMP ELSE NULL END,
                 CASE WHEN ? IN ('completed','failed','timed_out','skipped') THEN CURRENT_TIMESTAMP ELSE NULL END)
         ON CONFLICT DO UPDATE SET
            variant = COALESCE(excluded.variant, pm_subtask_attempts.variant),
            route_key = COALESCE(excluded.route_key, pm_subtask_attempts.route_key),
            route_channel = COALESCE(excluded.route_channel, pm_subtask_attempts.route_channel),
            status = excluded.status,
            elapsed_ms = excluded.elapsed_ms,
            citation_count = MAX(pm_subtask_attempts.citation_count, excluded.citation_count),
            domain_count = MAX(pm_subtask_attempts.domain_count, excluded.domain_count),
            tool_call_count = MAX(pm_subtask_attempts.tool_call_count, excluded.tool_call_count),
            quality_score = CASE
              WHEN excluded.quality_score IS NULL THEN pm_subtask_attempts.quality_score
              WHEN pm_subtask_attempts.quality_score IS NULL THEN excluded.quality_score
              ELSE MAX(pm_subtask_attempts.quality_score, excluded.quality_score)
            END,
            error_code = excluded.error_code,
            error_message = excluded.error_message,
            detail_json = excluded.detail_json,
            started_at = COALESCE(pm_subtask_attempts.started_at, excluded.started_at, CURRENT_TIMESTAMP),
            ended_at = CASE
              WHEN excluded.status IN ('completed','failed','timed_out','skipped') THEN CURRENT_TIMESTAMP
              ELSE pm_subtask_attempts.ended_at
            END,
            updated_at = CURRENT_TIMESTAMP
         RETURNING id",
    )
    .bind(i64::try_from(payload.subtask_run_id).unwrap_or(i64::MAX))
    .bind(&payload.run_id)
    .bind(&payload.subtask_key)
    .bind(i64::try_from(payload.attempt_no).unwrap_or(i64::MAX))
    .bind(&payload.attempt_key)
    .bind(payload.variant.as_deref())
    .bind(payload.route_key.as_deref())
    .bind(payload.route_channel.as_deref())
    .bind(&payload.status)
    .bind(payload.elapsed_ms.and_then(|v| i64::try_from(v).ok()))
    .bind(i64::try_from(payload.citation_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.domain_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(payload.tool_call_count).unwrap_or(i64::MAX))
    .bind(quality_score)
    .bind(payload.error_code.as_deref())
    .bind(
        payload
            .error_message
            .as_deref()
            .map(protected_persistence_text),
    )
    .bind(detail_raw)
    .bind(&payload.status)
    .bind(&payload.status)
    .fetch_one(db)
    .await
    .ok()?;
    u64::try_from(id).ok()
}

pub async fn list_pm_subtask_runs_by_task(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
    limit: usize,
) -> Vec<PmSubtaskRuntimeRow> {
    let fetch_limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
    let rows = match sqlx::query(
        "SELECT
            id, run_id, task_id, subtask_key, subtask_id, title, goal, deliverable,
            required_evidence_type, priority, status,
            CAST(probe_candidate_count AS INTEGER), CAST(probe_completed_count AS INTEGER),
            CAST(citation_count AS INTEGER), CAST(domain_count AS INTEGER), CAST(tool_call_count AS INTEGER),
            CAST(quality_score AS DOUBLE), error_code, error_message, detail_json,
            strftime('%Y-%m-%dT%H:%M:%SZ', started_at),
            strftime('%Y-%m-%dT%H:%M:%SZ', ended_at),
            strftime('%Y-%m-%dT%H:%M:%SZ', updated_at)
         FROM pm_subtask_runs
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
         ORDER BY updated_at DESC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .bind(fetch_limit)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(PmSubtaskRuntimeRow {
            id: row.get::<u64, _>(0),
            run_id: row.get::<String, _>(1),
            task_id: row.get::<Option<String>, _>(2),
            subtask_key: row.get::<String, _>(3),
            subtask_id: row.get::<Option<String>, _>(4),
            title: row.get::<String, _>(5),
            goal: row.get::<Option<String>, _>(6),
            deliverable: row.get::<Option<String>, _>(7),
            required_evidence_type: row.get::<Option<String>, _>(8),
            priority: row.get::<String, _>(9),
            status: row.get::<String, _>(10),
            probe_candidate_count: usize::try_from(row.get::<i64, _>(11).max(0)).unwrap_or(0),
            probe_completed_count: usize::try_from(row.get::<i64, _>(12).max(0)).unwrap_or(0),
            citation_count: usize::try_from(row.get::<i64, _>(13).max(0)).unwrap_or(0),
            domain_count: usize::try_from(row.get::<i64, _>(14).max(0)).unwrap_or(0),
            tool_call_count: usize::try_from(row.get::<i64, _>(15).max(0)).unwrap_or(0),
            quality_score: row.get::<Option<f64>, _>(16).map(|v| v.clamp(0.0, 1.0)),
            error_code: row.get::<Option<String>, _>(17),
            error_message: row.get::<Option<String>, _>(18),
            detail: row.get::<Option<Value>, _>(19),
            started_at: row.get::<Option<String>, _>(20),
            ended_at: row.get::<Option<String>, _>(21),
            updated_at: row.get::<Option<String>, _>(22),
        });
    }
    out
}

pub async fn list_pm_subtask_attempts_by_task_and_subtask(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    task_id: &str,
    subtask_ref: &str,
    limit: usize,
) -> Vec<PmSubtaskAttemptRow> {
    let fetch_limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
    let rows = match sqlx::query(
        "SELECT
            a.id, a.subtask_run_id, a.run_id, a.subtask_key, CAST(a.attempt_no AS INTEGER), a.attempt_key,
            a.variant, a.route_key, a.route_channel, a.status, a.elapsed_ms,
            CAST(a.citation_count AS INTEGER), CAST(a.domain_count AS INTEGER), CAST(a.tool_call_count AS INTEGER),
            CAST(a.quality_score AS DOUBLE), a.error_code, a.error_message, a.detail_json,
            strftime('%Y-%m-%dT%H:%M:%SZ', a.started_at),
            strftime('%Y-%m-%dT%H:%M:%SZ', a.ended_at),
            strftime('%Y-%m-%dT%H:%M:%SZ', a.updated_at)
         FROM pm_subtask_attempts a
         JOIN pm_subtask_runs r ON r.id = a.subtask_run_id
         WHERE r.tenant_id = ? AND r.user_id = ? AND r.task_id = ?
           AND (r.subtask_id = ? OR r.subtask_key = ? OR r.title = ?)
         ORDER BY a.attempt_no ASC, a.id ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(task_id)
    .bind(subtask_ref)
    .bind(subtask_ref)
    .bind(subtask_ref)
    .bind(fetch_limit)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(PmSubtaskAttemptRow {
            id: row.get::<u64, _>(0),
            subtask_run_id: row.get::<u64, _>(1),
            run_id: row.get::<String, _>(2),
            subtask_key: row.get::<String, _>(3),
            attempt_no: usize::try_from(row.get::<i64, _>(4).max(0)).unwrap_or(0),
            attempt_key: row.get::<String, _>(5),
            variant: row.get::<Option<String>, _>(6),
            route_key: row.get::<Option<String>, _>(7),
            route_channel: row.get::<Option<String>, _>(8),
            status: row.get::<String, _>(9),
            elapsed_ms: row
                .get::<Option<i64>, _>(10)
                .and_then(|v| u64::try_from(v.max(0)).ok()),
            citation_count: usize::try_from(row.get::<i64, _>(11).max(0)).unwrap_or(0),
            domain_count: usize::try_from(row.get::<i64, _>(12).max(0)).unwrap_or(0),
            tool_call_count: usize::try_from(row.get::<i64, _>(13).max(0)).unwrap_or(0),
            quality_score: row.get::<Option<f64>, _>(14).map(|v| v.clamp(0.0, 1.0)),
            error_code: row.get::<Option<String>, _>(15),
            error_message: row.get::<Option<String>, _>(16),
            detail: row.get::<Option<Value>, _>(17),
            started_at: row.get::<Option<String>, _>(18),
            ended_at: row.get::<Option<String>, _>(19),
            updated_at: row.get::<Option<String>, _>(20),
        });
    }
    out
}

pub async fn upsert_pm_provider_health(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    provider_key: &str,
    channel: &str,
    success: bool,
    latency_ms: Option<u64>,
    error_code: Option<&str>,
) {
    let success_inc = if success { 1i64 } else { 0i64 };
    let failure_inc = if success { 0i64 } else { 1i64 };
    let status = if success { "healthy" } else { "degraded" };
    let _ = sqlx::query(
        "INSERT INTO pm_provider_health
            (tenant_id, provider_key, channel, run_count, success_count, failure_count,
             avg_latency_ms, last_error_code, last_status, last_checked_at)
         VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            run_count = run_count + 1,
            success_count = success_count + excluded.success_count,
            failure_count = failure_count + excluded.failure_count,
            avg_latency_ms = CASE
              WHEN excluded.avg_latency_ms IS NULL THEN avg_latency_ms
              WHEN avg_latency_ms IS NULL THEN excluded.avg_latency_ms
              ELSE avg_latency_ms * 0.85 + excluded.avg_latency_ms * 0.15
            END,
            last_error_code = excluded.last_error_code,
            last_status = excluded.last_status,
            last_checked_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(provider_key)
    .bind(channel)
    .bind(success_inc)
    .bind(failure_inc)
    .bind(latency_ms.and_then(|x| i64::try_from(x).ok()))
    .bind(error_code)
    .bind(status)
    .execute(db)
    .await;
}

pub async fn record_pm_prompt_usage(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
    stage: &str,
    prompt_key: &str,
    prompt_version: &str,
    prompt_hash: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO pm_prompt_registry
            (tenant_id, prompt_key, prompt_version, prompt_hash, last_run_id, stage, run_count, last_used_at)
         VALUES (?, ?, ?, ?, ?, ?, 1, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            prompt_hash = excluded.prompt_hash,
            last_run_id = excluded.last_run_id,
            stage = excluded.stage,
            run_count = run_count + 1,
            last_used_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(prompt_key)
    .bind(prompt_version)
    .bind(prompt_hash)
    .bind(run_id)
    .bind(stage)
    .execute(db)
    .await;
}

pub async fn upsert_pm_route_learning_feature(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
    channel: Option<&str>,
    success: bool,
    quality: f64,
    latency_ms: f64,
    cost_usd: f64,
) {
    let success_inc = if success { 1f64 } else { 0f64 };
    let failure_inc = if success { 0f64 } else { 1f64 };
    let _ = sqlx::query(
        "INSERT INTO pm_route_learning_features
            (tenant_id, route_key, channel, total_runs, success_runs, failed_runs,
             ema_quality, ema_latency_ms, ema_cost_usd, ema_success_rate, last_run_at)
         VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            total_runs = total_runs + 1,
            success_runs = success_runs + excluded.success_runs,
            failed_runs = failed_runs + excluded.failed_runs,
            ema_quality = ema_quality * 0.80 + excluded.ema_quality * 0.20,
            ema_latency_ms = ema_latency_ms * 0.80 + excluded.ema_latency_ms * 0.20,
            ema_cost_usd = ema_cost_usd * 0.80 + excluded.ema_cost_usd * 0.20,
            ema_success_rate = (success_runs + excluded.success_runs) / MAX(total_runs + 1, 1),
            last_run_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(route_key)
    .bind(channel)
    .bind(success_inc)
    .bind(failure_inc)
    .bind(quality.clamp(0.0, 1.0))
    .bind(latency_ms.max(0.0))
    .bind(cost_usd.max(0.0))
    .bind(if success { 1.0 } else { 0.0 })
    .execute(db)
    .await;
}

pub async fn upsert_pm_route_bandit_state(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
    channel: Option<&str>,
    score: f64,
    exploration_bonus: f64,
) {
    let _ = sqlx::query(
        "INSERT INTO pm_route_bandit_state
            (tenant_id, route_key, channel, score, exploration_bonus, exploitation_score, last_decision_at)
         VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            score = excluded.score,
            exploration_bonus = excluded.exploration_bonus,
            exploitation_score = excluded.exploitation_score,
            last_decision_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(route_key)
    .bind(channel)
    .bind(score)
    .bind(exploration_bonus)
    .bind((score - exploration_bonus).max(0.0))
    .execute(db)
    .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_pm_quality_gate_metrics(
    db: &sqlx::SqlitePool,
    run_id: &str,
    tenant_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
    passed: bool,
    quality_score: f64,
    tool_call_count: usize,
    citation_count: usize,
    domain_count: usize,
    claim_count: usize,
    claim_alignment_ok: bool,
    triad_total_claims: usize,
    triad_aligned_claims: usize,
    triad_coverage: f64,
    conflict_adjudicated: bool,
    conflict_confidence: f64,
    missing: Option<&Value>,
    suggestions: Option<&Value>,
) {
    let missing_raw = missing.map(Value::to_string);
    let suggestions_raw = suggestions.map(Value::to_string);
    let _ = sqlx::query(
        "INSERT INTO pm_quality_gate_metrics
            (run_id, tenant_id, task_id, session_id, passed, quality_score,
             tool_call_count, citation_count, domain_count, claim_count, claim_alignment_ok,
             triad_total_claims, triad_aligned_claims, triad_coverage, conflict_adjudicated,
             conflict_confidence, missing_json, suggestions_json)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            tenant_id = excluded.tenant_id,
            task_id = excluded.task_id,
            session_id = excluded.session_id,
            passed = excluded.passed,
            quality_score = excluded.quality_score,
            tool_call_count = excluded.tool_call_count,
            citation_count = excluded.citation_count,
            domain_count = excluded.domain_count,
            claim_count = excluded.claim_count,
            claim_alignment_ok = excluded.claim_alignment_ok,
            triad_total_claims = excluded.triad_total_claims,
            triad_aligned_claims = excluded.triad_aligned_claims,
            triad_coverage = excluded.triad_coverage,
            conflict_adjudicated = excluded.conflict_adjudicated,
            conflict_confidence = excluded.conflict_confidence,
            missing_json = excluded.missing_json,
            suggestions_json = excluded.suggestions_json,
            created_at = COALESCE(created_at, CURRENT_TIMESTAMP)",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(task_id)
    .bind(session_id)
    .bind(if passed { 1i64 } else { 0i64 })
    .bind(quality_score.clamp(0.0, 1.0))
    .bind(i64::try_from(tool_call_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(citation_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(domain_count).unwrap_or(i64::MAX))
    .bind(i64::try_from(claim_count).unwrap_or(i64::MAX))
    .bind(if claim_alignment_ok { 1i64 } else { 0i64 })
    .bind(i64::try_from(triad_total_claims).unwrap_or(i64::MAX))
    .bind(i64::try_from(triad_aligned_claims).unwrap_or(i64::MAX))
    .bind(triad_coverage.clamp(0.0, 1.0))
    .bind(if conflict_adjudicated { 1i64 } else { 0i64 })
    .bind(conflict_confidence.clamp(0.0, 1.0))
    .bind(missing_raw)
    .bind(suggestions_raw)
    .execute(db)
    .await;
}

pub async fn get_pm_budget_profile(db: &sqlx::SqlitePool, tenant_id: &str) -> Option<String> {
    let row = sqlx::query(
        "SELECT profile_key
         FROM pm_budget_profiles
         WHERE tenant_id = ? AND enabled = 1
         ORDER BY is_default DESC, priority DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()??;

    Some(row.get::<String, _>(0))
}

pub async fn get_pm_budget_profile_config(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
) -> Option<PmBudgetProfileConfigRow> {
    let row = sqlx::query(
        "SELECT profile_key,
                pipeline_timeout_secs,
                max_attempts,
                retrieve_max_tool_calls,
                max_calls_per_source,
                source_slot_search_secs,
                source_slot_browser_secs,
                source_slot_api_fetch_secs,
                preflight_model_timeout_secs,
                preflight_probe_timeout_secs,
                preflight_overall_timeout_secs,
                retry_step_budget_secs,
                retry_total_budget_secs
         FROM pm_budget_profiles
         WHERE tenant_id = ? AND enabled = 1
         ORDER BY is_default DESC, priority DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()??;

    let to_u64 = |value: i32| -> u64 { u64::try_from(value.max(0)).unwrap_or(0) };
    let to_usize = |value: i32| -> usize { usize::try_from(value.max(0)).unwrap_or(0) };

    Some(PmBudgetProfileConfigRow {
        profile_key: row.get::<String, _>(0),
        pipeline_timeout_secs: to_u64(row.get::<i32, _>(1)),
        max_attempts: to_usize(row.get::<i32, _>(2)),
        retrieve_max_tool_calls: to_usize(row.get::<i32, _>(3)),
        max_calls_per_source: to_usize(row.get::<i32, _>(4)),
        source_slot_search_secs: to_u64(row.get::<i32, _>(5)),
        source_slot_browser_secs: to_u64(row.get::<i32, _>(6)),
        source_slot_api_fetch_secs: to_u64(row.get::<i32, _>(7)),
        preflight_model_timeout_secs: to_u64(row.get::<i32, _>(8)),
        preflight_probe_timeout_secs: to_u64(row.get::<i32, _>(9)),
        preflight_overall_timeout_secs: to_u64(row.get::<i32, _>(10)),
        retry_step_budget_secs: to_u64(row.get::<i32, _>(11)),
        retry_total_budget_secs: to_u64(row.get::<i32, _>(12)),
    })
}

pub async fn record_pm_audit_event(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    event_type: &str,
    severity: &str,
    message: &str,
    payload: Option<&Value>,
) {
    let payload_raw = protected_persistence_json(payload);
    let _ = sqlx::query(
        "INSERT INTO pm_audit_trails
            (tenant_id, user_id, run_id, event_type, severity, message, payload_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(run_id)
    .bind(event_type)
    .bind(severity)
    .bind(protected_persistence_text(message))
    .bind(payload_raw)
    .execute(db)
    .await;
}

#[derive(Debug, Clone)]
pub struct PmRouteCircuitStateRow {
    pub consecutive_failures: u32,
    pub remaining_open_secs: u64,
}

#[derive(Debug, Clone)]
pub struct PmDomainCircuitStateRow {
    pub consecutive_failures: u32,
    pub remaining_open_secs: u64,
}

pub async fn load_pm_route_circuit_state(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
) -> Option<PmRouteCircuitStateRow> {
    let row = sqlx::query(
        "SELECT
            CAST(consecutive_failures AS INTEGER),
            CAST(MAX(CAST((julianday(open_until) - julianday(CURRENT_TIMESTAMP)) * 86400 AS INTEGER), 0) AS INTEGER)
         FROM pm_route_circuit_states
         WHERE tenant_id = ? AND route_key = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(route_key)
    .fetch_optional(db)
    .await
    .ok()??;

    let failures = row.get::<Option<i64>, _>(0).unwrap_or(0).max(0);
    let remaining = row.get::<Option<i64>, _>(1).unwrap_or(0).max(0);
    Some(PmRouteCircuitStateRow {
        consecutive_failures: u32::try_from(failures).unwrap_or(u32::MAX),
        remaining_open_secs: u64::try_from(remaining).unwrap_or(u64::MAX),
    })
}

pub async fn load_pm_domain_circuit_state(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    domain_key: &str,
) -> Option<PmDomainCircuitStateRow> {
    let row = sqlx::query(
        "SELECT
            CAST(consecutive_failures AS INTEGER),
            CAST(MAX(CAST((julianday(open_until) - julianday(CURRENT_TIMESTAMP)) * 86400 AS INTEGER), 0) AS INTEGER)
         FROM pm_domain_circuit_states
         WHERE tenant_id = ? AND domain_key = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(domain_key)
    .fetch_optional(db)
    .await
    .ok()??;

    let failures = row.get::<Option<i64>, _>(0).unwrap_or(0).max(0);
    let remaining = row.get::<Option<i64>, _>(1).unwrap_or(0).max(0);
    Some(PmDomainCircuitStateRow {
        consecutive_failures: u32::try_from(failures).unwrap_or(u32::MAX),
        remaining_open_secs: u64::try_from(remaining).unwrap_or(u64::MAX),
    })
}

pub async fn load_open_pm_domain_circuit_keys(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    limit: usize,
) -> Vec<String> {
    let fetch_limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
    let rows = match sqlx::query(
        "SELECT domain_key
         FROM pm_domain_circuit_states
         WHERE tenant_id = ?
           AND open_until IS NOT NULL
           AND open_until > CURRENT_TIMESTAMP
         ORDER BY open_until ASC
         LIMIT ?",
    )
    .bind(tenant_id)
    .bind(fetch_limit)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    rows.into_iter()
        .filter_map(|row| row.get::<Option<String>, _>(0))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

pub async fn report_pm_route_circuit_failure(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
    channel: Option<&str>,
    failure_threshold: u32,
    cooldown_secs: u64,
    error_code: Option<&str>,
    error_message: Option<&str>,
) {
    let threshold = i64::from(failure_threshold.max(1));
    let cooldown = i64::try_from(cooldown_secs.max(1)).unwrap_or(i64::MAX);
    let _ = sqlx::query(
        "INSERT INTO pm_route_circuit_states
            (tenant_id, route_key, channel, consecutive_failures, open_until,
             last_error_code, last_error_message, last_failure_at)
         VALUES (
            ?, ?, ?, 1,
            CASE WHEN ? <= 1 THEN datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)) ELSE NULL END,
            ?, ?, CURRENT_TIMESTAMP
         )
         ON CONFLICT DO UPDATE SET
            channel = COALESCE(excluded.channel, channel),
            consecutive_failures = MIN(consecutive_failures + 1, 1000000),
            open_until = CASE
              WHEN MIN(consecutive_failures + 1, 1000000) >= ? THEN datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?))
              ELSE open_until
            END,
            last_error_code = excluded.last_error_code,
            last_error_message = excluded.last_error_message,
            last_failure_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(route_key)
    .bind(channel)
    .bind(threshold)
    .bind(cooldown)
    .bind(error_code)
    .bind(error_message)
    .bind(threshold)
    .bind(cooldown)
    .execute(db)
    .await;
}

pub async fn report_pm_route_circuit_success(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    route_key: &str,
    channel: Option<&str>,
) {
    let _ = sqlx::query(
        "INSERT INTO pm_route_circuit_states
            (tenant_id, route_key, channel, consecutive_failures, open_until, last_success_at)
         VALUES (?, ?, ?, 0, NULL, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            channel = COALESCE(excluded.channel, channel),
            consecutive_failures = 0,
            open_until = NULL,
            last_error_code = NULL,
            last_error_message = NULL,
            last_success_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(route_key)
    .bind(channel)
    .execute(db)
    .await;
}

pub async fn report_pm_domain_circuit_failure(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    domain_key: &str,
    failure_threshold: u32,
    cooldown_secs: u64,
    error_code: Option<&str>,
    error_message: Option<&str>,
) {
    let threshold = i64::from(failure_threshold.max(1));
    let cooldown = i64::try_from(cooldown_secs.max(1)).unwrap_or(i64::MAX);
    let _ = sqlx::query(
        "INSERT INTO pm_domain_circuit_states
            (tenant_id, domain_key, consecutive_failures, open_until,
             last_error_code, last_error_message, last_failure_at)
         VALUES (
            ?, ?, 1,
            CASE WHEN ? <= 1 THEN datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?)) ELSE NULL END,
            ?, ?, CURRENT_TIMESTAMP
         )
         ON CONFLICT DO UPDATE SET
            consecutive_failures = MIN(consecutive_failures + 1, 1000000),
            open_until = CASE
              WHEN MIN(consecutive_failures + 1, 1000000) >= ? THEN datetime(CURRENT_TIMESTAMP, printf('%+d seconds', ?))
              ELSE open_until
            END,
            last_error_code = excluded.last_error_code,
            last_error_message = excluded.last_error_message,
            last_failure_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(domain_key)
    .bind(threshold)
    .bind(cooldown)
    .bind(error_code)
    .bind(error_message)
    .bind(threshold)
    .bind(cooldown)
    .execute(db)
    .await;
}

pub async fn report_pm_domain_circuit_success(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    domain_key: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO pm_domain_circuit_states
            (tenant_id, domain_key, consecutive_failures, open_until, last_success_at)
         VALUES (?, ?, 0, NULL, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            consecutive_failures = 0,
            open_until = NULL,
            last_error_code = NULL,
            last_error_message = NULL,
            last_success_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(domain_key)
    .execute(db)
    .await;
}

pub async fn load_pm_retry_not_before_ms(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
) -> Option<i64> {
    let row = sqlx::query(
        "SELECT CAST(((julianday(next_allowed_at) - julianday('1970-01-01 00:00:00')) * 86400000000) / 1000 AS INTEGER)
         FROM pm_retry_governance_states
         WHERE tenant_id = ? AND run_id = ?
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(db)
    .await
    .ok()??;
    row.get::<Option<i64>, _>(0)
}

pub async fn upsert_pm_retry_not_before_ms(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
    session_id: Option<&str>,
    attempt: usize,
    base_backoff_ms: u64,
    jitter_ms: u64,
    not_before_epoch_ms: i64,
) {
    let attempt_i64 = i64::try_from(attempt).unwrap_or(i64::MAX);
    let base_i64 = i64::try_from(base_backoff_ms).unwrap_or(i64::MAX);
    let jitter_i64 = i64::try_from(jitter_ms).unwrap_or(i64::MAX);
    let _ = sqlx::query(
        "INSERT INTO pm_retry_governance_states
            (tenant_id, run_id, session_id, last_attempt, next_allowed_at, base_backoff_ms, jitter_ms)
         VALUES (
            ?, ?, ?, ?, datetime(? / 1000, 'unixepoch'), ?, ?
         )
         ON CONFLICT DO UPDATE SET
            session_id = COALESCE(excluded.session_id, session_id),
            last_attempt = MAX(last_attempt, excluded.last_attempt),
            next_allowed_at = MAX(
              COALESCE(pm_retry_governance_states.next_allowed_at, '1970-01-01 00:00:00'),
              COALESCE(excluded.next_allowed_at, '1970-01-01 00:00:00')
            ),
            base_backoff_ms = excluded.base_backoff_ms,
            jitter_ms = excluded.jitter_ms,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(session_id)
    .bind(attempt_i64)
    .bind(not_before_epoch_ms)
    .bind(base_i64)
    .bind(jitter_i64)
    .execute(db)
    .await;
}

#[cfg(test)]
mod tests {
    use super::{
        persist_pm_run_finish, protect_pm_ledger_value, protected_persistence_json,
        reserve_pm_resource_budget, settle_pm_resource_budget, PmRunConfigSnapshot,
        PmRunFinishPayload,
    };
    use sqlx::Row;

    async fn pm_finish_test_db() -> sqlx::SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        for statement in [
            "CREATE TABLE pm_research_runs (
                run_id TEXT PRIMARY KEY, status TEXT NOT NULL, current_stage TEXT,
                attempt INTEGER, total_elapsed_ms INTEGER, error_code TEXT,
                error_message TEXT, final_quality_score REAL, metadata_json TEXT,
                started_at TEXT, ended_at TEXT, updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
            "CREATE TABLE pm_research_stage_attempts (
                run_id TEXT NOT NULL, status TEXT NOT NULL, elapsed_ms INTEGER,
                started_at TEXT, ended_at TEXT, updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
            "CREATE TABLE pm_subtask_runs (
                run_id TEXT NOT NULL, status TEXT NOT NULL, error_code TEXT,
                error_message TEXT, ended_at TEXT, updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
            "CREATE TABLE pm_subtask_attempts (
                run_id TEXT NOT NULL, status TEXT NOT NULL, error_code TEXT,
                error_message TEXT, ended_at TEXT, updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
        ] {
            sqlx::query(statement)
                .execute(&db)
                .await
                .expect("create PM finish test table");
        }
        db
    }

    async fn pm_budget_test_db() -> sqlx::SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        for statement in [
            "CREATE TABLE resource_budget_accounts (
                tenant_id TEXT NOT NULL, owner_scope TEXT NOT NULL, dimension TEXT NOT NULL,
                available INTEGER NOT NULL, reserved INTEGER NOT NULL, committed INTEGER NOT NULL,
                PRIMARY KEY (tenant_id, owner_scope, dimension)
            )",
            "CREATE TABLE resource_budget_entries (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, owner_scope TEXT NOT NULL,
                reservation_id TEXT NOT NULL, dimension TEXT NOT NULL, amount INTEGER NOT NULL,
                state TEXT NOT NULL, created_at TEXT NOT NULL
            )",
            "CREATE TABLE pm_research_runs (
                run_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL,
                total_elapsed_ms INTEGER
            )",
            "CREATE TABLE pm_research_tool_call_ledger (
                run_id TEXT NOT NULL, call_seq INTEGER NOT NULL, tool_name TEXT NOT NULL
            )",
        ] {
            sqlx::query(statement)
                .execute(&db)
                .await
                .expect("create PM budget test table");
        }
        db
    }

    #[tokio::test]
    async fn terminal_pm_run_closes_every_unresolved_child_with_parent_reason() {
        for (parent_status, child_status, child_error_code) in [
            ("cancelled", "cancelled", "parent_cancelled"),
            ("failed", "failed", "parent_failed"),
            ("completed", "skipped", "parent_completed_without_execution"),
        ] {
            let db = pm_finish_test_db().await;
            sqlx::query(
                "INSERT INTO pm_research_runs (run_id, status, started_at)
                 VALUES ('run-1', 'running', CURRENT_TIMESTAMP)",
            )
            .execute(&db)
            .await
            .expect("insert parent run");
            sqlx::query(
                "INSERT INTO pm_research_stage_attempts (run_id, status, started_at)
                 VALUES ('run-1', 'running', CURRENT_TIMESTAMP)",
            )
            .execute(&db)
            .await
            .expect("insert stage attempt");
            for table in ["pm_subtask_runs", "pm_subtask_attempts"] {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO {table} (run_id, status, error_code, error_message)
                     VALUES ('run-1', 'queued', 'old_transient_error', 'existing detail')"
                )))
                .execute(&db)
                .await
                .expect("insert unresolved child");
            }

            persist_pm_run_finish(
                &db,
                "run-1",
                &PmRunFinishPayload {
                    status: parent_status.to_string(),
                    current_stage: Some("finish".to_string()),
                    attempt: Some(1),
                    total_elapsed_ms: Some(10),
                    error_code: None,
                    error_message: None,
                    final_quality_score: None,
                    metadata: None,
                },
            )
            .await;

            for table in ["pm_subtask_runs", "pm_subtask_attempts"] {
                let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SELECT status, error_code, error_message, ended_at FROM {table}
                     WHERE run_id = 'run-1'"
                )))
                .fetch_one(&db)
                .await
                .expect("load reconciled child");
                assert_eq!(row.get::<String, _>(0), child_status);
                assert_eq!(row.get::<String, _>(1), child_error_code);
                assert!(row
                    .get::<String, _>(2)
                    .contains("Parent PM run reached a terminal state"));
                assert!(row.get::<Option<String>, _>(3).is_some());
            }
        }
    }

    #[tokio::test]
    async fn pm_resource_budget_reserves_and_settles_without_oversell() {
        let db = pm_budget_test_db().await;
        let config = PmRunConfigSnapshot {
            budget_profile: "test".to_string(),
            pipeline_timeout_secs: 10,
            deadline_timeout_secs: 15,
            max_attempts: 2,
            source_slot_search_secs: 2,
            source_slot_browser_secs: 2,
            source_slot_api_fetch_secs: 2,
            retrieve_max_tool_calls: 5,
            max_calls_per_source: 2,
        };
        reserve_pm_resource_budget(&db, "run-1", "tenant", "user", &config)
            .await
            .expect("reserve PM budget");
        sqlx::query(
            "INSERT INTO pm_research_runs (run_id, tenant_id, user_id, total_elapsed_ms)
             VALUES ('run-1', 'tenant', 'user', 1200)",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO pm_research_tool_call_ledger (run_id, call_seq, tool_name)
             VALUES ('run-1', 1, 'WebSearch'), ('run-1', 2, 'REPL')",
        )
        .execute(&db)
        .await
        .unwrap();
        settle_pm_resource_budget(
            &db,
            "run-1",
            &PmRunFinishPayload {
                status: "completed".to_string(),
                current_stage: Some("done".to_string()),
                attempt: Some(1),
                total_elapsed_ms: Some(1_200),
                error_code: None,
                error_message: None,
                final_quality_score: Some(0.9),
                metadata: None,
            },
        )
        .await
        .expect("settle PM budget");

        let wall = sqlx::query(
            "SELECT available, reserved, committed FROM resource_budget_accounts
             WHERE tenant_id = 'tenant' AND dimension = 'wall_time_ms'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(wall.get::<i64, _>(0), 8_800);
        assert_eq!(wall.get::<i64, _>(1), 0);
        assert_eq!(wall.get::<i64, _>(2), 1_200);
        let tool_committed: i64 = sqlx::query_scalar(
            "SELECT committed FROM resource_budget_accounts
             WHERE tenant_id = 'tenant' AND dimension = 'tool_calls'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        let web_committed: i64 = sqlx::query_scalar(
            "SELECT committed FROM resource_budget_accounts
             WHERE tenant_id = 'tenant' AND dimension = 'web_queries'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(tool_committed, 2);
        assert_eq!(web_committed, 1);
    }

    #[test]
    fn pm_tool_ledger_never_persists_plaintext_credentials() {
        let mut value = Some(
            "api_key=sk-1234567890abcdef https://reader:db-secret@example.test/report".to_string(),
        );
        protect_pm_ledger_value(&mut value);
        let protected = value.expect("protected value");
        assert!(!protected.contains("sk-1234567890abcdef"));
        assert!(!protected.contains("db-secret"));
        assert!(protected.contains("[REDACTED"));
    }

    #[test]
    fn pm_audit_payload_never_persists_plaintext_credentials() {
        let payload = serde_json::json!({
            "url": "https://example.test/?token=query-secret-value",
            "authorization": "Bearer opaque-token-123456"
        });
        let protected = protected_persistence_json(Some(&payload)).expect("protected payload");
        assert!(!protected.contains("query-secret-value"));
        assert!(!protected.contains("opaque-token-123456"));
        assert!(protected.contains("[REDACTED"));
    }
}
