use super::PaginationParams;
use super::{
    AnalyticsDatasourceHealth, AnalyticsDatasourceHealthRow, AnalyticsOverview, AnalyticsRouting,
    AnalyticsRuleHitDaily, AnalyticsRuleHitItem, AnalyticsRuleHits, AnalyticsSemanticCoverage,
    AnalyticsTrends, DailyTrend, DatasourceCoverage,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Query, State};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! analytics_query {
    ($expr:expr, $fallback:expr, $ctx:literal) => {{
        match ($expr).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, context = $ctx, "analytics query failed, returning fallback");
                $fallback
            }
        }
    }};
}

const ANALYTICS_TOP_RULES_SQL: &str = "SELECT \
       COALESCE(NULLIF(COALESCE( \
         json_extract(jt.value, '$.rule_key'), \
         json_extract(jt.value, '$.ruleKey') \
       ), ''), 'unknown') AS rule_key, \
       COALESCE( \
         NULLIF(COALESCE( \
           json_extract(jt.value, '$.rule_name'), \
           json_extract(jt.value, '$.ruleName') \
         ), ''), \
         NULLIF(COALESCE( \
           json_extract(jt.value, '$.rule_key'), \
           json_extract(jt.value, '$.ruleKey') \
         ), ''), \
         'unknown' \
       ) AS rule_name, \
       COUNT(*) AS hits, \
       COUNT(DISTINCT q.id) AS queries \
     FROM nl2sql_queries q \
     JOIN json_each( \
       CASE WHEN json_valid(q.applied_rules_json) \
         THEN q.applied_rules_json ELSE '[]' END \
     ) jt \
     WHERE q.tenant_id = ? AND (? = 1 OR q.user_id = ?) AND q.deleted_at IS NULL \
       AND q.created_at >= ? AND q.created_at < ? \
     GROUP BY rule_key, rule_name \
     ORDER BY hits DESC, rule_key ASC \
     LIMIT 20";

#[derive(Debug, Deserialize)]
pub(crate) struct AnalyticsRangeQuery {
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SlowQueriesQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

impl SlowQueriesQuery {
    fn page(&self) -> i64 {
        i64::from(self.page.filter(|&p| p > 0).unwrap_or(1))
    }

    fn limit(&self) -> i64 {
        i64::from(self.per_page.filter(|&p| p > 0 && p <= 100).unwrap_or(20))
    }

    fn offset(&self) -> i64 {
        (self.page() - 1) * self.limit()
    }
}

fn parse_analytics_date_range(query: &AnalyticsRangeQuery) -> Result<(String, String)> {
    fn parse_date(label: &str, raw: &str) -> Result<NaiveDate> {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
            AppError::ValidationError(format!(
                "invalid {label}: {raw}. expected format is YYYY-MM-DD"
            ))
        })
    }

    let end_inclusive = match query.end_date.as_deref() {
        Some(raw) => parse_date("end_date", raw)?,
        None => Utc::now().date_naive(),
    };
    let start = match query.start_date.as_deref() {
        Some(raw) => parse_date("start_date", raw)?,
        None => end_inclusive - Duration::days(29),
    };

    if start > end_inclusive {
        return Err(AppError::ValidationError(format!(
            "start_date ({start}) must be <= end_date ({end_inclusive})"
        )));
    }

    let start_dt = start
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| AppError::ValidationError("failed to build start datetime".into()))?;
    let end_exclusive_dt = (end_inclusive + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| AppError::ValidationError("failed to build end datetime".into()))?;

    Ok((
        start_dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        end_exclusive_dt.format("%Y-%m-%d %H:%M:%S").to_string(),
    ))
}

fn first_table_from_semantic_context(ctx: &serde_json::Value) -> Option<String> {
    fn pick_from_item(item: &serde_json::Value) -> Option<String> {
        item.get("table_name")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("tableName").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    }

    if let Some(arr) = ctx.get("matched_tables").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(name) = pick_from_item(item) {
                return Some(name);
            }
        }
    }

    if let Some(arr) = ctx.get("matchedTables").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(name) = pick_from_item(item) {
                return Some(name);
            }
        }
    }

    if let Some(arr) = ctx.as_array() {
        for item in arr {
            if let Some(name) = pick_from_item(item) {
                return Some(name);
            }
        }
    }

    None
}

