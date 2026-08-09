use super::*;

pub(super) async fn record_pm_strategy_outcome(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PmStrategyRecordRequest>,
) -> impl IntoResponse {
    let route = req.route.trim();
    if route.is_empty() {
        return AppError::ValidationError("route cannot be empty".to_string()).into_response();
    }
    let channel = req
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);
    let variant = req
        .variant
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);
    let citation = req.citation_count.unwrap_or(0.0).clamp(0.0, 1000.0);
    let domain = req.domain_count.unwrap_or(0.0).clamp(0.0, 1000.0);
    let tools = req.tool_call_count.unwrap_or(0.0).clamp(0.0, 1000.0);
    let retrieve_ms = req
        .retrieve_duration_ms
        .unwrap_or(0.0)
        .clamp(0.0, 3_600_000.0);
    let cost = req.estimated_cost_usd.unwrap_or(0.0).clamp(0.0, 1000.0);
    let quality = ((citation.min(8.0) / 8.0) * 0.4
        + (domain.min(4.0) / 4.0) * 0.3
        + (if tools > 0.0 { 1.0 } else { 0.0 }) * 0.3)
        .clamp(0.0, 1.0);
    let success_inc = if req.passed { 1i64 } else { 0i64 };
    let failure_inc = if req.passed { 0i64 } else { 1i64 };

    let upsert = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO pm_research_route_stats
            (tenant_id, route_key, channel, variant, run_count, success_count, failure_count,
             avg_citation_count, avg_domain_count, avg_tool_call_count, avg_retrieve_duration_ms,
             avg_cost_usd, success_rate, avg_quality, score, last_run_at)
         VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT DO UPDATE SET
            channel = COALESCE(excluded.channel, channel),
            variant = COALESCE(excluded.variant, variant),
            run_count = run_count + 1,
            success_count = success_count + excluded.success_count,
            failure_count = failure_count + excluded.failure_count,
            avg_citation_count = avg_citation_count * 0.8 + excluded.avg_citation_count * 0.2,
            avg_domain_count = avg_domain_count * 0.8 + excluded.avg_domain_count * 0.2,
            avg_tool_call_count = avg_tool_call_count * 0.8 + excluded.avg_tool_call_count * 0.2,
            avg_retrieve_duration_ms = avg_retrieve_duration_ms * 0.8 + excluded.avg_retrieve_duration_ms * 0.2,
            avg_cost_usd = avg_cost_usd * 0.8 + excluded.avg_cost_usd * 0.2,
            avg_quality = avg_quality * 0.8 + excluded.avg_quality * 0.2,
            success_rate = (success_count + excluded.success_count) / MAX(run_count + 1, 1),
            score = (
              ((success_count + excluded.success_count) / MAX(run_count + 1, 1)) * 0.55 +
              (avg_quality * 0.8 + excluded.avg_quality * 0.2) * 0.30 +
              (1 / (1 + (avg_cost_usd * 0.8 + excluded.avg_cost_usd * 0.2) * 10)) * 0.15
            ),
            last_run_at = CURRENT_TIMESTAMP,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&claims.tenant_id)
    .bind(route)
    .bind(channel.as_deref())
    .bind(variant.as_deref())
    .bind(success_inc)
    .bind(failure_inc)
    .bind(citation)
    .bind(domain)
    .bind(tools)
    .bind(retrieve_ms)
    .bind(cost)
    .bind(if req.passed { 1.0 } else { 0.0 })
    .bind(quality)
    .bind(if req.passed { 0.9 } else { 0.3 })
    .execute(&state.db)
    .await;

    match upsert {
        Ok(_) => {
            let total_runs = (success_inc + failure_inc).max(1) as f64;
            let exploration_bonus = (1.0 / (1.0 + total_runs)).clamp(0.01, 0.10);
            upsert_pm_route_learning_feature(
                &state.db,
                &claims.tenant_id,
                route,
                channel.as_deref(),
                req.passed,
                quality,
                retrieve_ms,
                cost,
            )
            .await;
            upsert_pm_route_bandit_state(
                &state.db,
                &claims.tenant_id,
                route,
                channel.as_deref(),
                (quality * 0.60 + if req.passed { 0.40 } else { 0.10 }).clamp(0.0, 1.0),
                exploration_bonus,
            )
            .await;
            record_pm_audit_event(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                "manual-strategy-record",
                "pm_strategy_outcome",
                "info",
                "route strategy outcome recorded",
                Some(&serde_json::json!({
                    "route": route,
                    "channel": channel,
                    "variant": variant,
                    "passed": req.passed,
                    "quality": quality
                })),
            )
            .await;
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(e) => AppError::Database(e).into_response(),
    }
}

pub(super) async fn list_pm_strategy_leaderboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> impl IntoResponse {
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT route_key, channel,
                CAST(run_count AS INTEGER), CAST(success_rate AS DOUBLE), CAST(avg_quality AS DOUBLE),
                CAST(avg_cost_usd AS DOUBLE), CAST(avg_retrieve_duration_ms AS DOUBLE), CAST(score AS DOUBLE),
                strftime('%Y-%m-%dT%H:%M:%SZ', last_run_at)
         FROM pm_research_route_stats
         WHERE tenant_id = ?
         ORDER BY score DESC, run_count DESC, updated_at DESC
         LIMIT 30",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(PmStrategyLeaderboardItem {
            route: row.get::<String, _>(0),
            channel: row.get::<Option<String>, _>(1),
            run_count: row.get::<i64, _>(2),
            success_rate: row.get::<f64, _>(3),
            avg_quality: row.get::<f64, _>(4),
            avg_cost: row.get::<f64, _>(5),
            avg_retrieve_duration_ms: row.get::<f64, _>(6),
            score: row.get::<f64, _>(7),
            last_run_at: row.get::<Option<String>, _>(8),
        });
    }

    Json(PmStrategyLeaderboardResponse { rows: out }).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmOpsListQuery {
    limit: Option<u32>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmOpsPageQuery {
    page: Option<u32>,
    #[serde(alias = "per_page")]
    per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmOpsWindowQuery {
    days: Option<u32>,
    limit: Option<u32>,
    page: Option<u32>,
    #[serde(alias = "per_page")]
    per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmRunTraceQuery {
    lite: Option<bool>,
    include_raw_io: Option<bool>,
    stage_limit: Option<u32>,
    source_slot_limit: Option<u32>,
    tool_call_limit: Option<u32>,
    subtask_limit: Option<u32>,
    subtask_attempt_limit: Option<u32>,
    task_event_limit: Option<u32>,
    audit_limit: Option<u32>,
    claim_limit: Option<u32>,
    conflict_limit: Option<u32>,
    repair_limit: Option<u32>,
    prompt_limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetPmBudgetProfileRequest {
    profile_key: String,
}

async fn require_pm_governance_write(state: &AppState, claims: &Claims) -> Result<(), AppError> {
    if matches!(claims.role.as_str(), "admin" | "superadmin") {
        return Ok(());
    }
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT CAST(menu_permissions_json AS TEXT) AS menu_permissions_json
         FROM users
         WHERE tenant_id = ? AND id = ? AND is_active = 1
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let Some(row) = row else {
        return Err(AppError::Forbidden);
    };
    let raw = row
        .try_get::<Option<String>, _>("menu_permissions_json")
        .ok()
        .flatten();
    let allowed = raw
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .map(|permissions| {
            permissions
                .iter()
                .any(|permission| permission == "operations_governance:write")
        })
        .unwrap_or(false);
    if allowed {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn pm_budget_profile_display_name(profile: PmBudgetProfile) -> &'static str {
    match profile {
        PmBudgetProfile::Normal => "Normal",
        PmBudgetProfile::UnstableRelay => "Unstable Relay",
        PmBudgetProfile::ProxyHeavy => "Proxy Heavy",
        PmBudgetProfile::DeepResearch => "Deep Research",
    }
}

async fn pm_fetch_latency_percentiles(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    days: i64,
) -> Result<(Option<i64>, Option<i64>, Option<i64>, i64), sqlx::Error> {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let count_row = sqlx::query::<sqlx::Sqlite>(
        "SELECT CAST(COUNT(*) AS INTEGER) AS latency_count
         FROM pm_research_runs
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND COALESCE(
                total_elapsed_ms,
                CASE
                    WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
                    THEN MAX(((julianday(ended_at) - julianday(started_at)) * 86400000000) / 1000, 0)
                    ELSE NULL
                END
           ) IS NOT NULL",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(db)
    .await?;
    let count = count_row.get::<i64, _>(0).max(0);
    if count == 0 {
        return Ok((None, None, None, 0));
    }

    let mut values: [Option<i64>; 3] = [None, None, None];
    let percentiles = [0.50_f64, 0.95_f64, 0.99_f64];
    let count_usize = usize::try_from(count).unwrap_or(usize::MAX);
    for (idx, percentile) in percentiles.iter().enumerate() {
        let rank = ((count_usize as f64 * percentile).ceil() as usize).saturating_sub(1);
        let row = sqlx::query::<sqlx::Sqlite>(
            "SELECT CAST(COALESCE(
                total_elapsed_ms,
                CASE
                    WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
                    THEN MAX(((julianday(ended_at) - julianday(started_at)) * 86400000000) / 1000, 0)
                    ELSE NULL
                END
             ) AS INTEGER) AS elapsed_ms
             FROM pm_research_runs
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
               AND started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
               AND COALESCE(
                    total_elapsed_ms,
                    CASE
                        WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
                        THEN MAX(((julianday(ended_at) - julianday(started_at)) * 86400000000) / 1000, 0)
                        ELSE NULL
                    END
               ) IS NOT NULL
             ORDER BY COALESCE(
                    total_elapsed_ms,
                    CASE
                        WHEN ended_at IS NOT NULL AND started_at IS NOT NULL
                        THEN MAX(((julianday(ended_at) - julianday(started_at)) * 86400000000) / 1000, 0)
                        ELSE NULL
                    END
             ) ASC
             LIMIT 1 OFFSET ?",
        )
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(days)
        .bind(i64::try_from(rank).unwrap_or(i64::MAX))
        .fetch_optional(db)
        .await?;
        values[idx] = row.map(|r| r.get::<i64, _>(0));
    }

    Ok((values[0], values[1], values[2], count))
}

fn pm_json_f64_field(payload: Option<&serde_json::Value>, keys: &[&str]) -> Option<f64> {
    let value = payload?;
    let obj = value.as_object()?;
    for key in keys {
        let Some(raw) = obj.get(*key) else {
            continue;
        };
        if let Some(v) = raw.as_f64() {
            return Some(v);
        }
        if let Some(v) = raw.as_i64() {
            return Some(v as f64);
        }
        if let Some(v) = raw.as_u64() {
            return Some(v as f64);
        }
    }
    None
}

fn pm_json_i64_field(payload: Option<&serde_json::Value>, keys: &[&str]) -> Option<i64> {
    let value = payload?;
    let obj = value.as_object()?;
    for key in keys {
        let Some(raw) = obj.get(*key) else {
            continue;
        };
        if let Some(v) = raw.as_i64() {
            return Some(v);
        }
        if let Some(v) = raw.as_u64() {
            return i64::try_from(v).ok();
        }
        if let Some(v) = raw.as_f64() {
            return Some(v.round() as i64);
        }
    }
    None
}

fn pm_ops_limit_i64(value: Option<u32>, default: u32, min: u32, max: u32) -> i64 {
    i64::from(value.unwrap_or(default).clamp(min, max))
}

fn pm_parse_json_opt(raw: Option<String>) -> Option<serde_json::Value> {
    raw.and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
}

fn pm_row_json_value(row: &sqlx::sqlite::SqliteRow, idx: usize) -> Option<serde_json::Value> {
    if let Ok(value) = row.try_get::<Option<sqlx::types::Json<serde_json::Value>>, _>(idx) {
        return value.map(|json| json.0);
    }
    if let Ok(value) = row.try_get::<Option<serde_json::Value>, _>(idx) {
        return value;
    }
    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return pm_parse_json_opt(value);
    }
    None
}

fn pm_is_unknown_column(error: &sqlx::Error, column_markers: &[&str]) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    (text.contains("unknown column") || text.contains("no such column"))
        && column_markers
            .iter()
            .any(|marker| text.contains(&marker.to_ascii_lowercase()))
}

fn pm_is_sort_memory_error(error: &sqlx::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("out of sort memory") || text.contains("1038")
}

pub(super) async fn list_pm_budget_profiles(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> impl IntoResponse {
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT profile_key, display_name, enabled, is_default, priority,
                pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
                source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
                preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
                retry_step_budget_secs, retry_total_budget_secs, constraints_json, updated_at
         FROM pm_budget_profiles
         WHERE tenant_id = ?
         ORDER BY is_default DESC, priority DESC, updated_at DESC",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            let constraints = pm_row_json_value(&row, 17);
            serde_json::json!({
                "profileKey": row.get::<String, _>(0),
                "displayName": row.get::<Option<String>, _>(1),
                "enabled": row.get::<i8, _>(2) == 1,
                "isDefault": row.get::<i8, _>(3) == 1,
                "priority": row.get::<i32, _>(4),
                "pipelineTimeoutSecs": row.get::<i32, _>(5),
                "maxAttempts": row.get::<i32, _>(6),
                "retrieveMaxToolCalls": row.get::<i32, _>(7),
                "maxCallsPerSource": row.get::<i32, _>(8),
                "sourceSlotSearchSecs": row.get::<i32, _>(9),
                "sourceSlotBrowserSecs": row.get::<i32, _>(10),
                "sourceSlotApiFetchSecs": row.get::<i32, _>(11),
                "preflightModelTimeoutSecs": row.get::<i32, _>(12),
                "preflightProbeTimeoutSecs": row.get::<i32, _>(13),
                "preflightOverallTimeoutSecs": row.get::<i32, _>(14),
                "retryStepBudgetSecs": row.get::<i32, _>(15),
                "retryTotalBudgetSecs": row.get::<i32, _>(16),
                "constraintsJson": constraints,
                "updatedAt": row.get::<chrono::NaiveDateTime, _>(18),
            })
        })
        .collect();

    Json(serde_json::json!({
        "rows": items,
        "total": items.len(),
    }))
    .into_response()
}

pub(super) async fn set_pm_budget_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SetPmBudgetProfileRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_pm_governance_write(&state, &claims).await {
        return e.into_response();
    }
    if req.profile_key.trim().is_empty() {
        return AppError::ValidationError("profile_key cannot be empty".to_string())
            .into_response();
    }
    let profile = PmBudgetProfile::from_str(req.profile_key.trim());
    let profile_key = profile.as_str().to_string();
    let display_name = pm_budget_profile_display_name(profile).to_string();
    let budget = PmTimeoutBudget::baseline_for_profile(profile);

    let mut tx = match state.db.begin().await {
        Ok(t) => t,
        Err(e) => return AppError::Database(e).into_response(),
    };

    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE pm_budget_profiles
         SET is_default = 0, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ?",
    )
    .bind(&claims.tenant_id)
    .execute(&mut *tx)
    .await
    {
        return AppError::Database(e).into_response();
    }

    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO pm_budget_profiles
            (tenant_id, profile_key, display_name, enabled, is_default, priority,
             pipeline_timeout_secs, max_attempts, retrieve_max_tool_calls, max_calls_per_source,
             source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
             preflight_model_timeout_secs, preflight_probe_timeout_secs, preflight_overall_timeout_secs,
             retry_step_budget_secs, retry_total_budget_secs, constraints_json)
         VALUES (?, ?, ?, 1, 1, 100,
                 ?, ?, ?, ?,
                 ?, ?, ?,
                 ?, ?, ?,
                 ?, ?, ?)
         ON CONFLICT DO UPDATE SET
            display_name = excluded.display_name,
            enabled = 1,
            is_default = 1,
            priority = excluded.priority,
            pipeline_timeout_secs = excluded.pipeline_timeout_secs,
            max_attempts = excluded.max_attempts,
            retrieve_max_tool_calls = excluded.retrieve_max_tool_calls,
            max_calls_per_source = excluded.max_calls_per_source,
            source_slot_search_secs = excluded.source_slot_search_secs,
            source_slot_browser_secs = excluded.source_slot_browser_secs,
            source_slot_api_fetch_secs = excluded.source_slot_api_fetch_secs,
            preflight_model_timeout_secs = excluded.preflight_model_timeout_secs,
            preflight_probe_timeout_secs = excluded.preflight_probe_timeout_secs,
            preflight_overall_timeout_secs = excluded.preflight_overall_timeout_secs,
            retry_step_budget_secs = excluded.retry_step_budget_secs,
            retry_total_budget_secs = excluded.retry_total_budget_secs,
            constraints_json = excluded.constraints_json,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&claims.tenant_id)
    .bind(&profile_key)
    .bind(display_name)
    .bind(i32::try_from(budget.pipeline_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.max_attempts).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retrieve_max_tool_calls).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.max_calls_per_source).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_search_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_browser_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.source_slot_api_fetch_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_model_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_probe_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.preflight_overall_timeout_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retry_step_budget_secs).unwrap_or(i32::MAX))
    .bind(i32::try_from(budget.retry_total_budget_secs).unwrap_or(i32::MAX))
    .bind(
        serde_json::json!({
            "source": "api.activate",
            "activatedBy": claims.sub,
            "activatedAt": chrono::Utc::now().to_rfc3339(),
        })
        .to_string(),
    )
    .execute(&mut *tx)
    .await
    {
        return AppError::Database(e).into_response();
    }

    if let Err(e) = tx.commit().await {
        return AppError::Database(e).into_response();
    }

    Json(serde_json::json!({
        "ok": true,
        "activeProfile": profile_key,
        "budgetSnapshot": {
            "pipelineTimeoutSecs": budget.pipeline_timeout_secs,
            "maxAttempts": budget.max_attempts,
            "retrieveMaxToolCalls": budget.retrieve_max_tool_calls,
            "maxCallsPerSource": budget.max_calls_per_source,
            "sourceSlotSearchSecs": budget.source_slot_search_secs,
            "sourceSlotBrowserSecs": budget.source_slot_browser_secs,
            "sourceSlotApiFetchSecs": budget.source_slot_api_fetch_secs,
            "retryStepBudgetSecs": budget.retry_step_budget_secs,
            "retryTotalBudgetSecs": budget.retry_total_budget_secs,
        }
    }))
    .into_response()
}

