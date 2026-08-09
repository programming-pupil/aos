//! Dashboard API — token usage statistics and billing data.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct TokenStats {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub estimated_cost_usd: f64,
    pub session_count: u64,
    pub total_requests: u64,
    pub active_model_count: u64,
}

#[derive(Debug, Serialize)]
pub struct DailyTokenStats {
    pub date: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ModelUsageStats {
    pub model: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct CacheStats {
    pub total_cache_creation_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub estimated_savings_usd: f64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct DashboardOverview {
    pub token_stats: TokenStats,
    pub cache_stats: CacheStats,
    pub top_models: Vec<ModelUsageStats>,
    pub daily_trend: Vec<DailyTokenStats>,
}

#[derive(Debug, Serialize)]
pub struct ModuleTokenUsageStats {
    pub module: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
    pub token_share_pct: f64,
}

#[derive(Debug, Serialize, Clone)]
pub struct DashboardConfigOverviewStats {
    pub enabled_api_key_count: u64,
    pub enabled_hook_count: u64,
    pub enabled_mcp_server_count: u64,
    pub active_user_count: u64,
}

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[expect(dead_code)]
    pub model: Option<String>,
    #[expect(dead_code)]
    pub user_id: Option<String>,
}

fn parse_date_range(start: Option<&String>, end: Option<&String>) -> (String, String) {
    let now = Utc::now().naive_utc();
    // API treats end_date as inclusive day bound; convert to exclusive upper bound.
    let midnight = |date: NaiveDate| date.and_hms_opt(0, 0, 0).unwrap_or(now);
    let end_dt = end
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .and_then(|d| d.checked_add_signed(chrono::Duration::days(1)))
        .map_or(now, midnight);
    let start_dt = start
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .map_or(end_dt - chrono::Duration::days(30), midnight);

    (
        start_dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
        end_dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
    )
}

#[allow(clippy::cast_sign_loss)]
async fn query_dashboard_config_overview_stats(
    state: &AppState,
    tenant_id: &str,
) -> Result<DashboardConfigOverviewStats> {
    let api_key_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM api_keys WHERE tenant_id = ? AND enabled = 1")
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await?;

    let hook_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tenant_hooks WHERE tenant_id = ? AND enabled = 1")
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await?;

    let mcp_server_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM mcp_server_registry WHERE tenant_id = ? AND enabled = 1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    let tenant_user_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM users WHERE tenant_id = ? AND is_active = 1")
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await?;

    Ok(DashboardConfigOverviewStats {
        enabled_api_key_count: api_key_count.0 as u64,
        enabled_hook_count: hook_count.0 as u64,
        enabled_mcp_server_count: mcp_server_count.0 as u64,
        active_user_count: tenant_user_count.0 as u64,
    })
}

#[allow(clippy::cast_precision_loss)]
fn estimate_cache_savings(cache_read: u64, cache_creation: u64) -> f64 {
    const INPUT_COST_PER_MILLION: f64 = 15.0;
    const CACHE_READ_COST_PER_MILLION: f64 = 1.5;
    const CACHE_CREATION_COST_PER_MILLION: f64 = 18.75;
    let without_cache_cost = (cache_read as f64 / 1_000_000.0) * INPUT_COST_PER_MILLION;
    let with_cache_cost = (cache_read as f64 / 1_000_000.0) * CACHE_READ_COST_PER_MILLION
        + (cache_creation as f64 / 1_000_000.0) * CACHE_CREATION_COST_PER_MILLION;
    (without_cache_cost - with_cache_cost).max(0.0)
}

#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
fn calculate_cache_hit_rate(creation: u64, read: u64) -> f64 {
    if creation == 0 && read == 0 {
        return 0.0;
    }
    let total = (read as f64) + (creation as f64);
    if total == 0.0 {
        return 0.0;
    }
    (read as f64 / total) * 100.0
}

#[allow(clippy::cast_sign_loss)]
pub async fn overview(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<DashboardOverview>> {
    let (start, end) = parse_date_range(query.start_date.as_ref(), query.end_date.as_ref());
    let tenant_id = &claims.tenant_id;

    let row = sqlx::query_as::<_, (i64, i64, i64, i64, f64, i64, i64, i64)>(
        "
        SELECT
            CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(cache_creation_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(cache_read_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(estimated_cost_usd), 0) AS DOUBLE),
            CAST(COUNT(DISTINCT session_id) AS INTEGER),
            CAST(COUNT(*) AS INTEGER),
            CAST(COUNT(DISTINCT NULLIF(model, '')) AS INTEGER)
        FROM token_usage
        WHERE tenant_id = ? AND created_at >= ? AND created_at < ?
        ",
    )
    .bind(tenant_id)
    .bind(&start)
    .bind(&end)
    .fetch_one(&state.db)
    .await?;

    let token_stats = TokenStats {
        total_input_tokens: row.0 as u64,
        total_output_tokens: row.1 as u64,
        total_cache_creation_tokens: row.2 as u64,
        total_cache_read_tokens: row.3 as u64,
        estimated_cost_usd: row.4,
        session_count: row.5 as u64,
        total_requests: row.6 as u64,
        active_model_count: row.7 as u64,
    };

    let cache_stats = CacheStats {
        total_cache_creation_tokens: row.2 as u64,
        total_cache_read_tokens: row.3 as u64,
        estimated_savings_usd: estimate_cache_savings(row.2 as u64, row.3 as u64),
        cache_hit_rate: calculate_cache_hit_rate(row.2 as u64, row.3 as u64),
    };

    let top_models = get_top_models(&state.db, tenant_id, &start, &end, 10).await?;
    let daily_trend = get_daily_trend(&state.db, tenant_id, &start, &end).await?;

    Ok(Json(DashboardOverview {
        token_stats,
        cache_stats,
        top_models,
        daily_trend,
    }))
}

#[allow(clippy::cast_sign_loss)]
async fn get_top_models(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    start: &str,
    end: &str,
    limit: i64,
) -> Result<Vec<ModelUsageStats>> {
    let rows = sqlx::query_as::<_, (String, i64, i64, i64, f64)>(
        "
        SELECT model, COUNT(*),
            CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(estimated_cost_usd), 0) AS DOUBLE)
        FROM token_usage
        WHERE tenant_id = ? AND created_at >= ? AND created_at < ?
        GROUP BY model
        ORDER BY SUM(estimated_cost_usd) DESC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(start)
    .bind(end)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(model, count, input, output, cost)| ModelUsageStats {
            model,
            request_count: count as u64,
            input_tokens: input as u64,
            output_tokens: output as u64,
            estimated_cost_usd: cost,
        })
        .collect())
}