fn first_table_from_sql(sql: &str) -> Option<String> {
    super::extract_top_level_tables(sql).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::{first_table_from_semantic_context, percentile_offset, ANALYTICS_TOP_RULES_SQL};

    #[test]
    fn semantic_context_should_support_snake_and_camel_case() {
        let snake = serde_json::json!({
            "matched_tables": [{ "table_name": "users" }]
        });
        let camel = serde_json::json!({
            "matchedTables": [{ "tableName": "web_sessions" }]
        });
        let legacy = serde_json::json!([{ "tableName": "orders" }]);

        assert_eq!(
            first_table_from_semantic_context(&snake).as_deref(),
            Some("users")
        );
        assert_eq!(
            first_table_from_semantic_context(&camel).as_deref(),
            Some("web_sessions")
        );
        assert_eq!(
            first_table_from_semantic_context(&legacy).as_deref(),
            Some("orders")
        );
    }

    #[test]
    fn percentile_offsets_match_percent_rank_boundaries() {
        assert_eq!(percentile_offset(0, 50), 0);
        assert_eq!(percentile_offset(1, 99), 0);
        assert_eq!(percentile_offset(100, 50), 50);
        assert_eq!(percentile_offset(100, 95), 95);
        assert_eq!(percentile_offset(100, 99), 99);
        assert_eq!(percentile_offset(101, 50), 50);
    }

    #[tokio::test]
    async fn top_rule_hits_expand_sqlite_json_arrays() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect SQLite");
        sqlx::query(
            "CREATE TABLE nl2sql_queries ( \
               id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT, \
               applied_rules_json TEXT, created_at TEXT NOT NULL, deleted_at TEXT \
             )",
        )
        .execute(&pool)
        .await
        .expect("create query fixture table");
        for (id, rules) in [
            (
                "q1",
                r#"[{"rule_key":"tenant_scope","rule_name":"Tenant scope"},{"ruleKey":"limit","ruleName":"Limit"}]"#,
            ),
            (
                "q2",
                r#"[{"ruleKey":"tenant_scope","ruleName":"Tenant scope"}]"#,
            ),
            ("q3", "not-json"),
        ] {
            sqlx::query(
                "INSERT INTO nl2sql_queries \
                   (id, tenant_id, user_id, applied_rules_json, created_at) \
                 VALUES (?, 'tenant-a', 'user-a', ?, '2026-07-29 12:00:00')",
            )
            .bind(id)
            .bind(rules)
            .execute(&pool)
            .await
            .expect("insert query fixture");
        }

        let rows = sqlx::query_as::<_, (String, String, i64, i64)>(ANALYTICS_TOP_RULES_SQL)
            .bind("tenant-a")
            .bind(true)
            .bind("ignored-user")
            .bind("2026-07-29 00:00:00")
            .bind("2026-07-30 00:00:00")
            .fetch_all(&pool)
            .await
            .expect("aggregate SQLite rule hits");

        assert_eq!(
            rows,
            vec![
                ("tenant_scope".to_string(), "Tenant scope".to_string(), 2, 2,),
                ("limit".to_string(), "Limit".to_string(), 1, 1),
            ]
        );
    }
}