pub(super) async fn list_pm_slo_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsWindowQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let custom_days = query.days.map(|d| d.clamp(1, 365));
    let windows = if let Some(days) = custom_days {
        vec![days]
    } else {
        vec![7, 30]
    };
    let mut rows = Vec::with_capacity(windows.len());
    for days in windows {
        let summary = match sqlx::query::<sqlx::Sqlite>(
            "SELECT
                CAST(COUNT(*) AS INTEGER) AS total_runs,
                CAST(COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS completed_runs,
                CAST(COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS failed_runs,
                CAST(COALESCE(SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END), 0) AS INTEGER) AS cancelled_runs,
                CAST(COALESCE(SUM(CASE WHEN final_quality_score IS NOT NULL THEN 1 ELSE 0 END), 0) AS INTEGER) AS quality_sample_runs,
                CAST(COALESCE(SUM(CASE WHEN final_quality_score IS NOT NULL AND final_quality_score >= 0.60 THEN 1 ELSE 0 END), 0) AS INTEGER) AS quality_pass_runs,
                CAST(COALESCE(SUM(CASE WHEN status IN ('completed','failed','cancelled','interrupted') THEN 1 ELSE 0 END), 0) AS INTEGER) AS terminal_runs,
                CAST(COALESCE(SUM(CASE
                    WHEN status = 'completed'
                     AND COALESCE(CAST(JSON_EXTRACT(metadata_json, '$.answerTextPresent') AS INTEGER), 0) = 1
                    THEN 1 ELSE 0 END), 0) AS INTEGER) AS answer_delivery_runs
             FROM pm_research_runs
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
               AND started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))",
        )
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days))
        .fetch_one(&state.db)
        .await
        {
            Ok(v) => v,
            Err(e) => return AppError::Database(e).into_response(),
        };
        let total_runs = summary.get::<i64, _>(0).max(0);
        let completed_runs = summary.get::<i64, _>(1).max(0);
        let failed_runs = summary.get::<i64, _>(2).max(0);
        let cancelled_runs = summary.get::<i64, _>(3).max(0);
        let quality_sample_runs = summary.get::<i64, _>(4).max(0);
        let quality_pass_runs = summary.get::<i64, _>(5).max(0);
        let terminal_runs = summary.get::<i64, _>(6).max(0);
        let answer_delivery_runs = summary.get::<i64, _>(7).max(0);

        let (latency_p50, latency_p95, latency_p99, latency_sample_count) =
            match pm_fetch_latency_percentiles(&state.db, &claims, i64::from(days)).await {
                Ok(v) => v,
                Err(e) => return AppError::Database(e).into_response(),
            };

        let denom = total_runs.max(1) as f64;
        rows.push(serde_json::json!({
            "windowDays": days,
            "totalRuns": total_runs,
            "completedRuns": completed_runs,
            "failedRuns": failed_runs,
            "cancelledRuns": cancelled_runs,
            "successRate": (completed_runs as f64 / denom),
            "qualitySampleRuns": quality_sample_runs,
            "qualitySampleCoverage": (quality_sample_runs as f64 / denom),
            "qualityPassRate": (quality_pass_runs as f64 / quality_sample_runs.max(1) as f64),
            "terminalRate": (terminal_runs as f64 / denom),
            "answerDeliveryRate": (answer_delivery_runs as f64 / denom),
            "conclusionDeliveryRate": (answer_delivery_runs as f64 / denom),
            "latencySampleCount": latency_sample_count,
            "latencyP50Ms": latency_p50,
            "latencyP95Ms": latency_p95,
            "latencyP99Ms": latency_p99,
        }));
    }
    Json(serde_json::json!({
        "rows": rows,
        "generatedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

pub(super) async fn list_pm_failure_taxonomy(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsWindowQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let days = i64::from(query.days.unwrap_or(30).clamp(1, 365));
    let limit = i64::from(query.limit.unwrap_or(30).clamp(1, 200));
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT error_code_key,
                CAST(COUNT(*) AS INTEGER) AS object_count,
                CAST(COALESCE(SUM(CASE WHEN object_type = 'run' THEN 1 ELSE 0 END), 0) AS INTEGER) AS run_failure_count,
                CAST(COALESCE(SUM(CASE WHEN object_type = 'subtask' THEN 1 ELSE 0 END), 0) AS INTEGER) AS subtask_failure_count,
                CAST(COALESCE(SUM(CASE WHEN object_type = 'tool_call' THEN 1 ELSE 0 END), 0) AS INTEGER) AS tool_failure_count,
                CAST(AVG(elapsed_ms) AS INTEGER) AS avg_elapsed_ms,
                MAX(last_seen_at) AS last_seen_at
         FROM (
            SELECT COALESCE(NULLIF(error_code, ''), 'unknown') AS error_code_key,
                   'run' AS object_type, total_elapsed_ms AS elapsed_ms, updated_at AS last_seen_at
            FROM pm_research_runs
            WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
              AND COALESCE(started_at, created_at) >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
              AND (status = 'failed' OR (status = 'completed' AND COALESCE(error_code, '') <> ''))
            UNION ALL
            SELECT COALESCE(NULLIF(s.error_code, ''), 'unknown'),
                   'subtask',
                   CASE WHEN s.ended_at IS NOT NULL AND s.started_at IS NOT NULL
                        THEN MAX(CAST((julianday(s.ended_at) - julianday(s.started_at)) * 86400000 AS INTEGER), 0)
                        ELSE NULL END,
                   s.updated_at
            FROM pm_subtask_runs s
            INNER JOIN pm_research_runs r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id
            WHERE s.tenant_id = ? AND (? = 1 OR r.user_id = ?)
              AND s.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
              AND s.status = 'failed'
            UNION ALL
            SELECT COALESCE(NULLIF(l.error_code, ''), 'tool_error'),
                   'tool_call', l.latency_ms, l.created_at
            FROM pm_research_tool_call_ledger l
            INNER JOIN pm_research_runs r ON r.run_id = l.run_id
            WHERE r.tenant_id = ? AND (? = 1 OR r.user_id = ?)
              AND l.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
              AND l.is_error = 1
         ) failures
         GROUP BY error_code_key
         ORDER BY object_count DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "errorCode": row.get::<String, _>(0),
                "runCount": row.get::<i64, _>(1),
                "objectCount": row.get::<i64, _>(1),
                "failedCount": row.get::<i64, _>(1),
                "completedCount": 0,
                "runFailureCount": row.get::<i64, _>(2),
                "subtaskFailureCount": row.get::<i64, _>(3),
                "toolFailureCount": row.get::<i64, _>(4),
                "avgElapsedMs": row.get::<Option<i64>, _>(5),
                "lastSeenAt": row.get::<Option<chrono::NaiveDateTime>, _>(6),
            })
        })
        .collect();
    Json(serde_json::json!({
        "rows": items,
        "days": days,
        "total": items.len(),
    }))
    .into_response()
}

