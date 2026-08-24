use super::{
    upsert_nl2sql_conversation, validate_data_source_access, AgentExecuteRequest,
    AgentExecuteResponse,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::nl2sql::sql_safety::classify_sql;
use crate::routes::nl2sql::Nl2SqlAgent;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use sqlx::{Column, Row};
use std::sync::Arc;

const DEFAULT_MULTI_PAGE_SIZE: usize = 10;
const MAX_MULTI_PAGE_SIZE: usize = 200;
const STEP_PREVIEW_ROWS: usize = 20;
const FINAL_PREVIEW_ROWS: usize = 10;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct AgentResultPageQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentResultPageResponse {
    pub query_id: String,
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub page: u32,
    pub per_page: u32,
    pub total_rows: usize,
    pub has_more: bool,
}

/// POST /api/v1/nl2sql/agent/execute handler.
pub(crate) async fn agent_execute(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AgentExecuteRequest>,
) -> Result<Json<AgentExecuteResponse>> {
    Ok(Json(execute_agent_request(&state, &claims, req).await?))
}

pub(crate) async fn execute_agent_request(
    state: &AppState,
    claims: &Claims,
    req: AgentExecuteRequest,
) -> Result<AgentExecuteResponse> {
    execute_agent_request_with_budget(
        state,
        claims,
        req,
        super::agent_executor::DatasourceRequestBudget::new(3),
    )
    .await
}

/// Execute one NL2SQL request with a caller-owned datasource concurrency gate.
/// Complex orchestrators (attribution, bot tasks, etc.) pass the same gate to
/// every branch so their simultaneous database traffic stays bounded.
pub(crate) async fn execute_agent_request_with_budget(
    state: &AppState,
    claims: &Claims,
    req: AgentExecuteRequest,
    network_budget: Arc<super::agent_executor::DatasourceRequestBudget>,
) -> Result<AgentExecuteResponse> {
    super::require_nl2sql_embedding_config(state, &claims.tenant_id).await?;

    ensure_queryable_datasource_exists(state, claims).await?;

    // Validate access to every requested datasource.
    // If datasource_ids is empty, the agent will pick from all accessible datasources
    // (which are already tenant-scoped via the query in Nl2SqlAgent).
    for ds_id in &req.datasource_ids {
        validate_data_source_access(&state, &claims.tenant_id, &claims.sub, &claims.role, ds_id)
            .await?;
    }

    let conversation_id = req
        .conversation_id
        .clone()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("conv-{}", uuid::Uuid::new_v4()));
    let query_id = uuid::Uuid::new_v4().to_string();

    let agent = Nl2SqlAgent::with_network_budget(
        Arc::new(state.clone()),
        req.preferred_model.clone(),
        req.bounded,
        network_budget,
    );
    match agent
        .execute(
            &claims,
            &req.question,
            req.retrieval_question.as_deref(),
            req.shared_context.as_deref(),
            req.max_steps,
            &req.datasource_ids,
            &conversation_id,
            &query_id,
        )
        .await
    {
        Ok(mut resp) => {
            resp.conversation_id = Some(conversation_id.clone());
            resp.query_id = Some(query_id.clone());

            let full_columns = resp.final_result.columns.clone();
            let full_rows = resp.final_result.rows.clone();
            let total_rows = resp.final_result.row_count.max(full_rows.len());

            let generated_sql = {
                let parts = resp
                    .steps
                    .iter()
                    .filter_map(|step| {
                        step.sql.as_ref().map(|sql| {
                            format!(
                                "-- step {} [{}] datasource={} output={}\n{}",
                                step.step_id,
                                step.step_type,
                                step.datasource_id.as_deref().unwrap_or("N/A"),
                                step.output_name,
                                sql
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n\n"))
                }
            };

            let rows_returned = i64::try_from(resp.final_result.row_count).unwrap_or(i64::MAX);
            let total_ms = i64::try_from(resp.total_execution_ms).unwrap_or(i64::MAX);
            let executed = resp.error.is_none();
            let primary_datasource_id = resp.steps.iter().find_map(|step| {
                step.datasource_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
            });
            let routing_method = match resp.steps.first().map(|step| step.step_type.as_str()) {
                Some("federated_trino_query") => "federated_trino",
                Some("single_datasource_query") => {
                    if resp.used_references.is_empty() {
                        "single_datasource"
                    } else {
                        "sql_knowledge_single_datasource"
                    }
                }
                Some("error") if resp.used_references.is_empty() => "multi_step",
                Some("error") => "sql_knowledge_agent_error",
                _ => "multi_step",
            };

            if let Ok(rows_json) = serde_json::to_string(&full_rows) {
                let _ = sqlx::query(
                    "INSERT INTO nl2sql_agent_query_results
                     (query_id, tenant_id, user_id, conversation_id, columns_json, rows_json, total_rows)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(query_id) DO UPDATE SET
                       columns_json = excluded.columns_json,
                       rows_json = excluded.rows_json,
                       total_rows = excluded.total_rows,
                       updated_at = CURRENT_TIMESTAMP",
                )
                .bind(&query_id)
                .bind(&claims.tenant_id)
                .bind(&claims.sub)
                .bind(&conversation_id)
                .bind(serde_json::to_string(&full_columns).unwrap_or_else(|_| "[]".to_string()))
                .bind(rows_json)
                .bind(i64::try_from(total_rows).unwrap_or(i64::MAX))
                .execute(&state.db)
                .await;
            }

            let _ = sqlx::query(
                "INSERT INTO nl2sql_queries
                 (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, rows_returned, execution_ms, planning_ms, error_message, routing_method)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&query_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(primary_datasource_id.as_deref())
            .bind(&conversation_id)
            .bind(&req.question)
            .bind(generated_sql)
            .bind(executed)
            .bind(rows_returned)
            .bind(total_ms)
            .bind(total_ms)
            .bind(resp.error.clone())
            .bind(routing_method)
            .execute(&state.db)
            .await;

            upsert_nl2sql_conversation(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                &conversation_id,
                &req.question,
            )
            .await;

            let final_preview_rows = full_rows
                .into_iter()
                .take(FINAL_PREVIEW_ROWS)
                .collect::<Vec<_>>();
            resp.final_result.rows = final_preview_rows;
            resp.steps.iter_mut().for_each(|step| {
                if step.rows.len() > STEP_PREVIEW_ROWS {
                    step.rows.truncate(STEP_PREVIEW_ROWS);
                }
            });

            Ok(resp)
        }
        Err(e) => {
            let message = e.to_string();
            if message.contains("No accessible datasources found") {
                Err(AppError::ValidationError(
                    "[no_queryable_datasource] No queryable datasource is available. Configure one in Data Sources or ask an administrator for access. Connection details embedded in a Skill do not bypass AOS data permissions or auditing."
                        .to_string(),
                ))
            } else {
                Err(AppError::Internal(message))
            }
        }
    }
}

async fn ensure_queryable_datasource_exists(state: &AppState, claims: &Claims) -> Result<()> {
    const EXECUTABLE_TYPES: &str =
        "'mysql','tidb','postgres','clickhouse','presto','trino','mongodb'";
    let total_sql = format!(
        "SELECT COUNT(*) FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL AND db_type IN ({EXECUTABLE_TYPES})"
    );
    let total = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(total_sql))
        .bind(&claims.tenant_id)
        .fetch_one(&state.db)
        .await?;
    if total == 0 {
        return Err(AppError::ValidationError(
            "[no_datasource_configured] No queryable datasource is configured. Configure one in Data Sources before using NL2SQL. Connection details embedded in a Skill do not bypass AOS data permissions or auditing."
                .to_string(),
        ));
    }

    if claims.role == "admin" || claims.role == "superadmin" {
        return Ok(());
    }
    let accessible_sql = format!(
        "SELECT COUNT(*) FROM data_sources WHERE tenant_id = ? AND deleted_at IS NULL AND db_type IN ({EXECUTABLE_TYPES}) AND (visibility = 'tenant' OR user_id = ?)"
    );
    let accessible = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(accessible_sql))
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .fetch_one(&state.db)
        .await?;
    if accessible == 0 {
        return Err(AppError::ValidationError(
            "[no_datasource_access] No queryable datasource is available to your account. Ask an administrator to grant access. Connection details embedded in a Skill do not bypass AOS data permissions or auditing."
                .to_string(),
        ));
    }
    Ok(())
}

/// GET /api/v1/nl2sql/agent-results/{query_id}
pub(crate) async fn get_agent_result_page(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(query_id): Path<String>,
    Query(query): Query<AgentResultPageQuery>,
) -> Result<Json<AgentResultPageResponse>> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query
        .per_page
        .unwrap_or(DEFAULT_MULTI_PAGE_SIZE as u32)
        .clamp(1, MAX_MULTI_PAGE_SIZE as u32);
    let row = sqlx::query(
        "SELECT CAST(columns_json AS TEXT), rows_json, CAST(total_rows AS INTEGER)
         FROM nl2sql_agent_query_results
         WHERE tenant_id = ? AND user_id = ? AND query_id = ?
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&query_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound("agent result not found".to_string()));
    };
    let columns: Vec<String> = serde_json::from_str::<Vec<String>>(
        &row.get::<Option<String>, _>(0)
            .unwrap_or_else(|| "[]".to_string()),
    )
    .unwrap_or_default();
    let all_rows: Vec<serde_json::Value> =
        serde_json::from_str(&row.get::<String, _>(1)).unwrap_or_default();
    let total_rows = usize::try_from(row.get::<i64, _>(2)).unwrap_or(all_rows.len());
    let start = usize::try_from((page - 1) * per_page).unwrap_or(0);
    let end = start.saturating_add(usize::try_from(per_page).unwrap_or(DEFAULT_MULTI_PAGE_SIZE));
    let rows = if start >= all_rows.len() {
        Vec::new()
    } else {
        all_rows[start..all_rows.len().min(end)].to_vec()
    };
    let has_more = end < total_rows;
    Ok(Json(AgentResultPageResponse {
        query_id,
        columns,
        rows,
        page,
        per_page,
        total_rows,
        has_more,
    }))
}

// ── SQL safety ───────────────────────────────────────────────────────────────

/// Decode a single cell from a MySQL row into a JSON value. We try every
/// common type in decreasing specificity so numeric columns (including
/// `DECIMAL(p,s)` returned by `SUM`/`AVG`) round-trip correctly, rather
/// than falling through to `null` as they did when only `i64 / f64 /
/// String` were tried.
///
/// `DECIMAL` / `NEWDECIMAL` are preserved as JSON strings with full
/// precision — JS `Number` would silently round values like `0.79329000`
/// on the UI side.
fn decode_mysql_cell(row: &sqlx::mysql::MySqlRow, i: usize) -> serde_json::Value {
    use sqlx::TypeInfo;
    let col = &row.columns()[i];
    let ty = col.type_info().name();

    match ty {
        // Integer-ish
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
        | "BIGINT UNSIGNED" | "BIT" => row
            .try_get::<Option<u64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "FLOAT" | "DOUBLE" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        // DECIMAL / NUMERIC — keep full precision by rendering as a string.
        "DECIMAL" | "NUMERIC" | "NEWDECIMAL" => row
            .try_get::<Option<sqlx::types::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or_else(|| {
                row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
                    .ok()
                    .flatten()
                    .map(|v| serde_json::json!(v.to_rfc3339()))
                    .unwrap_or(serde_json::Value::Null)
            }),
        "BOOLEAN" => row
            .try_get::<Option<bool>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "JSON" => row
            .try_get::<Option<serde_json::Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" | "GEOMETRY" => {
            row.try_get::<Option<Vec<u8>>, _>(i)
                .ok()
                .flatten()
                .map(|v| serde_json::json!(format!("0x{}", hex::encode(v))))
                .unwrap_or(serde_json::Value::Null)
        }
        // VARCHAR, CHAR, TEXT, ENUM, SET, UUID, and anything else — treat as string.
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
    }
}