// GET /nl2sql/analytics/overview
pub(crate) async fn analytics_overview(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(range): Query<AnalyticsRangeQuery>,
) -> Result<Json<AnalyticsOverview>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let (start_at, end_at) = parse_analytics_date_range(&range)?;

    let total_queries: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_queries WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND created_at >= ? AND created_at < ?"
        ).bind(tenant_id).bind(tenant_wide).bind(&claims.sub).bind(&start_at).bind(&end_at).fetch_one(&state.db),
        (0,),
        "overview.total_queries"
    );

    let success_count: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND executed = 1 \
               AND (error_message IS NULL OR error_message = '')"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "overview.success_count"
    );

    let avg_confidence: (Option<f64>,) = analytics_query!(
        sqlx::query_as(
            "SELECT CAST(AVG(route_confidence) AS DOUBLE) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND route_confidence IS NOT NULL"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (None,),
        "overview.avg_confidence"
    );

    let avg_planning_ms: (Option<f64>,) = analytics_query!(
        sqlx::query_as(
            "SELECT CAST(AVG(planning_ms) AS DOUBLE) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND planning_ms IS NOT NULL"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (None,),
        "overview.avg_planning_ms"
    );

    let avg_execution_ms: (Option<f64>,) = analytics_query!(
        sqlx::query_as(
            "SELECT CAST(AVG(execution_ms) AS DOUBLE) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND execution_ms IS NOT NULL"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (None,),
        "overview.avg_execution_ms"
    );

    let cache_hit_queries: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND applied_rules_json IS NOT NULL \
               AND JSON_VALID(applied_rules_json) \
               AND EXISTS (SELECT 1 FROM json_each(applied_rules_json) \
                           WHERE value = 'result_cache_hit')"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "overview.cache_hit_queries"
    );

    let total_datasources: (i64,) = analytics_query!(
        sqlx::query_as("SELECT COUNT(*) FROM data_sources WHERE tenant_id = ? AND (? = 1 OR user_id IS NULL OR user_id = ?)")
            .bind(tenant_id)
            .bind(tenant_wide)
            .bind(&claims.sub)
            .fetch_one(&state.db),
        (0,),
        "overview.total_datasources"
    );

    let total_tables_indexed: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(DISTINCT (datasource_id || '/' || table_name)) FROM nl2sql_table_desc_semantics s \
             JOIN data_sources d ON d.id = s.datasource_id WHERE d.tenant_id = ? AND (? = 1 OR d.user_id IS NULL OR d.user_id = ?) AND s.deleted_at IS NULL AND s.ai_description IS NOT NULL AND s.ai_description != ''"
        ).bind(tenant_id).bind(tenant_wide).bind(&claims.sub).fetch_one(&state.db),
        (0,),
        "overview.total_tables_indexed"
    );

    let avg_semantic_coverage: (Option<f64>,) = analytics_query!(
        sqlx::query_as(
            "SELECT CAST(AVG(coverage) AS DOUBLE) FROM (
              SELECT CAST(COUNT(CASE WHEN s.ai_description IS NOT NULL AND s.ai_description != '' THEN 1 END) * 100.0 / NULLIF(COUNT(*), 0) AS DOUBLE) AS coverage
              FROM nl2sql_table_desc_semantics s
              JOIN data_sources d ON d.id = s.datasource_id
              WHERE d.tenant_id = ? AND (? = 1 OR d.user_id IS NULL OR d.user_id = ?) AND s.deleted_at IS NULL
              GROUP BY s.datasource_id
            ) t"
        ).bind(tenant_id).bind(tenant_wide).bind(&claims.sub).fetch_one(&state.db),
        (None,),
        "overview.avg_semantic_coverage"
    );

    let total_conversations: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_conversations WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND updated_at >= ? AND updated_at < ?"
        ).bind(tenant_id).bind(tenant_wide).bind(&claims.sub).bind(&start_at).bind(&end_at).fetch_one(&state.db),
        (0,),
        "overview.total_conversations"
    );

    let success_rate = if total_queries.0 > 0 {
        success_count.0 as f64 / total_queries.0 as f64 * 100.0
    } else {
        0.0
    };
    let avg_planning_ms_value = avg_planning_ms.0.unwrap_or(0.0);
    let avg_execution_ms_value = avg_execution_ms.0.unwrap_or(0.0);
    let planning_execution_ratio = if avg_execution_ms_value > 0.0 {
        avg_planning_ms_value / avg_execution_ms_value
    } else {
        0.0
    };
    let cache_hit_rate = if total_queries.0 > 0 {
        cache_hit_queries.0 as f64 / total_queries.0 as f64 * 100.0
    } else {
        0.0
    };

    Ok(Json(AnalyticsOverview {
        total_queries: total_queries.0,
        success_rate,
        avg_route_confidence: avg_confidence.0.unwrap_or(0.0),
        avg_planning_ms: avg_planning_ms_value,
        avg_execution_ms: avg_execution_ms_value,
        planning_execution_ratio,
        cache_hit_queries: cache_hit_queries.0,
        cache_hit_rate,
        total_datasources: total_datasources.0,
        total_tables_indexed: total_tables_indexed.0,
        avg_semantic_coverage: avg_semantic_coverage.0.unwrap_or(0.0),
        total_conversations: total_conversations.0,
    }))
}