pub(super) async fn list_pm_quality_gate_summary(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsWindowQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let custom_days = query.days.map(|d| d.clamp(1, 365));
    let windows = if let Some(days) = custom_days {
        vec![days]
    } else {
        vec![7, 30]
    };
    let mut rows = Vec::with_capacity(windows.len());
    for days in windows {
        let row = match sqlx::query::<sqlx::Sqlite>(
            "SELECT
                CAST(COUNT(*) AS INTEGER) AS total_rows,
                CAST(COALESCE(SUM(CASE WHEN passed = 1 THEN 1 ELSE 0 END), 0) AS INTEGER) AS passed_rows,
                CAST(COALESCE(AVG(quality_score), 0) AS DOUBLE) AS avg_quality_score,
                CAST(COALESCE(AVG(triad_coverage), 0) AS DOUBLE) AS avg_triad_coverage,
                CAST(COALESCE(AVG(CASE WHEN claim_alignment_ok = 1 THEN 1 ELSE 0 END), 0) AS DOUBLE) AS claim_alignment_rate,
                CAST(COALESCE(AVG(CASE WHEN conflict_adjudicated = 1 THEN 1 ELSE 0 END), 0) AS DOUBLE) AS conflict_adjudicated_rate,
                CAST(COALESCE(AVG(tool_call_count), 0) AS DOUBLE) AS avg_tool_call_count,
                CAST(COALESCE(AVG(citation_count), 0) AS DOUBLE) AS avg_citation_count,
                CAST(COALESCE(AVG(domain_count), 0) AS DOUBLE) AS avg_domain_count
             FROM pm_quality_gate_metrics q
             INNER JOIN pm_research_runs r ON r.tenant_id = q.tenant_id AND r.run_id = q.run_id
             WHERE q.tenant_id = ? AND (? = 1 OR r.user_id = ?)
               AND q.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))",
        )
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(i64::from(days))
        .fetch_one(&state.db)
        .await
        {
            Ok(v) => v,
            Err(e) => return AppError::Database(e).into_response(),
        };
        let total_rows = row.get::<i64, _>(0).max(0);
        let passed_rows = row.get::<i64, _>(1).max(0);
        rows.push(serde_json::json!({
            "windowDays": days,
            "totalRuns": total_rows,
            "passedRuns": passed_rows,
            "passRate": (passed_rows as f64 / total_rows.max(1) as f64),
            "avgQualityScore": row.get::<f64, _>(2),
            "avgTriadCoverage": row.get::<f64, _>(3),
            "claimAlignmentRate": row.get::<f64, _>(4),
            "conflictAdjudicatedRate": row.get::<f64, _>(5),
            "avgToolCallCount": row.get::<f64, _>(6),
            "avgCitationCount": row.get::<f64, _>(7),
            "avgDomainCount": row.get::<f64, _>(8),
        }));
    }
    Json(serde_json::json!({
        "rows": rows,
        "generatedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

pub(super) async fn list_pm_knowledge_coverage_warnings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsWindowQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let days = i64::from(query.days.unwrap_or(30).clamp(1, 365));
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.or(query.limit).unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from(page.saturating_sub(1).saturating_mul(per_page));
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT audit.run_id, audit.payload_json, audit.message, audit.created_at,
                (
                  SELECT task.id FROM agent_tasks task
                  WHERE task.tenant_id = audit.tenant_id
                    AND (task.source_ref = audit.run_id OR task.linked_resource_id = audit.run_id)
                  ORDER BY task.created_at DESC LIMIT 1
                ) AS task_id
         FROM pm_audit_trails audit
         WHERE audit.tenant_id = ? AND (? = 1 OR audit.user_id = ?)
           AND audit.event_type = 'pm_knowledge_coverage_warning'
           AND audit.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
         ORDER BY audit.id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let summary_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COUNT(*) AS INTEGER) AS warning_count,
            CAST(COALESCE(AVG(MAX(0.0, MIN(1.0, COALESCE(
                CAST(JSON_EXTRACT(payload_json, '$.knowledgeCoverageRatio') AS DECIMAL(18,6)),
                0.0
            )))), 0.0) AS DOUBLE) AS avg_coverage_ratio,
            CAST(COALESCE(MIN(MAX(0.0, MIN(1.0, COALESCE(
                CAST(JSON_EXTRACT(payload_json, '$.knowledgeCoverageRatio') AS DECIMAL(18,6)),
                0.0
            )))), 0.0) AS DOUBLE) AS min_coverage_ratio,
            CAST(COALESCE(MAX(MAX(COALESCE(
                CAST(JSON_EXTRACT(payload_json, '$.queuedSubtaskEstimate') AS INTEGER),
                0
            ), 0)), 0) AS INTEGER) AS max_queued_subtasks,
            CAST(COALESCE(AVG(MAX(COALESCE(
                CAST(JSON_EXTRACT(payload_json, '$.subtaskGapCount') AS INTEGER),
                0
            ), 0)), 0.0) AS DOUBLE) AS avg_subtask_gap_count,
            CAST(COALESCE(AVG(MAX(COALESCE(
                CAST(JSON_EXTRACT(payload_json, '$.dimensionGapCount') AS INTEGER),
                0
            ), 0)), 0.0) AS DOUBLE) AS avg_dimension_gap_count
         FROM pm_audit_trails
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND event_type = 'pm_knowledge_coverage_warning'
           AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let warning_count = summary_row.get::<i64, _>(0).max(0);
    let avg_coverage_ratio = summary_row.get::<f64, _>(1).clamp(0.0, 1.0);
    let min_coverage_ratio = summary_row.get::<f64, _>(2).clamp(0.0, 1.0);
    let max_queued_subtasks = summary_row.get::<i64, _>(3).max(0);
    let avg_subtask_gap_count = summary_row.get::<f64, _>(4).max(0.0);
    let avg_dimension_gap_count = summary_row.get::<f64, _>(5).max(0.0);

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let payload = pm_row_json_value(&row, 1);
        let coverage_ratio = pm_json_f64_field(payload.as_ref(), &["knowledgeCoverageRatio"])
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let planned_subtasks =
            pm_json_i64_field(payload.as_ref(), &["plannedSubtaskCount"]).unwrap_or(0);
        let executed_subtasks =
            pm_json_i64_field(payload.as_ref(), &["executedSubtaskCount"]).unwrap_or(0);
        let queued_subtasks =
            pm_json_i64_field(payload.as_ref(), &["queuedSubtaskEstimate"]).unwrap_or(0);
        let subtask_gap_count =
            pm_json_i64_field(payload.as_ref(), &["subtaskGapCount"]).unwrap_or(0);
        let dimension_gap_count =
            pm_json_i64_field(payload.as_ref(), &["dimensionGapCount"]).unwrap_or(0);
        items.push(serde_json::json!({
            "runId": row.get::<Option<String>, _>(0),
            "coverageRatio": coverage_ratio,
            "plannedSubtasks": planned_subtasks,
            "executedSubtasks": executed_subtasks,
            "queuedSubtasks": queued_subtasks,
            "subtaskGapCount": subtask_gap_count,
            "dimensionGapCount": dimension_gap_count,
            "message": row.get::<Option<String>, _>(2),
            "createdAt": row.get::<chrono::NaiveDateTime, _>(3),
            "taskId": row.get::<Option<String>, _>(4),
            "payload": payload,
        }));
    }
    Json(serde_json::json!({
        "days": days,
        "rows": items,
        "total": warning_count,
        "page": page,
        "perPage": per_page,
        "summary": {
            "warningCount": warning_count,
            "avgCoverageRatio": avg_coverage_ratio,
            "minCoverageRatio": min_coverage_ratio,
            "maxQueuedSubtasks": max_queued_subtasks,
            "avgSubtaskGapCount": avg_subtask_gap_count,
            "avgDimensionGapCount": avg_dimension_gap_count,
        }
    }))
    .into_response()
}