#[allow(clippy::cast_sign_loss)]
async fn get_daily_trend(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    start: &str,
    end: &str,
) -> Result<Vec<DailyTokenStats>> {
    let rows = sqlx::query_as::<_, (String, i64, i64, i64, i64, f64)>(
        "
        SELECT
            strftime('%Y-%m-%d', created_at),
            CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(cache_creation_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(cache_read_tokens), 0) AS INTEGER),
            CAST(COALESCE(SUM(estimated_cost_usd), 0) AS DOUBLE)
        FROM token_usage
        WHERE tenant_id = ? AND created_at >= ? AND created_at < ?
        GROUP BY strftime('%Y-%m-%d', created_at)
        ORDER BY strftime('%Y-%m-%d', created_at)
        ",
    )
    .bind(tenant_id)
    .bind(start)
    .bind(end)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(date, input, output, cache_c, cache_r, cost)| DailyTokenStats {
                date,
                input_tokens: input as u64,
                output_tokens: output as u64,
                cache_creation_tokens: cache_c as u64,
                cache_read_tokens: cache_r as u64,
                estimated_cost_usd: cost,
            },
        )
        .collect())
}

pub async fn daily_trend(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<Vec<DailyTokenStats>>> {
    let (start, end) = parse_date_range(query.start_date.as_ref(), query.end_date.as_ref());
    Ok(Json(
        get_daily_trend(&state.db, &claims.tenant_id, &start, &end).await?,
    ))
}

pub async fn model_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<Vec<ModelUsageStats>>> {
    let (start, end) = parse_date_range(query.start_date.as_ref(), query.end_date.as_ref());
    Ok(Json(
        get_top_models(&state.db, &claims.tenant_id, &start, &end, 20).await?,
    ))
}

pub async fn config_overview_stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<DashboardConfigOverviewStats>> {
    Ok(Json(
        query_dashboard_config_overview_stats(&state, &claims.tenant_id).await?,
    ))
}

#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
pub async fn module_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<Vec<ModuleTokenUsageStats>>> {
    let (start, end) = parse_date_range(query.start_date.as_ref(), query.end_date.as_ref());
    let tenant_id = &claims.tenant_id;

    let rows = sqlx::query_as::<_, (String, i64, i64, i64, f64)>(
        "
        SELECT module_key,
               COUNT(*) AS request_count,
               CAST(COALESCE(SUM(input_tokens), 0) AS INTEGER) AS input_tokens,
               CAST(COALESCE(SUM(output_tokens), 0) AS INTEGER) AS output_tokens,
               CAST(COALESCE(SUM(estimated_cost_usd), 0) AS DOUBLE) AS estimated_cost_usd
        FROM (
            SELECT
                CASE
                    WHEN session_id LIKE 'nl2sql:%'
                      OR request_id LIKE 'nl2sql:%' THEN 'analytics'
                    WHEN session_id LIKE 'chat-adv-%'
                      OR request_id LIKE 'chat-adv-%' THEN 'adversarial'
                    WHEN session_id = 'pm-copilot'
                      OR session_id LIKE 'pm-%'
                      OR session_id LIKE 'pm:%'
                      OR request_id LIKE 'pm-%'
                      OR request_id LIKE 'pm:%'
                      OR source = 'pm' THEN 'operations'
                    WHEN session_id LIKE 'rd:%'
                      OR session_id LIKE 'rd-%'
                      OR request_id LIKE 'rd:%'
                      OR request_id LIKE 'rd-%'
                      OR source = 'rd' THEN 'engineering'
                    WHEN source = 'chat' THEN 'chat'
                    WHEN source = 'agent' THEN 'agent'
                    ELSE 'chat'
                END AS module_key,
                input_tokens,
                output_tokens,
                estimated_cost_usd
            FROM (
                SELECT tu.session_id,
                       tu.request_id,
                       tu.input_tokens,
                       tu.output_tokens,
                       tu.estimated_cost_usd,
                       ag.source
                FROM token_usage tu
                LEFT JOIN agent_sessions ag
                  ON ag.tenant_id = tu.tenant_id
                 AND ag.session_id = tu.session_id
                WHERE tu.tenant_id = ? AND tu.created_at >= ? AND tu.created_at < ?
            ) token_usage
        ) t
        GROUP BY module_key
        ",
    )
    .bind(tenant_id)
    .bind(&start)
    .bind(&end)
    .fetch_all(&state.db)
    .await?;

    let mut by_module: HashMap<String, ModuleTokenUsageStats> = HashMap::new();
    for (module, request_count, input_tokens, output_tokens, estimated_cost_usd) in rows {
        let input_tokens_u64 = input_tokens as u64;
        let output_tokens_u64 = output_tokens as u64;
        by_module.insert(
            module.clone(),
            ModuleTokenUsageStats {
                module,
                request_count: request_count as u64,
                input_tokens: input_tokens_u64,
                output_tokens: output_tokens_u64,
                total_tokens: input_tokens_u64 + output_tokens_u64,
                estimated_cost_usd,
                token_share_pct: 0.0,
            },
        );
    }

    let mut results = vec![
        by_module.remove("chat").unwrap_or(ModuleTokenUsageStats {
            module: "chat".to_string(),
            request_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            estimated_cost_usd: 0.0,
            token_share_pct: 0.0,
        }),
        by_module
            .remove("adversarial")
            .unwrap_or(ModuleTokenUsageStats {
                module: "adversarial".to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                token_share_pct: 0.0,
            }),
        by_module
            .remove("analytics")
            .unwrap_or(ModuleTokenUsageStats {
                module: "analytics".to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                token_share_pct: 0.0,
            }),
        by_module
            .remove("engineering")
            .unwrap_or(ModuleTokenUsageStats {
                module: "engineering".to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                token_share_pct: 0.0,
            }),
        by_module
            .remove("operations")
            .unwrap_or(ModuleTokenUsageStats {
                module: "operations".to_string(),
                request_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                token_share_pct: 0.0,
            }),
    ];
    results.extend(by_module.into_values());

    let all_tokens = results.iter().map(|x| x.total_tokens).sum::<u64>();
    for item in &mut results {
        item.token_share_pct = if all_tokens > 0 {
            (item.total_tokens as f64 / all_tokens as f64) * 100.0
        } else {
            0.0
        };
    }

    Ok(Json(results))
}

// ── Usage Alerts ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct UsageAlertInfo {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub alert_type: String,
    pub threshold_tokens: i64,
    pub threshold_usd: Option<f64>,
    pub enabled: bool,
    pub notified_at: Option<String>,
    pub created_at: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UsageAlertListResponse {
    pub alerts: Vec<UsageAlertInfo>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreateAlertRequest {
    pub name: String,
    pub alert_type: String,
    pub threshold_tokens: i64,
    pub threshold_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAlertRequest {
    pub name: Option<String>,
    pub alert_type: Option<String>,
    pub threshold_tokens: Option<i64>,
    pub threshold_usd: Option<f64>,
    pub enabled: Option<bool>,
}

impl
    From<(
        String,
        String,
        String,
        String,
        i64,
        Option<f64>,
        bool,
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
        Option<String>,
    )> for UsageAlertInfo
{
    #[allow(clippy::too_many_arguments)]
    fn from(
        (
            id,
            tenant_id,
            name,
            alert_type,
            threshold_tokens,
            threshold_usd,
            enabled,
            notified_at,
            created_at,
            created_by,
        ): (
            String,
            String,
            String,
            String,
            i64,
            Option<f64>,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
            Option<String>,
        ),
    ) -> Self {
        Self {
            id,
            tenant_id,
            name,
            alert_type,
            threshold_tokens,
            threshold_usd,
            enabled,
            notified_at: notified_at.map(|dt| dt.to_rfc3339()),
            created_at: created_at.to_rfc3339(),
            created_by,
        }
    }
}

async fn list_alerts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<UsageAlertListResponse>> {
    let tenant_id = &claims.tenant_id;
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            i64,
            Option<f64>,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
            Option<String>,
        ),
    >(
        "SELECT id, tenant_id, name, alert_type, threshold_tokens, \
         CAST(threshold_usd AS DOUBLE), enabled, \
         notified_at, created_at, created_by FROM usage_alerts \
         WHERE tenant_id = ? ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let alerts: Vec<UsageAlertInfo> = rows
        .into_iter()
        .map(
            |(
                id,
                tenant_id,
                name,
                alert_type,
                threshold_tokens,
                threshold_usd,
                enabled,
                notified_at,
                created_at,
                created_by,
            )| {
                UsageAlertInfo::from((
                    id,
                    tenant_id,
                    name,
                    alert_type,
                    threshold_tokens,
                    threshold_usd.map(|d| d.to_string().parse().unwrap_or(0.0)),
                    enabled,
                    notified_at,
                    created_at,
                    created_by,
                ))
            },
        )
        .collect();

    let total = alerts.len();
    Ok(Json(UsageAlertListResponse { alerts, total }))
}

async fn create_alert(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateAlertRequest>,
) -> Result<Json<UsageAlertInfo>> {
    let tenant_id = &claims.tenant_id;

    if req.name.trim().is_empty() {
        return Err(AppError::ValidationError(
            "alert name cannot be empty".into(),
        ));
    }
    let valid_types = ["daily_budget", "monthly_budget", "per_key_limit"];
    if !valid_types.contains(&req.alert_type.as_str()) {
        return Err(AppError::ValidationError(format!(
            "alert_type must be one of: {}",
            valid_types.join(", ")
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let threshold_usd: Option<f64> = req.threshold_usd;

    sqlx::query(
        "INSERT INTO usage_alerts (id, tenant_id, name, alert_type, threshold_tokens, threshold_usd, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.alert_type)
    .bind(req.threshold_tokens)
    .bind(threshold_usd)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            i64,
            Option<f64>,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
            Option<String>,
        ),
    >(
        "SELECT id, tenant_id, name, alert_type, threshold_tokens, \
         CAST(threshold_usd AS DOUBLE), enabled, \
         notified_at, created_at, created_by FROM usage_alerts WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(UsageAlertInfo::from((
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9,
    ))))
}

async fn update_alert(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAlertRequest>,
) -> Result<Json<UsageAlertInfo>> {
    let tenant_id = &claims.tenant_id;

    let mut updates: Vec<&str> = Vec::new();
    let mut bindings: Vec<String> = Vec::new();

    if let Some(ref name) = req.name {
        if name.trim().is_empty() {
            return Err(AppError::ValidationError(
                "alert name cannot be empty".into(),
            ));
        }
        updates.push("name = ?");
        bindings.push(name.clone());
    }
    if let Some(ref alert_type) = req.alert_type {
        let valid_types = ["daily_budget", "monthly_budget", "per_key_limit"];
        if !valid_types.contains(&alert_type.as_str()) {
            return Err(AppError::ValidationError("invalid alert_type".into()));
        }
        updates.push("alert_type = ?");
        bindings.push(alert_type.clone());
    }
    if let Some(tt) = req.threshold_tokens {
        updates.push("threshold_tokens = ?");
        bindings.push(tt.to_string());
    }
    if let Some(tu) = req.threshold_usd {
        let d: f64 = tu;
        updates.push("threshold_usd = ?");
        bindings.push(format!("{d:?}"));
    }
    if let Some(enabled) = req.enabled {
        updates.push("enabled = ?");
        bindings.push(if enabled { "1" } else { "0" }.to_string());
    }

    if !updates.is_empty() {
        let query = format!(
            "UPDATE usage_alerts SET {} WHERE id = ? AND tenant_id = ?",
            updates.join(", ")
        );
        let mut q = sqlx::query(&query);
        for b in &bindings {
            q = q.bind(b);
        }
        q = q.bind(&id).bind(tenant_id);
        q.execute(&state.db).await?;
    }

    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            i64,
            Option<f64>,
            bool,
            Option<chrono::DateTime<chrono::Utc>>,
            chrono::DateTime<chrono::Utc>,
            Option<String>,
        ),
    >(
        "SELECT id, tenant_id, name, alert_type, threshold_tokens, \
         CAST(threshold_usd AS DOUBLE), enabled, \
         notified_at, created_at, created_by FROM usage_alerts WHERE id = ? AND tenant_id = ?",
    )
    .bind(&id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("alert '{id}' not found")))?;

    Ok(Json(UsageAlertInfo::from((
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9,
    ))))
}

async fn delete_alert(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let tenant_id = &claims.tenant_id;
    let result = sqlx::query("DELETE FROM usage_alerts WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("alert '{id}' not found")));
    }
    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

pub fn routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/overview", routing_get(overview))
        .route("/config-overview-stats", routing_get(config_overview_stats))
        .route("/daily-trend", routing_get(daily_trend))
        .route("/model-usage", routing_get(model_usage))
        .route("/module-usage", routing_get(module_usage))
        .route("/alerts", routing_get(list_alerts))
        .route("/alerts", routing_post(create_alert))
        .route("/alerts/{id}", routing_patch(update_alert))
        .route("/alerts/{id}", routing_delete(delete_alert))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth_middleware::require_auth,
        ))
}