// GET /nl2sql/analytics/routing
pub(crate) async fn analytics_routing(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(range): Query<AnalyticsRangeQuery>,
) -> Result<Json<AnalyticsRouting>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let (start_at, end_at) = parse_analytics_date_range(&range)?;

    // Confidence distribution buckets
    let dist_rows: Vec<(String, i64)> = analytics_query!(
        sqlx::query_as(
            "SELECT CASE \
               WHEN route_confidence IS NULL THEN 'unknown' \
               WHEN route_confidence >= 0.8 THEN '0.8-1.0' \
               WHEN route_confidence >= 0.6 THEN '0.6-0.8' \
               WHEN route_confidence >= 0.4 THEN '0.4-0.6' \
               WHEN route_confidence >= 0.2 THEN '0.2-0.4' \
               ELSE '0.0-0.2' END AS confidence_bucket, \
               COUNT(*) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND created_at >= ? AND created_at < ? \
             GROUP BY confidence_bucket \
             ORDER BY confidence_bucket",
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_all(&state.db),
        vec![],
        "routing.confidence_distribution"
    );

    let confidence_distribution: Vec<serde_json::Value> = dist_rows
        .into_iter()
        .map(|(range, count)| serde_json::json!({ "range": range, "count": count }))
        .collect();

    // Method distribution
    let method_rows: Vec<(String, i64)> = analytics_query!(
        sqlx::query_as(
            "SELECT COALESCE(NULLIF(routing_method, ''), 'unknown') AS method, COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND created_at >= ? AND created_at < ? \
             GROUP BY method",
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_all(&state.db),
        vec![],
        "routing.method_distribution"
    );

    let total_routed: i64 = method_rows.iter().map(|(_, c)| c).sum();
    let method_distribution: Vec<serde_json::Value> = method_rows
        .into_iter()
        .map(|(method, count)| serde_json::json!({
            "method": method,
            "count": count,
            "rate": if total_routed > 0 { count as f64 / total_routed as f64 * 100.0 } else { 0.0 }
        }))
        .collect();

    // Top routed tables (enterprise fallback chain):
    // 1) semantic_context.matched_tables[0].table_name (snake_case)
    // 2) semantic_context.matchedTables[0].tableName (camelCase)
    // 3) legacy array-style semantic_context[0].{table_name|tableName}
    // 4) parse FROM/JOIN table from generated_sql
    let top_table_rows: Vec<(Option<serde_json::Value>, Option<String>)> = analytics_query!(
        sqlx::query_as(
            "SELECT semantic_context, generated_sql \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ?",
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_all(&state.db),
        vec![],
        "routing.top_tables"
    );

    let mut table_counter: HashMap<String, i64> = HashMap::new();
    let mut unknown_rows = 0_i64;
    for (semantic_context, generated_sql) in top_table_rows {
        let from_semantic = semantic_context
            .as_ref()
            .and_then(first_table_from_semantic_context);
        let from_sql = generated_sql.as_deref().and_then(first_table_from_sql);
        let table = from_semantic.or(from_sql).filter(|s| !s.trim().is_empty());
        if let Some(table) = table {
            *table_counter.entry(table).or_insert(0) += 1;
        } else {
            // Keep track of non-attributable rows, but do not let "unknown"
            // dominate ranked table insights when valid hits exist.
            unknown_rows += 1;
        }
    }

    let mut top_pairs: Vec<(String, i64)> = table_counter.into_iter().collect();
    top_pairs.sort_by(|(ta, ca), (tb, cb)| cb.cmp(ca).then_with(|| ta.cmp(tb)));
    if top_pairs.is_empty() && unknown_rows > 0 {
        top_pairs.push(("unknown".to_string(), unknown_rows));
    }
    top_pairs.truncate(10);
    let top_routed_tables: Vec<serde_json::Value> = top_pairs
        .into_iter()
        .map(|(table, count)| serde_json::json!({ "table": table, "count": count }))
        .collect();

    // Clarification rate
    let clarification_count: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_clarification_messages \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND created_at >= ? AND created_at < ? \
               AND clarification_question IS NOT NULL AND clarification_question != ''",
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "routing.clarification_count"
    );

    let clarification_rate = if total_routed > 0 {
        clarification_count.0 as f64 / total_routed as f64 * 100.0
    } else {
        0.0
    };

    Ok(Json(AnalyticsRouting {
        confidence_distribution,
        method_distribution,
        top_routed_tables,
        clarification_rate,
    }))
}

// GET /nl2sql/analytics/semantic-coverage
pub(crate) async fn analytics_semantic_coverage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<AnalyticsSemanticCoverage>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();

    let rows: Vec<(String, String, i64, i64, i64, i64)> = sqlx::query_as(
        r#"SELECT
            ds.id,
            ds.name,
            COUNT(DISTINCT s.table_name) AS total_tables,
            COUNT(DISTINCT CASE WHEN s.ai_description IS NOT NULL AND s.ai_description != '' THEN s.table_name END) AS indexed_tables,
            COUNT(s.table_name) AS total_columns,
            COUNT(CASE WHEN s.ai_description IS NOT NULL AND s.ai_description != '' THEN 1 END) AS indexed_columns
          FROM data_sources ds
          LEFT JOIN nl2sql_table_desc_semantics s ON s.datasource_id = ds.id
          WHERE ds.tenant_id = ? AND (? = 1 OR ds.user_id IS NULL OR ds.user_id = ?)
          GROUP BY ds.id, ds.name"#
    ).bind(tenant_id).bind(tenant_wide).bind(&claims.sub).fetch_all(&state.db).await?;

    let datasources = rows
        .into_iter()
        .map(
            |(
                datasource_id,
                datasource_name,
                total_tables,
                indexed_tables,
                total_columns,
                indexed_columns,
            )| {
                let coverage_pct = if total_columns > 0 {
                    indexed_columns as f64 / total_columns as f64 * 100.0
                } else {
                    0.0
                };
                DatasourceCoverage {
                    datasource_id,
                    datasource_name,
                    total_tables,
                    indexed_tables,
                    total_columns,
                    indexed_columns,
                    coverage_pct,
                }
            },
        )
        .collect();

    Ok(Json(AnalyticsSemanticCoverage { datasources }))
}