pub(super) async fn list_pm_runtime_insights(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsWindowQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let days = i64::from(query.days.unwrap_or(30).clamp(1, 365));

    let run_summary_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COUNT(*) AS INTEGER) AS total_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END), 0) AS INTEGER) AS queued_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS INTEGER) AS running_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS completed_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS failed_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END), 0) AS INTEGER) AS cancelled_runs,
            CAST(COALESCE(SUM(CASE WHEN COALESCE(attempt, 1) > 1 THEN 1 ELSE 0 END), 0) AS INTEGER) AS retried_runs,
            CAST(COALESCE(SUM(CASE WHEN COALESCE(attempt, 1) > 1 AND status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS recovered_runs,
            CAST(COALESCE(SUM(CASE WHEN COALESCE(attempt, 1) <= 1 AND status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS first_pass_completed_runs
         FROM pm_research_runs
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND COALESCE(started_at, created_at) >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let task_backlog_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COALESCE(SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END), 0) AS INTEGER) AS queued_tasks,
            CAST(COALESCE(SUM(CASE WHEN status IN ('running','cancelling','interrupted') THEN 1 ELSE 0 END), 0) AS INTEGER) AS running_tasks
         FROM pm_research_tasks
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let subtask_backlog_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COALESCE(SUM(CASE WHEN s.status = 'queued' THEN 1 ELSE 0 END), 0) AS INTEGER) AS queued_subtasks,
            CAST(COALESCE(SUM(CASE WHEN s.status = 'running' THEN 1 ELSE 0 END), 0) AS INTEGER) AS running_subtasks
         FROM pm_subtask_runs s
         INNER JOIN pm_research_runs r
            ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id
         WHERE s.tenant_id = ? AND (? = 1 OR r.user_id = ?)
           AND r.status IN ('queued','running')",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let run_backlog_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COALESCE(SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END), 0) AS INTEGER),
            CAST(COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0) AS INTEGER)
         FROM pm_research_runs WHERE tenant_id = ? AND (? = 1 OR user_id = ?)",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let source_quota_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COUNT(*) AS INTEGER) AS retry_repair_attempts,
            CAST(COALESCE(SUM(CASE
                WHEN sa.error_code = 'source_quota_exhausted'
                  OR JSON_EXTRACT(sa.detail_json, '$.error') = 'source_quota_exhausted'
                  OR JSON_EXTRACT(sa.detail_json, '$.reason') LIKE 'source_quota_exhausted%'
                THEN 1 ELSE 0
            END), 0) AS INTEGER) AS source_quota_exhausted_attempts
         FROM pm_research_stage_attempts sa
         INNER JOIN pm_research_runs r ON r.run_id = sa.run_id
         WHERE r.tenant_id = ? AND (? = 1 OR r.user_id = ?)
           AND r.started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND sa.stage = 'retry_repair'",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let daily_run_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            strftime('%Y-%m-%d', COALESCE(started_at, created_at)) AS run_date,
            CAST(COUNT(*) AS INTEGER) AS total_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS completed_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS INTEGER) AS failed_runs,
            CAST(COALESCE(SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END), 0) AS INTEGER) AS cancelled_runs,
            CAST(COALESCE(SUM(CASE WHEN COALESCE(attempt, 1) > 1 THEN 1 ELSE 0 END), 0) AS INTEGER) AS retried_runs
         FROM pm_research_runs
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND COALESCE(started_at, created_at) >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
         GROUP BY run_date
         ORDER BY run_date",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let daily_runs: Vec<serde_json::Value> = daily_run_rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "date": row.get::<String, _>(0),
                "totalRuns": row.get::<i64, _>(1),
                "completedRuns": row.get::<i64, _>(2),
                "failedRuns": row.get::<i64, _>(3),
                "cancelledRuns": row.get::<i64, _>(4),
                "retriedRuns": row.get::<i64, _>(5),
            })
        })
        .collect();

    let daily_quota_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            strftime('%Y-%m-%d', sa.created_at) AS run_date,
            CAST(COUNT(*) AS INTEGER) AS retry_repair_attempts,
            CAST(COALESCE(SUM(CASE
                WHEN sa.error_code = 'source_quota_exhausted'
                  OR JSON_EXTRACT(sa.detail_json, '$.error') = 'source_quota_exhausted'
                  OR JSON_EXTRACT(sa.detail_json, '$.reason') LIKE 'source_quota_exhausted%'
                THEN 1 ELSE 0
            END), 0) AS INTEGER) AS source_quota_exhausted_attempts
         FROM pm_research_stage_attempts sa
         INNER JOIN pm_research_runs r ON r.run_id = sa.run_id
         WHERE r.tenant_id = ? AND (? = 1 OR r.user_id = ?)
           AND r.started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND sa.stage = 'retry_repair'
         GROUP BY run_date
         ORDER BY run_date",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let daily_source_quota: Vec<serde_json::Value> = daily_quota_rows
        .into_iter()
        .map(|row| {
            let attempts = row.get::<i64, _>(1).max(0);
            let exhausted = row.get::<i64, _>(2).max(0);
            serde_json::json!({
                "date": row.get::<String, _>(0),
                "retryRepairAttempts": attempts,
                "sourceQuotaExhaustedAttempts": exhausted,
                "sourceQuotaExhaustedRate": if attempts > 0 { exhausted as f64 / attempts as f64 } else { 0.0 },
            })
        })
        .collect();

    let queue_health_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COALESCE(SUM(CASE WHEN queued_age_secs IS NOT NULL THEN 1 ELSE 0 END), 0) AS INTEGER) AS queued_count,
            CAST(COALESCE(SUM(CASE WHEN running_age_secs IS NOT NULL THEN 1 ELSE 0 END), 0) AS INTEGER) AS running_count,
            CAST(MAX(queued_age_secs) AS INTEGER) AS oldest_queued_task_age_secs,
            CAST(MAX(running_age_secs) AS INTEGER) AS longest_running_task_age_secs,
            CAST(COALESCE(SUM(stale_running), 0) AS INTEGER) AS stale_running_tasks,
            CAST(AVG(queue_wait_secs) AS INTEGER) AS avg_queue_wait_secs,
            CAST(MAX(running_heartbeat_age_secs) AS INTEGER) AS longest_running_heartbeat_age_secs
         FROM (
            SELECT
                CASE
                    WHEN status = 'queued'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS queued_age_secs,
                CASE
                    WHEN status IN ('running','cancelling','interrupted')
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_age_secs,
                CASE
                    WHEN status IN ('running','cancelling','interrupted')
                      AND COALESCE(heartbeat_at, updated_at, created_at) < datetime(CURRENT_TIMESTAMP, '-10 minutes')
                    THEN 1 ELSE 0
                END AS stale_running,
                NULL AS queue_wait_secs,
                CASE
                    WHEN status IN ('running','cancelling','interrupted')
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(heartbeat_at, updated_at, created_at))) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_heartbeat_age_secs
            FROM pm_research_tasks
            WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
              AND completed_at IS NULL
              AND status IN ('queued','running','cancelling','interrupted')
            UNION ALL
            SELECT
                CASE
                    WHEN status = 'queued'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS queued_age_secs,
                CASE
                    WHEN status = 'running'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(started_at, created_at))) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_age_secs,
                CASE
                    WHEN status = 'running'
                      AND COALESCE(updated_at, started_at, created_at) < datetime(CURRENT_TIMESTAMP, '-10 minutes')
                    THEN 1 ELSE 0
                END AS stale_running,
                CASE
                    WHEN started_at IS NOT NULL
                      AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
                    THEN MAX(CAST((julianday(started_at) - julianday(created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS queue_wait_secs,
                CASE
                    WHEN status = 'running'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(updated_at, started_at, created_at))) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_heartbeat_age_secs
            FROM pm_research_runs
            WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
              AND (
                status IN ('queued','running')
                OR (started_at IS NOT NULL AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)))
              )
            UNION ALL
            SELECT
                CASE
                    WHEN s.status = 'queued'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(s.created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS queued_age_secs,
                CASE
                    WHEN s.status = 'running'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(s.started_at, s.created_at))) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_age_secs,
                CASE
                    WHEN s.status = 'running'
                      AND COALESCE(s.updated_at, s.started_at, s.created_at) < datetime(CURRENT_TIMESTAMP, '-10 minutes')
                    THEN 1 ELSE 0
                END AS stale_running,
                CASE
                    WHEN s.started_at IS NOT NULL
                      AND s.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
                    THEN MAX(CAST((julianday(s.started_at) - julianday(s.created_at)) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS queue_wait_secs,
                CASE
                    WHEN s.status = 'running'
                    THEN MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(COALESCE(s.updated_at, s.started_at, s.created_at))) * 86400 AS INTEGER), 0)
                    ELSE NULL
                END AS running_heartbeat_age_secs
            FROM pm_subtask_runs s
            INNER JOIN pm_research_runs r
              ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id
            WHERE s.tenant_id = ? AND (? = 1 OR r.user_id = ?)
              AND r.status IN ('queued','running')
              AND (
                s.status IN ('queued','running')
                OR (s.started_at IS NOT NULL AND s.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)))
              )
         ) q",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(days)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let oldest_queued_object = match sqlx::query::<sqlx::Sqlite>(
        "SELECT object_type, object_id, title, created_at,
                MAX(CAST((julianday(CURRENT_TIMESTAMP) - julianday(created_at)) * 86400 AS INTEGER), 0) AS age_secs
         FROM (
            SELECT 'task' AS object_type, task_id AS object_id, message AS title, created_at
            FROM pm_research_tasks WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND status = 'queued' AND completed_at IS NULL
            UNION ALL
            SELECT 'run' AS object_type, run_id AS object_id, COALESCE(NULLIF(user_message, ''), run_id) AS title, created_at
            FROM pm_research_runs WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND status = 'queued'
            UNION ALL
            SELECT 'subtask' AS object_type, CAST(s.id AS TEXT) AS object_id, s.title, s.created_at
            FROM pm_subtask_runs s
            INNER JOIN pm_research_runs r ON r.run_id = s.run_id AND r.tenant_id = s.tenant_id
            WHERE s.tenant_id = ? AND (? = 1 OR r.user_id = ?) AND s.status = 'queued' AND r.status IN ('queued','running')
         ) queued
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await
    {
        Ok(row) => row.map(|row| serde_json::json!({
            "objectType": row.get::<String, _>(0),
            "objectId": row.get::<String, _>(1),
            "title": row.get::<String, _>(2),
            "createdAt": row.get::<String, _>(3),
            "ageSecs": row.get::<i64, _>(4).max(0),
        })),
        Err(e) => return AppError::Database(e).into_response(),
    };

    let cost_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COALESCE(COUNT(*), 0) AS INTEGER) AS usage_record_count,
            CAST(COALESCE(SUM(CASE WHEN pricing_source IN ('built_in','custom') THEN 1 ELSE 0 END), 0) AS INTEGER) AS priced_record_count,
            CAST(COALESCE(SUM(CASE WHEN pricing_source = 'unknown' THEN 1 ELSE 0 END), 0) AS INTEGER) AS unpriced_record_count,
            CAST(COALESCE(SUM(total_tokens), 0) AS INTEGER) AS total_tokens,
            CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER) AS input_tokens,
            CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER) AS output_tokens,
            CAST(COALESCE(SUM(CASE WHEN pricing_source IN ('built_in','custom') THEN estimated_cost_usd ELSE 0 END), 0) AS DOUBLE) AS estimated_cost_usd
         FROM token_usage tu
         WHERE tu.tenant_id = ? AND (? = 1 OR tu.user_id = ?)
           AND tu.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND tu.usage_kind = 'request_delta'
           AND EXISTS (
             SELECT 1
             FROM pm_research_runs r
             WHERE r.tenant_id = tu.tenant_id
               AND r.session_id = tu.session_id
               AND r.status IN ('completed','failed','cancelled')
               AND r.started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
               AND tu.created_at >= r.started_at
               AND tu.created_at <= COALESCE(r.ended_at, CURRENT_TIMESTAMP)
           )",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let cost_by_model_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            model,
            provider, pricing_source,
            CAST(COUNT(*) AS INTEGER) AS request_count,
            CAST(COALESCE(SUM(total_tokens), 0) AS INTEGER) AS total_tokens,
            CAST(COALESCE(SUM(estimated_cost_usd), 0) AS DOUBLE) AS estimated_cost_usd
         FROM token_usage tu
         WHERE tu.tenant_id = ? AND (? = 1 OR tu.user_id = ?)
           AND tu.created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND tu.usage_kind = 'request_delta'
           AND EXISTS (
             SELECT 1
             FROM pm_research_runs r
             WHERE r.tenant_id = tu.tenant_id
               AND r.session_id = tu.session_id
               AND r.status IN ('completed','failed','cancelled')
               AND r.started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
               AND tu.created_at >= r.started_at
               AND tu.created_at <= COALESCE(r.ended_at, CURRENT_TIMESTAMP)
           )
         GROUP BY model, provider, pricing_source
         ORDER BY estimated_cost_usd DESC, total_tokens DESC
         LIMIT 8",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(days)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let cost_by_model: Vec<serde_json::Value> = cost_by_model_rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "model": row.get::<String, _>(0),
                "provider": row.get::<String, _>(1),
                "pricingSource": row.get::<String, _>(2),
                "requestCount": row.get::<i64, _>(3),
                "usageRecordCount": row.get::<i64, _>(3),
                "totalTokens": row.get::<i64, _>(4),
                "estimatedCostUsd": row.get::<f64, _>(5),
            })
        })
        .collect();

    let cost_run_coverage_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COUNT(*) AS INTEGER) AS measured_run_count,
            CAST(COALESCE(SUM(CASE
                WHEN r.status = 'completed'
                 AND COALESCE(CAST(JSON_EXTRACT(r.metadata_json, '$.answerTextPresent') AS INTEGER), 0) = 1
                THEN 1 ELSE 0 END), 0) AS INTEGER) AS measured_answer_delivery_count
         FROM pm_research_runs r
         WHERE r.tenant_id = ? AND (? = 1 OR r.user_id = ?)
           AND r.status IN ('completed','failed','cancelled')
           AND r.started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND EXISTS (
             SELECT 1 FROM token_usage tu
             WHERE tu.tenant_id = r.tenant_id
               AND tu.session_id = r.session_id
               AND tu.usage_kind = 'request_delta'
               AND tu.created_at >= r.started_at
               AND tu.created_at <= COALESCE(r.ended_at, CURRENT_TIMESTAMP)
           )",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let deep_loop_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT
            CAST(COUNT(*) AS INTEGER) AS event_count,
            CAST(COUNT(DISTINCT run_id) AS INTEGER) AS loop_run_count,
            CAST(COUNT(DISTINCT CASE WHEN event_type = 'pm.deep_loop.finalized' THEN run_id END) AS INTEGER) AS finalized_count,
            CAST(COUNT(DISTINCT CASE WHEN event_type = 'pm.deep_loop.degraded_synthesis' THEN run_id END) AS INTEGER) AS degraded_count,
            CAST(COALESCE(SUM(CASE WHEN event_type = 'pm.deep_loop.followup_planned' THEN 1 ELSE 0 END), 0) AS INTEGER) AS followup_count,
            CAST(COUNT(DISTINCT CASE
                WHEN event_type = 'pm.deep_loop.evidence_scored'
                 AND JSON_EXTRACT(payload_json, '$.scores.decisionReadinessScore') IS NOT NULL
                THEN run_id END) AS INTEGER) AS score_sample_count,
            CAST(COALESCE(AVG(CASE WHEN event_type = 'pm.deep_loop.evidence_scored' THEN CAST(JSON_EXTRACT(payload_json, '$.scores.decisionReadinessScore') AS DECIMAL(18,6)) END), 0.0) AS DOUBLE) AS avg_decision_readiness,
            CAST(COALESCE(AVG(CASE WHEN event_type = 'pm.deep_loop.evidence_scored' THEN CAST(JSON_EXTRACT(payload_json, '$.scores.actionabilityScore') AS DECIMAL(18,6)) END), 0.0) AS DOUBLE) AS avg_actionability,
            CAST(COALESCE(AVG(CASE WHEN event_type = 'pm.deep_loop.evidence_scored' THEN CAST(JSON_EXTRACT(payload_json, '$.scores.firstPartyAlignmentScore') AS DECIMAL(18,6)) END), 0.0) AS DOUBLE) AS avg_first_party_alignment,
            CAST(COALESCE(AVG(CASE WHEN event_type = 'pm.deep_loop.evidence_scored' THEN CAST(JSON_EXTRACT(payload_json, '$.scores.evidenceCoverageScore') AS DECIMAL(18,6)) END), 0.0) AS DOUBLE) AS avg_evidence_coverage
         FROM pm_audit_trails
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
           AND event_type LIKE 'pm.deep_loop.%'",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let failure_drilldown_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT bucket, CAST(COUNT(*) AS INTEGER) AS event_count
         FROM (
            SELECT CASE
                WHEN LOWER(COALESCE(error_code, error_message, '')) LIKE '%direct%timeout%' THEN 'direct_timeout'
                WHEN LOWER(COALESCE(error_code, error_message, '')) LIKE '%retrieve%timeout%' THEN 'retrieval_timeout'
                WHEN LOWER(COALESCE(error_code, error_message, '')) LIKE '%force%synth%' THEN 'force_synth_fallback'
                WHEN LOWER(COALESCE(error_code, error_message, '')) LIKE '%quality%' THEN 'quality_rewrite'
                WHEN LOWER(COALESCE(error_code, error_message, '')) LIKE '%timeout%' THEN 'timeout_other'
                ELSE 'other'
            END AS bucket
            FROM pm_research_runs
            WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
              AND started_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
              AND (error_code IS NOT NULL OR error_message IS NOT NULL)
            UNION ALL
            SELECT CASE
                WHEN event_type = 'pm.deep_loop.quality_failed' THEN 'quality_rewrite'
                WHEN event_type = 'pm.deep_loop.degraded_synthesis' THEN 'force_synth_fallback'
                WHEN event_type = 'pm.deep_loop.followup_planned' THEN 'quality_rewrite'
                WHEN LOWER(COALESCE(message, '')) LIKE '%timeout%' THEN 'timeout_other'
                ELSE 'other'
            END AS bucket
            FROM pm_audit_trails
            WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
              AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))
              AND (
                event_type IN ('pm.deep_loop.quality_failed','pm.deep_loop.degraded_synthesis','pm.deep_loop.followup_planned')
                OR LOWER(COALESCE(message, '')) LIKE '%timeout%'
              )
         ) buckets
         GROUP BY bucket
         ORDER BY event_count DESC",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let failure_drilldown: Vec<serde_json::Value> = failure_drilldown_rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "bucket": row.get::<String, _>(0),
                "count": row.get::<i64, _>(1),
            })
        })
        .collect();

    let total_runs = run_summary_row.get::<i64, _>(0).max(0);
    let retried_runs = run_summary_row.get::<i64, _>(6).max(0);
    let recovered_runs = run_summary_row.get::<i64, _>(7).max(0);
    let first_pass_completed_runs = run_summary_row.get::<i64, _>(8).max(0);
    let retry_repair_attempts = source_quota_row.get::<i64, _>(0).max(0);
    let source_quota_exhausted_attempts = source_quota_row.get::<i64, _>(1).max(0);
    let terminal_runs = run_summary_row
        .get::<i64, _>(3)
        .max(0)
        .saturating_add(run_summary_row.get::<i64, _>(4).max(0))
        .saturating_add(run_summary_row.get::<i64, _>(5).max(0));
    let cancelled_runs = run_summary_row.get::<i64, _>(5).max(0);
    let completed_runs = run_summary_row.get::<i64, _>(3).max(0);
    let failed_runs = run_summary_row.get::<i64, _>(4).max(0);
    let event_count = deep_loop_row.get::<i64, _>(0).max(0);
    let deep_loop_run_count = deep_loop_row.get::<i64, _>(1).max(0);
    let degraded_deep_loop_count = deep_loop_row.get::<i64, _>(3).max(0);
    let deep_score_sample_count = deep_loop_row.get::<i64, _>(5).max(0);
    let deep_terminal_event_count = deep_loop_row
        .get::<i64, _>(2)
        .max(0)
        .saturating_add(degraded_deep_loop_count);
    let answer_delivery_runs = match sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COALESCE(SUM(CASE
             WHEN status = 'completed'
              AND COALESCE(CAST(JSON_EXTRACT(metadata_json, '$.answerTextPresent') AS INTEGER), 0) = 1
             THEN 1 ELSE 0 END), 0) AS INTEGER)
         FROM pm_research_runs
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
           AND COALESCE(started_at, created_at) >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?))",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(days)
    .fetch_one(&state.db)
    .await
    {
        Ok(value) => value.max(0),
        Err(e) => return AppError::Database(e).into_response(),
    };
    let usage_record_count = cost_row.get::<i64, _>(0).max(0);
    let priced_usage_record_count = cost_row.get::<i64, _>(1).max(0);
    let unpriced_usage_record_count = cost_row.get::<i64, _>(2).max(0);
    let estimated_cost_usd = cost_row.get::<f64, _>(6).max(0.0);
    let cost_sample_run_count = cost_run_coverage_row.get::<i64, _>(0).max(0);
    let measured_answer_delivery_runs = cost_run_coverage_row.get::<i64, _>(1).max(0);
    let queued_count = queue_health_row.get::<i64, _>(0).max(0);
    let running_count = queue_health_row.get::<i64, _>(1).max(0);
    let oldest_queued_task_age_secs = queue_health_row
        .get::<Option<i64>, _>(2)
        .map(|value| value.max(0));
    let longest_running_task_age_secs = queue_health_row
        .get::<Option<i64>, _>(3)
        .map(|value| value.max(0));
    let stale_running_tasks = queue_health_row.get::<i64, _>(4).max(0);
    let avg_queue_wait_secs = queue_health_row
        .get::<Option<i64>, _>(5)
        .map(|value| value.max(0));
    let longest_running_heartbeat_age_secs = queue_health_row
        .get::<Option<i64>, _>(6)
        .map(|value| value.max(0));

    Json(serde_json::json!({
        "days": days,
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "summary": {
            "totalRuns": total_runs,
            "queuedRuns": run_summary_row.get::<i64, _>(1).max(0),
            "runningRuns": run_summary_row.get::<i64, _>(2).max(0),
            "completedRuns": completed_runs,
            "failedRuns": failed_runs,
            "cancelledRuns": cancelled_runs,
            "terminalRuns": terminal_runs,
            "terminalRate": if total_runs > 0 { terminal_runs as f64 / total_runs as f64 } else { 0.0 },
            "answerDeliveryRuns": answer_delivery_runs,
            "answerDeliveryRate": if total_runs > 0 { answer_delivery_runs as f64 / total_runs as f64 } else { 0.0 },
            "retriedRuns": retried_runs,
            "recoveredRuns": recovered_runs,
            "retryRecoveryRate": if retried_runs > 0 { recovered_runs as f64 / retried_runs as f64 } else { 0.0 },
            "firstPassSuccessRate": if total_runs > 0 { first_pass_completed_runs as f64 / total_runs as f64 } else { 0.0 },
            "failureRate": if total_runs > 0 { failed_runs as f64 / total_runs as f64 } else { 0.0 },
            "manualInterruptionRate": if total_runs > 0 { cancelled_runs as f64 / total_runs as f64 } else { 0.0 },
            "currentQueuedTasks": task_backlog_row.get::<i64, _>(0).max(0),
            "currentRunningTasks": task_backlog_row.get::<i64, _>(1).max(0),
            "currentQueuedSubtasks": subtask_backlog_row.get::<i64, _>(0).max(0),
            "currentRunningSubtasks": subtask_backlog_row.get::<i64, _>(1).max(0),
            "retryRepairAttempts": retry_repair_attempts,
            "sourceQuotaExhaustedAttempts": source_quota_exhausted_attempts,
            "sourceQuotaExhaustedRate": if retry_repair_attempts > 0 {
                source_quota_exhausted_attempts as f64 / retry_repair_attempts as f64
            } else {
                0.0
            },
            "degradedSynthesisRate": if deep_terminal_event_count > 0 { degraded_deep_loop_count as f64 / deep_terminal_event_count as f64 } else { 0.0 },
            "derived": true,
        },
        "queueHealth": {
            "queuedCount": queued_count,
            "runningCount": running_count,
            "queuedTasks": task_backlog_row.get::<i64, _>(0).max(0),
            "runningTasks": task_backlog_row.get::<i64, _>(1).max(0),
            "queuedRuns": run_backlog_row.get::<i64, _>(0).max(0),
            "runningRuns": run_backlog_row.get::<i64, _>(1).max(0),
            "queuedSubtasks": subtask_backlog_row.get::<i64, _>(0).max(0),
            "runningSubtasks": subtask_backlog_row.get::<i64, _>(1).max(0),
            "oldestQueuedObject": oldest_queued_object,
            "oldestQueuedTaskAgeSecs": oldest_queued_task_age_secs,
            "longestRunningTaskAgeSecs": longest_running_task_age_secs,
            "staleRunningTasks": stale_running_tasks,
            "avgQueueWaitSecs": avg_queue_wait_secs,
            "longestRunningHeartbeatAgeSecs": longest_running_heartbeat_age_secs,
        },
        "cost": {
            "requestCount": usage_record_count,
            "usageRecordCount": usage_record_count,
            "pricedUsageRecordCount": priced_usage_record_count,
            "unpricedUsageRecordCount": unpriced_usage_record_count,
            "pricingCoverage": if usage_record_count > 0 { priced_usage_record_count as f64 / usage_record_count as f64 } else { 0.0 },
            "costComplete": usage_record_count > 0 && unpriced_usage_record_count == 0,
            "totalTokens": cost_row.get::<i64, _>(3).max(0),
            "inputTokens": cost_row.get::<i64, _>(4).max(0),
            "outputTokens": cost_row.get::<i64, _>(5).max(0),
            "estimatedCostUsd": estimated_cost_usd,
            "costSampleRunCount": cost_sample_run_count,
            "costRunCoverage": if terminal_runs > 0 { cost_sample_run_count as f64 / terminal_runs as f64 } else { 0.0 },
            "avgCostPerRunUsd": if cost_sample_run_count > 0 && unpriced_usage_record_count == 0 {
                Some(estimated_cost_usd / cost_sample_run_count as f64)
            } else { None },
            "avgCostPerSuccessfulDeliveryUsd": if measured_answer_delivery_runs > 0 && unpriced_usage_record_count == 0 {
                Some(estimated_cost_usd / measured_answer_delivery_runs as f64)
            } else { None },
            "byModel": cost_by_model,
            "derivedFromTokenUsage": true,
        },
        "deepResearch": {
            "eventCount": event_count,
            "runCount": deep_loop_run_count,
            "scoreSampleCount": deep_score_sample_count,
            "scoreSampleCoverage": if deep_loop_run_count > 0 { deep_score_sample_count as f64 / deep_loop_run_count as f64 } else { 0.0 },
            "finalizedCount": deep_loop_row.get::<i64, _>(2).max(0),
            "degradedSynthesisCount": degraded_deep_loop_count,
            "followupPlannedCount": deep_loop_row.get::<i64, _>(4).max(0),
            "avgDecisionReadiness": deep_loop_row.get::<f64, _>(6).clamp(0.0, 1.0),
            "avgActionability": deep_loop_row.get::<f64, _>(7).clamp(0.0, 1.0),
            "avgFirstPartyAlignment": deep_loop_row.get::<f64, _>(8).clamp(0.0, 1.0),
            "avgEvidenceCoverage": deep_loop_row.get::<f64, _>(9).clamp(0.0, 1.0),
        },
        "failureDrilldown": failure_drilldown,
        "userOutcome": {
            "retryRate": if total_runs > 0 { retried_runs as f64 / total_runs as f64 } else { 0.0 },
            "manualInterruptionRate": if total_runs > 0 { cancelled_runs as f64 / total_runs as f64 } else { 0.0 },
            "followupRepairCount": deep_loop_row.get::<i64, _>(3).max(0),
            "lowQualitySignalSource": "derived_from_quality_gate_and_runtime_events",
        },
        "dailyRuns": daily_runs,
        "dailySourceQuota": daily_source_quota,
    }))
    .into_response()
}