/// Decode a single cell from a PostgreSQL row into a JSON value. Uses the
/// same precision-preserving strategy as [`decode_mysql_cell`] for
/// `NUMERIC` / `DECIMAL`.
fn decode_postgres_cell(row: &sqlx::postgres::PgRow, i: usize) -> serde_json::Value {
    use sqlx::TypeInfo;
    let col = &row.columns()[i];
    let ty = col.type_info().name();

    match ty {
        "INT2" | "INT4" | "INT8" => row
            .try_get::<Option<i64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "FLOAT4" | "FLOAT8" => row
            .try_get::<Option<f64>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "NUMERIC" => row
            .try_get::<Option<sqlx::types::BigDecimal>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "BOOL" => row
            .try_get::<Option<bool>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_rfc3339()))
            .unwrap_or(serde_json::Value::Null),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        "JSON" | "JSONB" => row
            .try_get::<Option<serde_json::Value>, _>(i)
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null),
        "BYTEA" => row
            .try_get::<Option<Vec<u8>>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(format!("0x{}", hex::encode(v))))
            .unwrap_or(serde_json::Value::Null),
        _ => row
            .try_get::<Option<String>, _>(i)
            .ok()
            .flatten()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
    }
}

pub(crate) fn is_safe_sql(sql: &str) -> bool {
    use crate::routes::nl2sql::sql_safety::SqlSafetyResult;
    matches!(classify_sql(sql), SqlSafetyResult::Safe)
}