// GET /nl2sql/analytics/trends
pub(crate) async fn analytics_trends(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(range): Query<AnalyticsRangeQuery>,
) -> Result<Json<AnalyticsTrends>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let (start_at, end_at) = parse_analytics_date_range(&range)?;

    let rows: Vec<(String, i64, i64, Option<f64>)> = sqlx::query_as(
        "SELECT \
           strftime('%Y-%m-%d', created_at) AS dt, \
           COUNT(*) AS total, \
           CAST(SUM(CASE WHEN executed = 1 AND (error_message IS NULL OR error_message = '') THEN 1 ELSE 0 END) AS INTEGER) AS success, \
           AVG(route_confidence) AS avg_conf \
         FROM nl2sql_queries \
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL AND created_at >= ? AND created_at < ? \
         GROUP BY dt \
         ORDER BY dt",
    )
    .bind(tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&start_at)
    .bind(&end_at)
    .fetch_all(&state.db)
    .await?;

    let daily = rows
        .into_iter()
        .map(|(date, queries, success, avg_confidence)| {
            let success_rate = if queries > 0 {
                success as f64 / queries as f64 * 100.0
            } else {
                0.0
            };
            DailyTrend {
                date,
                queries,
                success_rate,
                avg_confidence: avg_confidence.unwrap_or(0.0),
            }
        })
        .collect();

    Ok(Json(AnalyticsTrends { daily }))
}