pub(super) async fn list_pm_research_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsListQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = if let Some(status) = query.status.as_deref() {
        sqlx::query::<sqlx::Sqlite>(
            "SELECT run_id, task_id, session_id, source, status, current_stage, attempt,
                    budget_profile, total_elapsed_ms, error_code, error_message,
                    CAST(final_quality_score AS DOUBLE), started_at, ended_at
             FROM pm_research_runs
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND status = ?
             ORDER BY updated_at DESC
             LIMIT ?",
        )
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(status)
        .bind(limit)
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query::<sqlx::Sqlite>(
            "SELECT run_id, task_id, session_id, source, status, current_stage, attempt,
                    budget_profile, total_elapsed_ms, error_code, error_message,
                    CAST(final_quality_score AS DOUBLE), started_at, ended_at
             FROM pm_research_runs
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
             ORDER BY updated_at DESC
             LIMIT ?",
        )
        .bind(&claims.tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(limit)
        .fetch_all(&state.db)
        .await
    };
    let rows = match rows {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "runId": row.get::<String, _>(0),
                "taskId": row.get::<Option<String>, _>(1),
                "sessionId": row.get::<String, _>(2),
                "source": row.get::<String, _>(3),
                "status": row.get::<String, _>(4),
                "currentStage": row.get::<Option<String>, _>(5),
                "attempt": row.get::<Option<i32>, _>(6),
                "budgetProfile": row.get::<String, _>(7),
                "totalElapsedMs": row.get::<Option<i64>, _>(8),
                "errorCode": row.get::<Option<String>, _>(9),
                "errorMessage": row.get::<Option<String>, _>(10),
                "finalQualityScore": row.get::<Option<f64>, _>(11),
                "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(12),
                "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(13),
            })
        })
        .collect();
    Json(serde_json::json!({"rows": items, "total": items.len()})).into_response()
}