// GET /nl2sql/analytics/rule-hits
pub(crate) async fn analytics_rule_hits(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(range): Query<AnalyticsRangeQuery>,
) -> Result<Json<AnalyticsRuleHits>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let (start_at, end_at) = parse_analytics_date_range(&range)?;

    let total_queries: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ?"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "rule_hits.total_queries"
    );

    let queries_with_rule_hits: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT COUNT(*) FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND json_type(applied_rules_json) = 'array' \
               AND json_array_length(applied_rules_json) > 0"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "rule_hits.queries_with_rule_hits"
    );

    let total_rule_hits: (i64,) = analytics_query!(
        sqlx::query_as(
            "SELECT CAST(COALESCE(SUM(json_array_length(applied_rules_json)), 0) AS INTEGER) \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
               AND json_type(applied_rules_json) = 'array'"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_one(&state.db),
        (0,),
        "rule_hits.total_rule_hits"
    );

    let top_rows: Vec<(String, String, i64, i64)> = analytics_query!(
        sqlx::query_as(ANALYTICS_TOP_RULES_SQL)
            .bind(tenant_id)
            .bind(tenant_wide)
            .bind(&claims.sub)
            .bind(&start_at)
            .bind(&end_at)
            .fetch_all(&state.db),
        vec![],
        "rule_hits.top_rules"
    );

    let top_rules = top_rows
        .into_iter()
        .map(
            |(rule_key, rule_name, hits, queries)| AnalyticsRuleHitItem {
                rule_key: if rule_key.is_empty() {
                    "unknown".to_string()
                } else {
                    rule_key
                },
                rule_name,
                hits,
                queries,
                query_hit_rate: if total_queries.0 > 0 {
                    (queries as f64 / total_queries.0 as f64) * 100.0
                } else {
                    0.0
                },
            },
        )
        .collect::<Vec<_>>();

    let daily_rows: Vec<(String, i64, i64, i64)> = analytics_query!(
        sqlx::query_as(
            "SELECT \
               strftime('%Y-%m-%d', created_at) AS dt, \
               COUNT(*) AS total_queries, \
               CAST(SUM(CASE \
                 WHEN json_type(applied_rules_json) = 'array' AND json_array_length(applied_rules_json) > 0 THEN 1 \
                 ELSE 0 END) AS INTEGER) AS queries_with_hits, \
               CAST(COALESCE(SUM(CASE \
                 WHEN json_type(applied_rules_json) = 'array' THEN json_array_length(applied_rules_json) \
                 ELSE 0 END), 0) AS INTEGER) AS total_hits \
             FROM nl2sql_queries \
             WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
               AND created_at >= ? AND created_at < ? \
             GROUP BY dt \
             ORDER BY dt"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_all(&state.db),
        vec![],
        "rule_hits.daily"
    );

    let daily = daily_rows
        .into_iter()
        .map(
            |(date, total_queries, queries_with_hits, total_hits)| AnalyticsRuleHitDaily {
                date,
                total_queries,
                queries_with_hits,
                coverage_rate: if total_queries > 0 {
                    (queries_with_hits as f64 / total_queries as f64) * 100.0
                } else {
                    0.0
                },
                total_hits,
            },
        )
        .collect::<Vec<_>>();

    let coverage_rate = if total_queries.0 > 0 {
        (queries_with_rule_hits.0 as f64 / total_queries.0 as f64) * 100.0
    } else {
        0.0
    };

    Ok(Json(AnalyticsRuleHits {
        total_queries: total_queries.0,
        queries_with_rule_hits: queries_with_rule_hits.0,
        coverage_rate,
        total_rule_hits: total_rule_hits.0,
        top_rules,
        daily,
    }))
}

// GET /nl2sql/analytics/datasource-health
pub(crate) async fn analytics_datasource_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(range): Query<AnalyticsRangeQuery>,
) -> Result<Json<AnalyticsDatasourceHealth>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let (start_at, end_at) = parse_analytics_date_range(&range)?;
    let limit: i64 = 100;

    let p95_rows: Vec<(String, Option<f64>)> = analytics_query!(
        sqlx::query_as(
            "SELECT dsid, MIN(execution_ms) AS p95_execution_ms
             FROM (
                SELECT
                    COALESCE(q.data_source_id, '') AS dsid,
                    CAST(q.execution_ms AS DOUBLE) AS execution_ms,
                    CUME_DIST() OVER (
                        PARTITION BY COALESCE(q.data_source_id, '')
                        ORDER BY q.execution_ms
                    ) AS cd
                FROM nl2sql_queries q
                WHERE q.tenant_id = ? AND (? = 1 OR q.user_id = ?)
                  AND q.deleted_at IS NULL
                  AND q.created_at >= ? AND q.created_at < ?
                  AND q.execution_ms IS NOT NULL
                  AND q.execution_ms > 0
             ) ranked
             WHERE cd >= 0.95
             GROUP BY dsid"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .fetch_all(&state.db),
        vec![],
        "datasource_health.p95"
    );
    let p95_map: HashMap<String, f64> = p95_rows
        .into_iter()
        .filter_map(|(dsid, p95)| p95.map(|v| (dsid, v)))
        .collect();

    let rows: Vec<(String, Option<String>, i64, i64, i64, Option<f64>)> = analytics_query!(
        sqlx::query_as(
            "SELECT
                COALESCE(q.data_source_id, '') AS datasource_id,
                MAX(ds.name) AS datasource_name,
                COUNT(*) AS total_queries,
                CAST(COALESCE(SUM(CASE
                    WHEN q.executed = 1 AND (q.error_message IS NULL OR q.error_message = '')
                    THEN 1 ELSE 0
                END), 0) AS INTEGER) AS successful_queries,
                CAST(COALESCE(SUM(CASE
                    WHEN q.executed = 1 AND q.error_message IS NOT NULL AND q.error_message <> ''
                    THEN 1 ELSE 0
                END), 0) AS INTEGER) AS failed_queries,
                CAST(AVG(CASE
                    WHEN q.execution_ms IS NOT NULL AND q.execution_ms > 0 THEN q.execution_ms
                    ELSE NULL
                END) AS DOUBLE) AS avg_execution_ms
             FROM nl2sql_queries q
             LEFT JOIN data_sources ds
               ON ds.id = q.data_source_id
              AND ds.tenant_id = q.tenant_id
             WHERE q.tenant_id = ? AND (? = 1 OR q.user_id = ?)
               AND q.deleted_at IS NULL
               AND q.created_at >= ? AND q.created_at < ?
             GROUP BY datasource_id
             ORDER BY total_queries DESC, datasource_id ASC
             LIMIT ?"
        )
        .bind(tenant_id)
        .bind(tenant_wide)
        .bind(&claims.sub)
        .bind(&start_at)
        .bind(&end_at)
        .bind(limit)
        .fetch_all(&state.db),
        vec![],
        "datasource_health.summary"
    );

    let items = rows
        .into_iter()
        .map(
            |(
                datasource_id,
                datasource_name,
                total_queries,
                successful_queries,
                failed_queries,
                avg_execution_ms,
            )| {
                let total = total_queries.max(0);
                let success = successful_queries.max(0);
                AnalyticsDatasourceHealthRow {
                    datasource_id: datasource_id.clone(),
                    datasource_name: datasource_name
                        .unwrap_or_else(|| "unknown".to_string())
                        .trim()
                        .to_string(),
                    total_queries: total,
                    successful_queries: success,
                    failed_queries: failed_queries.max(0),
                    success_rate: if total > 0 {
                        success as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    },
                    avg_execution_ms: avg_execution_ms.unwrap_or(0.0),
                    p95_execution_ms: p95_map.get(&datasource_id).copied(),
                }
            },
        )
        .collect::<Vec<_>>();

    Ok(Json(AnalyticsDatasourceHealth {
        total: i64::try_from(items.len()).unwrap_or(0),
        rows: items,
    }))
}

// GET /nl2sql/analytics/user-leaderboard — per-user query statistics for the leaderboard.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserLeaderboardEntry {
    pub user_id: String,
    pub total_queries: i64,
    pub successful_queries: i64,
    pub success_rate: f64,
    pub avg_execution_ms: Option<f64>,
    pub avg_confidence: Option<f64>,
    pub rank: i64,
}

#[derive(Debug, Serialize)]
pub struct UserLeaderboardResponse {
    pub items: Vec<UserLeaderboardEntry>,
    pub period_days: i64,
}

pub(crate) async fn analytics_user_leaderboard(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<UserLeaderboardResponse>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let period_days = params.per_page.unwrap_or(30).min(365) as i64;
    let offset = params.offset();
    let limit = params.limit().min(100);

    let rows: Vec<(String, i64, i64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT \
            user_id, \
            COUNT(*) AS total_queries, \
            CAST(SUM(CASE WHEN executed = 1 AND (error_message IS NULL OR error_message = '') THEN 1 ELSE 0 END) AS INTEGER) AS successful_queries, \
            CAST(AVG(execution_ms) AS DOUBLE) AS avg_exec_ms, \
            CAST(AVG(route_confidence) AS DOUBLE) AS avg_confidence \
         FROM nl2sql_queries \
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
           AND created_at >= datetime(CURRENT_TIMESTAMP, printf('-%d days', ?)) \
         GROUP BY user_id \
         ORDER BY total_queries DESC \
         LIMIT ? OFFSET ?",
    )
    .bind(tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(period_days)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let entries: Vec<UserLeaderboardEntry> = rows
        .into_iter()
        .enumerate()
        .map(
            |(i, (user_id, total, success, avg_exec_ms, avg_confidence))| {
                let success_rate = if total > 0 {
                    success as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                UserLeaderboardEntry {
                    user_id,
                    total_queries: total,
                    successful_queries: success,
                    success_rate: (success_rate * 100.0).round() / 100.0,
                    avg_execution_ms: avg_exec_ms,
                    avg_confidence: avg_confidence.map(|v| (v * 100.0).round() / 100.0),
                    rank: offset + (i as i64) + 1,
                }
            },
        )
        .collect();

    Ok(Json(UserLeaderboardResponse {
        items: entries,
        period_days,
    }))
}

// ── F-11: Query Performance Analysis ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlowQueryItem {
    pub id: String,
    pub question: String,
    pub data_source_id: String,
    pub generated_sql: Option<String>,
    pub execution_ms: i64,
    pub rows_returned: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SlowQueriesResponse {
    pub items: Vec<SlowQueryItem>,
    pub total: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

fn percentile_offset(total: i64, percentile: i64) -> i64 {
    if total <= 1 {
        return 0;
    }
    let numerator = (total - 1) * percentile.clamp(0, 100);
    numerator.saturating_add(99) / 100
}

async fn execution_percentile_ms(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    start_at: &str,
    end_at: &str,
    offset: i64,
) -> Result<Option<f64>> {
    let value = sqlx::query_scalar::<_, u64>(
        r#"SELECT execution_ms
           FROM nl2sql_queries
           WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL
             AND created_at >= ? AND created_at < ?
             AND execution_ms IS NOT NULL AND execution_ms > 0
           ORDER BY execution_ms ASC, created_at ASC, id ASC
           LIMIT 1 OFFSET ?"#,
    )
    .bind(&claims.tenant_id)
    .bind(claims.has_tenant_wide_monitoring_scope())
    .bind(&claims.sub)
    .bind(start_at)
    .bind(end_at)
    .bind(offset.max(0))
    .fetch_optional(db)
    .await?;
    Ok(value.map(|value| value as f64))
}

/// GET /api/v1/nl2sql/analytics/slow-queries — top-N slowest queries.
pub(crate) async fn slow_queries(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<SlowQueriesQuery>,
) -> Result<Json<SlowQueriesResponse>> {
    let tenant_id = &claims.tenant_id;
    let tenant_wide = claims.has_tenant_wide_monitoring_scope();
    let limit = params.limit().min(100);
    let offset = params.offset().max(0);
    let range = AnalyticsRangeQuery {
        start_date: params.start_date.clone(),
        end_date: params.end_date.clone(),
    };
    let (start_at, end_at) = parse_analytics_date_range(&range)?;

    let total: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM nl2sql_queries \
         WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL \
           AND created_at >= ? AND created_at < ? \
           AND execution_ms IS NOT NULL AND execution_ms > 0",
    )
    .bind(tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&start_at)
    .bind(&end_at)
    .fetch_one(&state.db)
    .await?;

    let rows: Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<u64>,
        Option<u64>,
        String,
    )> = sqlx::query_as(
        r#"SELECT q.id, q.question, q.data_source_id, q.generated_sql,
                      ranked.execution_ms, q.rows_returned,
                      strftime('%Y-%m-%d %H:%M:%S', ranked.created_at) as created_at
               FROM (
                 SELECT id, execution_ms, created_at
                 FROM nl2sql_queries
                 WHERE tenant_id = ? AND (? = 1 OR user_id = ?) AND deleted_at IS NULL
                   AND created_at >= ? AND created_at < ?
                   AND execution_ms IS NOT NULL AND execution_ms > 0
                 ORDER BY execution_ms DESC, created_at DESC, id DESC
                 LIMIT ? OFFSET ?
               ) ranked
               JOIN nl2sql_queries q ON q.id = ranked.id
               ORDER BY ranked.execution_ms DESC, ranked.created_at DESC, ranked.id DESC"#,
    )
    .bind(tenant_id)
    .bind(tenant_wide)
    .bind(&claims.sub)
    .bind(&start_at)
    .bind(&end_at)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let items: Vec<SlowQueryItem> = rows
        .into_iter()
        .map(
            |(
                id,
                question,
                data_source_id,
                generated_sql,
                execution_ms,
                rows_returned,
                created_at,
            )| {
                SlowQueryItem {
                    id,
                    question,
                    data_source_id: data_source_id.unwrap_or_default(),
                    generated_sql,
                    execution_ms: execution_ms
                        .and_then(|v| i64::try_from(v).ok())
                        .unwrap_or(0),
                    rows_returned: rows_returned.and_then(|v| i64::try_from(v).ok()),
                    created_at,
                }
            },
        )
        .collect();

    // Window-ranking every matching row forced the database to materialize and sort
    // the full range. Read the exact percentile positions from the ordered
    // covering index instead, keeping memory bounded regardless of history size.
    let percentiles = if total.0 > 0 {
        (
            execution_percentile_ms(
                &state.db,
                &claims,
                &start_at,
                &end_at,
                percentile_offset(total.0, 50),
            )
            .await?,
            execution_percentile_ms(
                &state.db,
                &claims,
                &start_at,
                &end_at,
                percentile_offset(total.0, 95),
            )
            .await?,
            execution_percentile_ms(
                &state.db,
                &claims,
                &start_at,
                &end_at,
                percentile_offset(total.0, 99),
            )
            .await?,
        )
    } else {
        (None, None, None)
    };

    Ok(Json(SlowQueriesResponse {
        total: usize::try_from(total.0).unwrap_or(0),
        items,
        p50_ms: percentiles.0,
        p95_ms: percentiles.1,
        p99_ms: percentiles.2,
    }))
}

// ── Query Policy Routes ─────────────────────────────────────────────────────────