pub(super) async fn get_pm_research_run_trace(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(run_id): Path<String>,
    Query(query): Query<PmRunTraceQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let run_id = run_id.trim().to_string();
    if run_id.is_empty() {
        return AppError::ValidationError("run_id cannot be empty".to_string()).into_response();
    }

    let lite = query.lite.unwrap_or(false);
    let include_raw_io = query.include_raw_io.unwrap_or(!lite);
    let stage_limit = pm_ops_limit_i64(query.stage_limit, if lite { 120 } else { 400 }, 1, 2_000);
    let slot_limit = pm_ops_limit_i64(
        query.source_slot_limit,
        if lite { 240 } else { 800 },
        1,
        5_000,
    );
    let tool_limit = pm_ops_limit_i64(
        query.tool_call_limit,
        if lite { 600 } else { 4_000 },
        1,
        20_000,
    );
    let subtask_limit =
        pm_ops_limit_i64(query.subtask_limit, if lite { 160 } else { 400 }, 1, 5_000);
    let subtask_attempt_limit = pm_ops_limit_i64(
        query.subtask_attempt_limit,
        if lite { 360 } else { 1_200 },
        1,
        20_000,
    );
    let task_event_limit = pm_ops_limit_i64(
        query.task_event_limit,
        if lite { 300 } else { 1_000 },
        1,
        20_000,
    );
    let audit_limit = pm_ops_limit_i64(query.audit_limit, if lite { 120 } else { 500 }, 1, 10_000);
    let claim_limit = pm_ops_limit_i64(query.claim_limit, if lite { 180 } else { 800 }, 1, 20_000);
    let conflict_limit = pm_ops_limit_i64(
        query.conflict_limit,
        if lite { 120 } else { 400 },
        1,
        10_000,
    );
    let _repair_limit =
        pm_ops_limit_i64(query.repair_limit, if lite { 120 } else { 400 }, 1, 10_000);
    let prompt_limit = pm_ops_limit_i64(query.prompt_limit, if lite { 40 } else { 200 }, 1, 2_000);
    let mut degraded_sections: Vec<String> = Vec::new();

    let run_row = match sqlx::query::<sqlx::Sqlite>(
        "SELECT run_id, task_id, session_id, source, status, current_stage, attempt,
                budget_profile, pipeline_timeout_secs, max_attempts,
                source_slot_search_secs, source_slot_browser_secs, source_slot_api_fetch_secs,
                retrieve_max_tool_calls, max_calls_per_source, user_message, total_elapsed_ms,
                error_code, error_message, CAST(final_quality_score AS DOUBLE), metadata_json,
                started_at, deadline_at, ended_at, created_at, updated_at
         FROM pm_research_runs
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND run_id = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&run_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return AppError::NotFound(format!("pm run {} not found", run_id)).into_response()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let task_id = run_row.get::<Option<String>, _>(1);
    let run_json = serde_json::json!({
        "runId": run_row.get::<String, _>(0),
        "taskId": task_id,
        "sessionId": run_row.get::<String, _>(2),
        "source": run_row.get::<String, _>(3),
        "status": run_row.get::<String, _>(4),
        "currentStage": run_row.get::<Option<String>, _>(5),
        "attempt": run_row.get::<Option<i32>, _>(6),
        "budgetProfile": run_row.get::<String, _>(7),
        "budget": {
            "pipelineTimeoutSecs": run_row.get::<i32, _>(8),
            "maxAttempts": run_row.get::<i32, _>(9),
            "sourceSlotSearchSecs": run_row.get::<i32, _>(10),
            "sourceSlotBrowserSecs": run_row.get::<i32, _>(11),
            "sourceSlotApiFetchSecs": run_row.get::<i32, _>(12),
            "retrieveMaxToolCalls": run_row.get::<i32, _>(13),
            "maxCallsPerSource": run_row.get::<i32, _>(14),
        },
        "input": {
            "userMessage": run_row.get::<Option<String>, _>(15),
        },
        "result": {
            "totalElapsedMs": run_row.get::<Option<i64>, _>(16),
            "errorCode": run_row.get::<Option<String>, _>(17),
            "errorMessage": run_row.get::<Option<String>, _>(18),
            "finalQualityScore": run_row.get::<Option<f64>, _>(19),
            "metadata": pm_row_json_value(&run_row, 20),
        },
        "startedAt": run_row.get::<Option<chrono::NaiveDateTime>, _>(21),
        "deadlineAt": run_row.get::<Option<chrono::NaiveDateTime>, _>(22),
        "endedAt": run_row.get::<Option<chrono::NaiveDateTime>, _>(23),
        "createdAt": run_row.get::<chrono::NaiveDateTime, _>(24),
        "updatedAt": run_row.get::<chrono::NaiveDateTime, _>(25),
    });

    let stage_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT id, stage, attempt_no, status, strategy, route_key, channel, variant,
                timeout_secs, budget_secs, elapsed_ms, detail_json, repair_scope_json, result_json,
                error_code, error_message, started_at, ended_at, created_at, updated_at
         FROM pm_research_stage_attempts
         WHERE run_id = ?
         ORDER BY attempt_no ASC, id ASC
         LIMIT ?",
    )
    .bind(&run_id)
    .bind(stage_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<u64, _>(0),
                    "stage": row.get::<String, _>(1),
                    "attemptNo": row.get::<i32, _>(2),
                    "status": row.get::<String, _>(3),
                    "strategy": row.get::<Option<String>, _>(4),
                    "routeKey": row.get::<Option<String>, _>(5),
                    "channel": row.get::<Option<String>, _>(6),
                    "variant": row.get::<Option<String>, _>(7),
                    "timeoutSecs": row.get::<Option<i32>, _>(8),
                    "budgetSecs": row.get::<Option<i32>, _>(9),
                    "elapsedMs": row.get::<Option<i64>, _>(10),
                    "detail": pm_row_json_value(&row, 11),
                    "repairScope": pm_row_json_value(&row, 12),
                    "result": pm_row_json_value(&row, 13),
                    "errorCode": row.get::<Option<String>, _>(14),
                    "errorMessage": row.get::<Option<String>, _>(15),
                    "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(16),
                    "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(17),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(18),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(19),
                })
            })
            .collect::<Vec<_>>(),
        Err(error) if pm_is_unknown_column(&error, &["repair_scope_json", "result_json"]) => {
            let rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT id, stage, attempt_no, status, strategy, route_key, channel, variant,
                        timeout_secs, budget_secs, elapsed_ms, detail_json,
                        error_code, error_message, started_at, ended_at, created_at, updated_at
                 FROM pm_research_stage_attempts
                 WHERE run_id = ?
                 ORDER BY attempt_no ASC, id ASC
                 LIMIT ?",
            )
            .bind(&run_id)
            .bind(stage_limit)
            .fetch_all(&state.db)
            .await
            {
                Ok(v) => v,
                Err(e) => return AppError::Database(e).into_response(),
            };
            rows.into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.get::<u64, _>(0),
                        "stage": row.get::<String, _>(1),
                        "attemptNo": row.get::<i32, _>(2),
                        "status": row.get::<String, _>(3),
                        "strategy": row.get::<Option<String>, _>(4),
                        "routeKey": row.get::<Option<String>, _>(5),
                        "channel": row.get::<Option<String>, _>(6),
                        "variant": row.get::<Option<String>, _>(7),
                        "timeoutSecs": row.get::<Option<i32>, _>(8),
                        "budgetSecs": row.get::<Option<i32>, _>(9),
                        "elapsedMs": row.get::<Option<i64>, _>(10),
                        "detail": pm_row_json_value(&row, 11),
                        "repairScope": serde_json::Value::Null,
                        "result": serde_json::Value::Null,
                        "errorCode": row.get::<Option<String>, _>(12),
                        "errorMessage": row.get::<Option<String>, _>(13),
                        "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(14),
                        "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(15),
                        "createdAt": row.get::<chrono::NaiveDateTime, _>(16),
                        "updatedAt": row.get::<chrono::NaiveDateTime, _>(17),
                    })
                })
                .collect::<Vec<_>>()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let slot_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT id, stage_attempt_id, slot_seq, route_key, channel, variant, source_key, source_url,
                status, tool_call_count, elapsed_ms, error_code, error_message, detail_json,
                started_at, ended_at, created_at, updated_at
         FROM pm_research_source_slots
         WHERE run_id = ?
         ORDER BY slot_seq ASC, id ASC
         LIMIT ?",
    )
    .bind(&run_id)
    .bind(slot_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<u64, _>(0),
                    "stageAttemptId": row.get::<Option<u64>, _>(1),
                    "slotSeq": row.get::<i32, _>(2),
                    "routeKey": row.get::<Option<String>, _>(3),
                    "channel": row.get::<Option<String>, _>(4),
                    "variant": row.get::<Option<String>, _>(5),
                    "sourceKey": row.get::<Option<String>, _>(6),
                    "sourceUrl": row.get::<Option<String>, _>(7),
                    "status": row.get::<String, _>(8),
                    "toolCallCount": row.get::<i32, _>(9),
                    "elapsedMs": row.get::<Option<i64>, _>(10),
                    "errorCode": row.get::<Option<String>, _>(11),
                    "errorMessage": row.get::<Option<String>, _>(12),
                    "detail": pm_row_json_value(&row, 13),
                    "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(14),
                    "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(15),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(16),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(17),
                })
            })
            .collect::<Vec<_>>(),
        Err(e) => return AppError::Database(e).into_response(),
    };

    let tool_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT id, stage_attempt_id, source_slot_id, CAST(call_seq AS INTEGER) AS call_seq,
                tool_name, tool_use_id,
                input_preview, output_preview, input_raw, output_raw, is_error,
                error_code, error_message, http_status, latency_ms, route_key, channel,
                provider, provider_trace, url, domain, created_at
         FROM pm_research_tool_call_ledger
         WHERE run_id = ?
         ORDER BY call_seq ASC, id ASC
         LIMIT ?",
    )
    .bind(&run_id)
    .bind(tool_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<u64, _>(0),
                    "stageAttemptId": row.get::<Option<u64>, _>(1),
                    "sourceSlotId": row.get::<Option<u64>, _>(2),
                    "callSeq": row.get::<i64, _>(3),
                    "toolName": row.get::<String, _>(4),
                    "toolUseId": row.get::<Option<String>, _>(5),
                    "inputPreview": row.get::<Option<String>, _>(6),
                    "outputPreview": row.get::<Option<String>, _>(7),
                    "inputRaw": if include_raw_io { row.get::<Option<String>, _>(8) } else { None },
                    "outputRaw": if include_raw_io { row.get::<Option<String>, _>(9) } else { None },
                    "isError": row.get::<i8, _>(10) == 1,
                    "errorCode": row.get::<Option<String>, _>(11),
                    "errorMessage": row.get::<Option<String>, _>(12),
                    "httpStatus": row.get::<Option<i32>, _>(13),
                    "latencyMs": row.get::<Option<i64>, _>(14),
                    "routeKey": row.get::<Option<String>, _>(15),
                    "channel": row.get::<Option<String>, _>(16),
                    "provider": row.get::<Option<String>, _>(17),
                    "providerTrace": row.get::<Option<String>, _>(18),
                    "url": row.get::<Option<String>, _>(19),
                    "domain": row.get::<Option<String>, _>(20),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(21),
                })
            })
            .collect::<Vec<_>>(),
        Err(error)
            if pm_is_unknown_column(
                &error,
                &["input_raw", "output_raw", "provider", "provider_trace"],
            ) =>
        {
            let rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT id, stage_attempt_id, source_slot_id, CAST(call_seq AS INTEGER) AS call_seq,
                        tool_name, tool_use_id,
                        input_preview, output_preview, is_error, error_code, error_message,
                        http_status, latency_ms, route_key, channel, url, domain, created_at
                 FROM pm_research_tool_call_ledger
                 WHERE run_id = ?
                 ORDER BY call_seq ASC, id ASC
                 LIMIT ?",
            )
            .bind(&run_id)
            .bind(tool_limit)
            .fetch_all(&state.db)
            .await
            {
                Ok(v) => v,
                Err(e) => return AppError::Database(e).into_response(),
            };
            rows.into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.get::<u64, _>(0),
                        "stageAttemptId": row.get::<Option<u64>, _>(1),
                        "sourceSlotId": row.get::<Option<u64>, _>(2),
                        "callSeq": row.get::<i64, _>(3),
                        "toolName": row.get::<String, _>(4),
                        "toolUseId": row.get::<Option<String>, _>(5),
                        "inputPreview": row.get::<Option<String>, _>(6),
                        "outputPreview": row.get::<Option<String>, _>(7),
                        "inputRaw": serde_json::Value::Null,
                        "outputRaw": serde_json::Value::Null,
                        "isError": row.get::<i8, _>(8) == 1,
                        "errorCode": row.get::<Option<String>, _>(9),
                        "errorMessage": row.get::<Option<String>, _>(10),
                        "httpStatus": row.get::<Option<i32>, _>(11),
                        "latencyMs": row.get::<Option<i64>, _>(12),
                        "routeKey": row.get::<Option<String>, _>(13),
                        "channel": row.get::<Option<String>, _>(14),
                        "provider": serde_json::Value::Null,
                        "providerTrace": serde_json::Value::Null,
                        "url": row.get::<Option<String>, _>(15),
                        "domain": row.get::<Option<String>, _>(16),
                        "createdAt": row.get::<chrono::NaiveDateTime, _>(17),
                    })
                })
                .collect::<Vec<_>>()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let subtask_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT id, subtask_key, subtask_id, title, goal, deliverable, required_evidence_type,
                priority, status, probe_candidate_count, probe_completed_count, citation_count,
                domain_count, tool_call_count, CAST(quality_score AS DOUBLE), error_code,
                error_message, detail_json, started_at, ended_at, created_at, updated_at
         FROM pm_subtask_runs
         WHERE run_id = ?
         ORDER BY id ASC
         LIMIT ?",
    )
    .bind(&run_id)
    .bind(subtask_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<u64, _>(0),
                    "subtaskKey": row.get::<String, _>(1),
                    "subtaskId": row.get::<Option<String>, _>(2),
                    "title": row.get::<String, _>(3),
                    "goal": row.get::<Option<String>, _>(4),
                    "deliverable": row.get::<Option<String>, _>(5),
                    "requiredEvidenceType": row.get::<Option<String>, _>(6),
                    "priority": row.get::<String, _>(7),
                    "status": row.get::<String, _>(8),
                    "probeCandidateCount": row.get::<u32, _>(9),
                    "probeCompletedCount": row.get::<u32, _>(10),
                    "citationCount": row.get::<u32, _>(11),
                    "domainCount": row.get::<u32, _>(12),
                    "toolCallCount": row.get::<u32, _>(13),
                    "qualityScore": row.get::<Option<f64>, _>(14),
                    "errorCode": row.get::<Option<String>, _>(15),
                    "errorMessage": row.get::<Option<String>, _>(16),
                    "detail": pm_row_json_value(&row, 17),
                    "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(18),
                    "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(19),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(20),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(21),
                })
            })
            .collect::<Vec<_>>(),
        Err(e) => return AppError::Database(e).into_response(),
    };

    let subtask_attempt_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT id, subtask_run_id, subtask_key, attempt_no, attempt_key, variant,
                route_key, route_channel, status, elapsed_ms, citation_count, domain_count,
                tool_call_count, CAST(quality_score AS DOUBLE), error_code, error_message,
                detail_json, started_at, ended_at, created_at, updated_at
         FROM pm_subtask_attempts
         WHERE run_id = ?
         ORDER BY subtask_run_id ASC, attempt_no ASC, id ASC
         LIMIT ?",
    )
    .bind(&run_id)
    .bind(subtask_attempt_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<u64, _>(0),
                    "subtaskRunId": row.get::<u64, _>(1),
                    "subtaskKey": row.get::<String, _>(2),
                    "attemptNo": row.get::<i32, _>(3),
                    "attemptKey": row.get::<String, _>(4),
                    "variant": row.get::<Option<String>, _>(5),
                    "routeKey": row.get::<Option<String>, _>(6),
                    "routeChannel": row.get::<Option<String>, _>(7),
                    "status": row.get::<String, _>(8),
                    "elapsedMs": row.get::<Option<i64>, _>(9),
                    "citationCount": row.get::<u32, _>(10),
                    "domainCount": row.get::<u32, _>(11),
                    "toolCallCount": row.get::<u32, _>(12),
                    "qualityScore": row.get::<Option<f64>, _>(13),
                    "errorCode": row.get::<Option<String>, _>(14),
                    "errorMessage": row.get::<Option<String>, _>(15),
                    "detail": pm_row_json_value(&row, 16),
                    "startedAt": row.get::<Option<chrono::NaiveDateTime>, _>(17),
                    "endedAt": row.get::<Option<chrono::NaiveDateTime>, _>(18),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(19),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(20),
                })
            })
            .collect::<Vec<_>>(),
        Err(e) => return AppError::Database(e).into_response(),
    };

    let quality_gate = match sqlx::query::<sqlx::Sqlite>(
        "SELECT passed, CAST(quality_score AS DOUBLE), tool_call_count, citation_count,
                domain_count, claim_count, claim_alignment_ok, triad_total_claims,
                triad_aligned_claims, CAST(triad_coverage AS DOUBLE), conflict_adjudicated,
                CAST(conflict_confidence AS DOUBLE), missing_json, suggestions_json, created_at
         FROM pm_quality_gate_metrics
         WHERE tenant_id = ? AND run_id = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(row)) => Some(serde_json::json!({
            "passed": row.get::<i8, _>(0) == 1,
            "qualityScore": row.get::<f64, _>(1),
            "toolCallCount": row.get::<i32, _>(2),
            "citationCount": row.get::<i32, _>(3),
            "domainCount": row.get::<i32, _>(4),
            "claimCount": row.get::<i32, _>(5),
            "claimAlignmentOk": row.get::<i8, _>(6) == 1,
            "triadTotalClaims": row.get::<i32, _>(7),
            "triadAlignedClaims": row.get::<i32, _>(8),
            "triadCoverage": row.get::<f64, _>(9),
            "conflictAdjudicated": row.get::<i8, _>(10) == 1,
            "conflictConfidence": row.get::<f64, _>(11),
            "missing": pm_row_json_value(&row, 12),
            "suggestions": pm_row_json_value(&row, 13),
            "createdAt": row.get::<chrono::NaiveDateTime, _>(14),
        })),
        Ok(None) => None,
        Err(e) => return AppError::Database(e).into_response(),
    };

    let claim_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT claim_key, claim_text, verdict, CAST(confidence AS DOUBLE), evidence_excerpt,
                url, domain, reason, created_at, updated_at
         FROM pm_claim_verdicts
         WHERE tenant_id = ? AND run_id = ?
         ORDER BY updated_at DESC, created_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .bind(claim_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "claimKey": row.get::<String, _>(0),
                    "claimText": row.get::<String, _>(1),
                    "verdict": row.get::<String, _>(2),
                    "confidence": row.get::<f64, _>(3),
                    "evidenceExcerpt": row.get::<Option<String>, _>(4),
                    "url": row.get::<Option<String>, _>(5),
                    "domain": row.get::<Option<String>, _>(6),
                    "reason": row.get::<Option<String>, _>(7),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(8),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(9),
                })
            })
            .collect::<Vec<_>>(),
        Err(error) if pm_is_sort_memory_error(&error) => {
            degraded_sections.push("claimVerdicts(sort_memory)".to_string());
            let rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT claim_key, claim_text, verdict, CAST(confidence AS DOUBLE), evidence_excerpt,
                        url, domain, reason, created_at, updated_at
                 FROM pm_claim_verdicts
                 WHERE tenant_id = ? AND run_id = ?
                 LIMIT ?",
            )
            .bind(&claims.tenant_id)
            .bind(&run_id)
            .bind(claim_limit.min(200))
            .fetch_all(&state.db)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    degraded_sections.push("claimVerdicts(fallback_failed)".to_string());
                    tracing::warn!(
                        run_id = %run_id,
                        tenant_id = %claims.tenant_id,
                        error = %e,
                        "run trace fallback claim query failed"
                    );
                    Vec::new()
                }
            };
            rows.into_iter()
                .map(|row| {
                    serde_json::json!({
                        "claimKey": row.get::<String, _>(0),
                        "claimText": row.get::<String, _>(1),
                        "verdict": row.get::<String, _>(2),
                        "confidence": row.get::<f64, _>(3),
                        "evidenceExcerpt": row.get::<Option<String>, _>(4),
                        "url": row.get::<Option<String>, _>(5),
                        "domain": row.get::<Option<String>, _>(6),
                        "reason": row.get::<Option<String>, _>(7),
                        "createdAt": row.get::<chrono::NaiveDateTime, _>(8),
                        "updatedAt": row.get::<chrono::NaiveDateTime, _>(9),
                    })
                })
                .collect::<Vec<_>>()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let conflict_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT topic_key, topic, source_a, claim_a, source_b, claim_b, verdict,
                CAST(confidence AS DOUBLE), reason, support_urls_json, created_at, updated_at
         FROM pm_conflict_cases
         WHERE tenant_id = ? AND run_id = ?
         ORDER BY updated_at DESC, created_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .bind(conflict_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "topicKey": row.get::<String, _>(0),
                    "topic": row.get::<String, _>(1),
                    "sourceA": row.get::<Option<String>, _>(2),
                    "claimA": row.get::<Option<String>, _>(3),
                    "sourceB": row.get::<Option<String>, _>(4),
                    "claimB": row.get::<Option<String>, _>(5),
                    "verdict": row.get::<Option<String>, _>(6),
                    "confidence": row.get::<f64, _>(7),
                    "reason": row.get::<Option<String>, _>(8),
                    "supportUrls": pm_row_json_value(&row, 9),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(10),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(11),
                })
            })
            .collect::<Vec<_>>(),
        Err(error) if pm_is_sort_memory_error(&error) => {
            degraded_sections.push("conflictCases(sort_memory)".to_string());
            let rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT topic_key, topic, source_a, claim_a, source_b, claim_b, verdict,
                        CAST(confidence AS DOUBLE), reason, support_urls_json, created_at, updated_at
                 FROM pm_conflict_cases
                 WHERE tenant_id = ? AND run_id = ?
                 LIMIT ?",
            )
            .bind(&claims.tenant_id)
            .bind(&run_id)
            .bind(conflict_limit.min(120))
            .fetch_all(&state.db)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    degraded_sections.push("conflictCases(fallback_failed)".to_string());
                    tracing::warn!(
                        run_id = %run_id,
                        tenant_id = %claims.tenant_id,
                        error = %e,
                        "run trace fallback conflict query failed"
                    );
                    Vec::new()
                }
            };
            rows.into_iter()
                .map(|row| {
                    serde_json::json!({
                        "topicKey": row.get::<String, _>(0),
                        "topic": row.get::<String, _>(1),
                        "sourceA": row.get::<Option<String>, _>(2),
                        "claimA": row.get::<Option<String>, _>(3),
                        "sourceB": row.get::<Option<String>, _>(4),
                        "claimB": row.get::<Option<String>, _>(5),
                        "verdict": row.get::<Option<String>, _>(6),
                        "confidence": row.get::<f64, _>(7),
                        "reason": row.get::<Option<String>, _>(8),
                        "supportUrls": pm_row_json_value(&row, 9),
                        "createdAt": row.get::<chrono::NaiveDateTime, _>(10),
                        "updatedAt": row.get::<chrono::NaiveDateTime, _>(11),
                    })
                })
                .collect::<Vec<_>>()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let repair_rows: Vec<serde_json::Value> = Vec::new();

    let audit_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT event_type, severity, message, payload_json, created_at
         FROM pm_audit_trails
         WHERE tenant_id = ? AND run_id = ?
         ORDER BY id ASC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .bind(audit_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "eventType": row.get::<String, _>(0),
                    "severity": row.get::<String, _>(1),
                    "message": row.get::<String, _>(2),
                    "payload": pm_row_json_value(&row, 3),
                    "createdAt": row.get::<chrono::NaiveDateTime, _>(4),
                })
            })
            .collect::<Vec<_>>(),
        Err(e) => return AppError::Database(e).into_response(),
    };

    let prompt_rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT prompt_key, prompt_version, prompt_hash, stage, CAST(run_count AS INTEGER),
                metadata_json, last_used_at, updated_at
         FROM pm_prompt_registry
         WHERE tenant_id = ? AND last_run_id = ?
         ORDER BY updated_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&run_id)
    .bind(prompt_limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "promptKey": row.get::<String, _>(0),
                    "promptVersion": row.get::<String, _>(1),
                    "promptHash": row.get::<String, _>(2),
                    "stage": row.get::<Option<String>, _>(3),
                    "runCount": row.get::<i64, _>(4),
                    "metadata": pm_row_json_value(&row, 5),
                    "lastUsedAt": row.get::<Option<chrono::NaiveDateTime>, _>(6),
                    "updatedAt": row.get::<chrono::NaiveDateTime, _>(7),
                })
            })
            .collect::<Vec<_>>(),
        Err(error) if pm_is_sort_memory_error(&error) => {
            degraded_sections.push("promptUsage(sort_memory)".to_string());
            let rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT prompt_key, prompt_version, prompt_hash, stage, CAST(run_count AS INTEGER),
                        metadata_json, last_used_at, updated_at
                 FROM pm_prompt_registry
                 WHERE tenant_id = ? AND last_run_id = ?
                 ORDER BY id DESC
                 LIMIT ?",
            )
            .bind(&claims.tenant_id)
            .bind(&run_id)
            .bind(prompt_limit.min(80))
            .fetch_all(&state.db)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    degraded_sections.push("promptUsage(fallback_failed)".to_string());
                    tracing::warn!(
                        run_id = %run_id,
                        tenant_id = %claims.tenant_id,
                        error = %e,
                        "run trace fallback prompt query failed"
                    );
                    Vec::new()
                }
            };
            rows.into_iter()
                .map(|row| {
                    serde_json::json!({
                        "promptKey": row.get::<String, _>(0),
                        "promptVersion": row.get::<String, _>(1),
                        "promptHash": row.get::<String, _>(2),
                        "stage": row.get::<Option<String>, _>(3),
                        "runCount": row.get::<i64, _>(4),
                        "metadata": pm_row_json_value(&row, 5),
                        "lastUsedAt": row.get::<Option<chrono::NaiveDateTime>, _>(6),
                        "updatedAt": row.get::<chrono::NaiveDateTime, _>(7),
                    })
                })
                .collect::<Vec<_>>()
        }
        Err(e) => return AppError::Database(e).into_response(),
    };

    let task_event_rows =
        if let Some(task_id) = run_json.get("taskId").and_then(serde_json::Value::as_str) {
            let id_rows = match sqlx::query::<sqlx::Sqlite>(
                "SELECT id
                 FROM pm_research_task_events
                 WHERE tenant_id = ? AND task_id = ?
                 ORDER BY seq ASC, id ASC
                 LIMIT ?",
            )
            .bind(&claims.tenant_id)
            .bind(task_id)
            .bind(task_event_limit)
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(e) => return AppError::Database(e).into_response(),
            };
            let ids = id_rows
                .into_iter()
                .filter_map(|row| row.try_get::<u64, _>(0).ok())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                Vec::new()
            } else {
                let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                    "SELECT id, seq, status, stage, attempt, message, elapsed_ms, stage_elapsed_ms,
                            detail_json, response_json, error_message, created_at
                     FROM pm_research_task_events
                     WHERE tenant_id = ",
                );
                builder
                    .push_bind(&claims.tenant_id)
                    .push(" AND task_id = ")
                    .push_bind(task_id)
                    .push(" AND id IN (");
                let mut separated = builder.separated(", ");
                for id in &ids {
                    separated.push_bind(crate::sqlite_i64(*id));
                }
                separated.push_unseparated(")");
                match builder.build().fetch_all(&state.db).await {
                    Ok(mut rows) => {
                        rows.sort_by_key(|row| row.get::<u64, _>(0));
                        rows.into_iter()
                            .map(|row| {
                                serde_json::json!({
                                    "seq": row.get::<u64, _>(1),
                                    "status": row.get::<String, _>(2),
                                    "stage": row.get::<Option<String>, _>(3),
                                    "attempt": row.get::<Option<i32>, _>(4),
                                    "message": row.get::<Option<String>, _>(5),
                                    "elapsedMs": row.get::<u64, _>(6),
                                    "stageElapsedMs": row.get::<Option<u64>, _>(7),
                                    "detail": pm_row_json_value(&row, 8),
                                    "response": pm_row_json_value(&row, 9),
                                    "errorMessage": row.get::<Option<String>, _>(10),
                                    "createdAt": row.get::<chrono::NaiveDateTime, _>(11),
                                })
                            })
                            .collect::<Vec<_>>()
                    }
                    Err(e) => return AppError::Database(e).into_response(),
                }
            }
        } else {
            Vec::new()
        };

    let stage_attempt_count = stage_rows.len();
    let source_slot_count = slot_rows.len();
    let tool_call_count = tool_rows.len();
    let subtask_run_count = subtask_rows.len();
    let subtask_attempt_count = subtask_attempt_rows.len();
    let claim_verdict_count = claim_rows.len();
    let conflict_case_count = conflict_rows.len();
    let repair_attempt_count = repair_rows.len();
    let audit_trail_count = audit_rows.len();
    let task_event_count = task_event_rows.len();
    let prompt_usage_count = prompt_rows.len();
    let deep_loop = stage_rows
        .iter()
        .rev()
        .find_map(|row| {
            let detail = row.get("detail")?;
            if let Some(nested) = detail.get("deepLoop") {
                return Some(nested.clone());
            }
            if detail
                .get("loopState")
                .and_then(serde_json::Value::as_str)
                .is_some()
                || detail
                    .get("event")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|event| event.starts_with("pm.deep_loop."))
            {
                return Some(detail.clone());
            }
            None
        })
        .unwrap_or(serde_json::Value::Null);

    Json(serde_json::json!({
        "runId": run_id,
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "lite": lite,
        "includeRawIo": include_raw_io,
        "degradedSections": degraded_sections,
        "deepLoop": deep_loop,
        "run": run_json,
        "lifecycle": {
            "stageAttempts": stage_rows,
            "sourceSlots": slot_rows,
            "toolCalls": tool_rows,
            "subtasks": {
                "runs": subtask_rows,
                "attempts": subtask_attempt_rows,
            },
            "qualityGate": quality_gate,
            "claimVerdicts": claim_rows,
            "conflictCases": conflict_rows,
            "repairAttempts": repair_rows,
            "auditTrails": audit_rows,
            "taskEvents": task_event_rows,
            "promptUsage": prompt_rows,
        },
        "summary": {
            "stageAttemptCount": stage_attempt_count,
            "sourceSlotCount": source_slot_count,
            "toolCallCount": tool_call_count,
            "subtaskRunCount": subtask_run_count,
            "subtaskAttemptCount": subtask_attempt_count,
            "claimVerdictCount": claim_verdict_count,
            "conflictCaseCount": conflict_case_count,
            "repairAttemptCount": repair_attempt_count,
            "auditTrailCount": audit_trail_count,
            "taskEventCount": task_event_count,
            "promptUsageCount": prompt_usage_count,
        }
    }))
    .into_response()
}

pub(super) async fn list_pm_provider_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsListQuery>,
) -> impl IntoResponse {
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT provider_key, channel,
                CAST(run_count AS INTEGER), CAST(success_count AS INTEGER), CAST(failure_count AS INTEGER),
                CAST(avg_latency_ms AS INTEGER), last_error_code, last_status, last_checked_at
         FROM pm_provider_health
         WHERE tenant_id = ?
         ORDER BY updated_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "providerKey": row.get::<String, _>(0),
                "channel": row.get::<String, _>(1),
                "runCount": row.get::<i64, _>(2),
                "successCount": row.get::<i64, _>(3),
                "failureCount": row.get::<i64, _>(4),
                "avgLatencyMs": row.get::<Option<i64>, _>(5),
                "lastErrorCode": row.get::<Option<String>, _>(6),
                "lastStatus": row.get::<String, _>(7),
                "lastCheckedAt": row.get::<Option<chrono::NaiveDateTime>, _>(8),
            })
        })
        .collect();
    Json(serde_json::json!({
        "rows": items,
        "total": items.len(),
        "scope": "lifetime_aggregate",
        "generatedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

pub(super) async fn list_pm_route_learning_features(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsPageQuery>,
) -> impl IntoResponse {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let limit = i64::from(per_page);
    let offset = i64::from(page.saturating_sub(1) * per_page);
    let total = match sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER)
         FROM pm_route_learning_features
         WHERE tenant_id = ?",
    )
    .bind(&claims.tenant_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(v) => v.max(0),
        Err(e) => return AppError::Database(e).into_response(),
    };
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT route_key, channel, CAST(total_runs AS INTEGER), CAST(success_runs AS INTEGER), CAST(failed_runs AS INTEGER),
                CAST(ema_quality AS DOUBLE), CAST(ema_latency_ms AS DOUBLE), CAST(ema_cost_usd AS DOUBLE), CAST(ema_success_rate AS DOUBLE), last_run_at
         FROM pm_route_learning_features
         WHERE tenant_id = ?
         ORDER BY updated_at DESC, id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "route": row.get::<String, _>(0),
                "channel": row.get::<Option<String>, _>(1),
                "totalRuns": row.get::<i64, _>(2),
                "successRuns": row.get::<i64, _>(3),
                "failedRuns": row.get::<i64, _>(4),
                "emaQuality": row.get::<f64, _>(5),
                "emaLatencyMs": row.get::<f64, _>(6),
                "emaCostUsd": row.get::<f64, _>(7),
                "emaSuccessRate": row.get::<f64, _>(8),
                "lastRunAt": row.get::<Option<chrono::NaiveDateTime>, _>(9),
            })
        })
        .collect();
    Json(serde_json::json!({
        "rows": items,
        "total": total,
        "page": page,
        "perPage": per_page,
    }))
    .into_response()
}

pub(super) async fn list_pm_prompt_registry(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsListQuery>,
) -> impl IntoResponse {
    let limit = i64::from(query.limit.unwrap_or(50).clamp(1, 200));
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT prompt_key, prompt_version, prompt_hash, stage, CAST(run_count AS INTEGER), last_run_id, last_used_at
         FROM pm_prompt_registry
         WHERE tenant_id = ?
         ORDER BY last_used_at DESC, updated_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "promptKey": row.get::<String, _>(0),
                "promptVersion": row.get::<String, _>(1),
                "promptHash": row.get::<String, _>(2),
                "stage": row.get::<Option<String>, _>(3),
                "runCount": row.get::<i64, _>(4),
                "lastRunId": row.get::<Option<String>, _>(5),
                "lastUsedAt": row.get::<Option<chrono::NaiveDateTime>, _>(6),
            })
        })
        .collect();
    Json(serde_json::json!({"rows": items, "total": items.len()})).into_response()
}

pub(super) async fn list_pm_audit_trails(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<PmOpsListQuery>,
) -> impl IntoResponse {
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let limit = i64::from(query.limit.unwrap_or(100).clamp(1, 500));
    let rows = match sqlx::query::<sqlx::Sqlite>(
        "SELECT run_id, event_type, severity, message, payload_json, created_at
         FROM pm_audit_trails
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?)
         ORDER BY id DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    {
        Ok(v) => v,
        Err(e) => return AppError::Database(e).into_response(),
    };
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            let payload = pm_row_json_value(&row, 4);
            serde_json::json!({
                "runId": row.get::<Option<String>, _>(0),
                "eventType": row.get::<String, _>(1),
                "severity": row.get::<String, _>(2),
                "message": row.get::<String, _>(3),
                "payload": payload,
                "createdAt": row.get::<chrono::NaiveDateTime, _>(5),
            })
        })
        .collect();
    Json(serde_json::json!({"rows": items, "total": items.len()})).into_response()
}
