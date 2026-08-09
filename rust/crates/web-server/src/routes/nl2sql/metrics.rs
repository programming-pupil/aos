use super::auth::require_admin;
use super::{
    validate_data_source_access, CreateMetricRequest, ListMetricsResponse, MetricItem,
    UpdateMetricRequest,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::{Deserialize, Serialize};

// GET /nl2sql/metrics/:datasource_id
pub(crate) async fn list_metrics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ListMetricsResponse>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    // Try with status column (migration 008+); fall back for older schemas.
    let rows_with_status: sqlx::Result<Vec<(i64, String, serde_json::Value, String, Option<serde_json::Value>, Option<String>, String, Option<String>, String, Option<String>)>> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(m.id AS INTEGER), m.metric_name, m.metric_aliases, m.expression, m.filter_conditions, m.description, m.granularity, \
         COALESCE(NULLIF(u.name, ''), NULLIF(u.email, ''), m.created_by) AS created_by, \
         strftime('%Y-%m-%d %H:%M:%S', m.created_at), m.status \
         FROM nl2sql_metrics m \
         LEFT JOIN users u ON m.created_by = u.id AND u.tenant_id = m.tenant_id \
         WHERE m.tenant_id = ? AND m.datasource_id = ? AND m.deleted_at IS NULL ORDER BY m.metric_name",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await;

    let rows: Vec<(i64, String, serde_json::Value, String, Option<serde_json::Value>, Option<String>, String, Option<String>, String, Option<String>)> = match rows_with_status {
        Ok(r) => r,
        Err(_) => {
            sqlx::query_as::<sqlx::Sqlite, _>(
                "SELECT CAST(m.id AS INTEGER), m.metric_name, m.metric_aliases, m.expression, m.filter_conditions, m.description, m.granularity, \
                 COALESCE(NULLIF(u.name, ''), NULLIF(u.email, ''), m.created_by) AS created_by, \
                 strftime('%Y-%m-%d %H:%M:%S', m.created_at), NULL \
                 FROM nl2sql_metrics m \
                 LEFT JOIN users u ON m.created_by = u.id AND u.tenant_id = m.tenant_id \
                 WHERE m.tenant_id = ? AND m.datasource_id = ? AND m.deleted_at IS NULL ORDER BY m.metric_name",
            )
            .bind(&claims.tenant_id)
            .bind(&datasource_id)
            .fetch_all(&state.db)
            .await?
        }
    };

    let metrics = rows
        .into_iter()
        .map(
            |(
                id,
                metric_name,
                metric_aliases,
                expression,
                filter_conditions,
                description,
                granularity,
                created_by,
                created_at,
                status,
            )| {
                MetricItem {
                    id,
                    metric_name,
                    metric_aliases,
                    expression,
                    filter_conditions,
                    description,
                    granularity,
                    created_by,
                    created_at,
                    status,
                }
            },
        )
        .collect();

    Ok(Json(ListMetricsResponse { metrics }))
}

// POST /nl2sql/metrics/:datasource_id
pub(crate) async fn create_metric(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateMetricRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_metrics (tenant_id, datasource_id, metric_name, metric_aliases, expression, \
         filter_conditions, description, granularity, created_by) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&req.metric_name)
    .bind(&req.metric_aliases)
    .bind(&req.expression)
    .bind(&req.filter_conditions)
    .bind(&req.description)
    .bind(&req.granularity)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "id": result.last_insert_rowid() }),
    ))
}

// PATCH /nl2sql/metrics/:datasource_id/:id
pub(crate) async fn update_metric(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, metric_id)): Path<(String, String)>,
    Json(req): Json<UpdateMetricRequest>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    if req.metric_name.is_none()
        && req.metric_aliases.is_none()
        && req.expression.is_none()
        && req.filter_conditions.is_none()
        && req.description.is_none()
        && req.granularity.is_none()
    {
        return Err(AppError::ValidationError("No fields to update".into()));
    }

    let mut qb: sqlx::query_builder::QueryBuilder<sqlx::Sqlite> =
        sqlx::query_builder::QueryBuilder::new("UPDATE nl2sql_metrics SET ");
    let mut has_assignment = false;

    if let Some(ref v) = req.metric_name {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("metric_name = ");
        qb.push_bind(v);
        has_assignment = true;
    }
    if let Some(ref v) = req.metric_aliases {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("metric_aliases = ");
        qb.push_bind(serde_json::to_string(v).unwrap_or_default());
        has_assignment = true;
    }
    if let Some(ref v) = req.expression {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("expression = ");
        qb.push_bind(v);
        has_assignment = true;
    }
    if let Some(ref v) = req.filter_conditions {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("filter_conditions = ");
        qb.push_bind(serde_json::to_string(v).unwrap_or_default());
        has_assignment = true;
    }
    if let Some(ref v) = req.description {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("description = ");
        qb.push_bind(v);
        has_assignment = true;
    }
    if let Some(ref v) = req.granularity {
        if has_assignment {
            qb.push(", ");
        }
        qb.push("granularity = ");
        qb.push_bind(v);
        has_assignment = true;
    }

    if has_assignment {
        qb.push(", ");
    }
    qb.push("updated_at = CURRENT_TIMESTAMP");

    qb.push(" WHERE id = ");
    qb.push_bind(
        metric_id
            .parse::<i64>()
            .map_err(|_| AppError::ValidationError("invalid metric id".into()))?,
    );
    qb.push(" AND tenant_id = ");
    qb.push_bind(&claims.tenant_id);
    qb.push(" AND datasource_id = ");
    qb.push_bind(&datasource_id);

    qb.build().execute(&state.db).await?;

    Ok(Json(serde_json::json!({ "updated": true })))
}

// DELETE /nl2sql/metrics/:datasource_id/:id
pub(crate) async fn delete_metric(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, metric_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    require_admin(&claims)?;
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_metrics WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL")
        .bind(
            metric_id
                .parse::<i64>()
                .map_err(|_| AppError::ValidationError("invalid metric id".into()))?,
        )
        .bind(&claims.tenant_id)
        .bind(&datasource_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
pub(crate) struct MetricStatusRequest {
    pub action: String, // "submit_review" | "approve" | "reject" | "deprecate" | "restore"
    #[serde(default)]
    pub comment: Option<String>,
}

// POST /nl2sql/metrics/:datasource_id/:metric_id/status
pub(crate) async fn update_metric_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, metric_id)): Path<(String, u64)>,
    Json(req): Json<MetricStatusRequest>,
) -> Result<Json<serde_json::Value>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let allowed_actions = ["submit_review", "approve", "reject", "deprecate", "restore"];
    if !allowed_actions.contains(&req.action.as_str()) {
        return Err(AppError::ValidationError(format!(
            "invalid action '{}'",
            req.action
        )));
    }

    // Approve/reject require admin.
    if matches!(req.action.as_str(), "approve" | "reject" | "deprecate") {
        require_admin(&claims)?;
    }

    let (from_status, to_status) = match req.action.as_str() {
        "submit_review" => ("draft", "review"),
        "approve" => ("review", "published"),
        "reject" => ("review", "draft"),
        "deprecate" => ("published", "deprecated"),
        "restore" => ("deprecated", "draft"),
        _ => unreachable!(),
    };

    let affected = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_metrics SET status = ?, approved_by = IIF(? = 'approve', ?, approved_by), \
         approved_at = IIF(? = 'approve', CURRENT_TIMESTAMP, approved_at) \
         WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND status = ? AND deleted_at IS NULL",
    )
    .bind(to_status)
    .bind(&req.action)
    .bind(&claims.sub)
    .bind(&req.action)
    .bind(crate::sqlite_i64(metric_id))
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(from_status)
    .execute(&state.db)
    .await?
    .rows_affected();

    if affected == 0 {
        return Err(AppError::ValidationError(format!(
            "metric not found or status transition '{from_status}' → '{to_status}' not allowed"
        )));
    }

    Ok(Json(serde_json::json!({ "status": to_status })))
}

// GET /nl2sql/metrics/:datasource_id/lookup?question=...
pub(crate) async fn metric_lookup(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let question = params
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();

    // Empty question must not match every metric. Earlier this code returned all metrics
    // because `"".contains("")` is always true — turning a no-op lookup into a full table dump
    // and silently degrading routing precision. Reject the call instead.
    if question.is_empty() {
        return Ok(Json(serde_json::json!({ "matches": [] })));
    }

    let rows: Vec<(i64, String, serde_json::Value, String, Option<serde_json::Value>, Option<String>, String)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(id AS INTEGER), metric_name, metric_aliases, expression, filter_conditions, description, granularity \
         FROM nl2sql_metrics WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    // Simple fuzzy match: check if any alias or metric_name is contained in the question (case-insensitive).
    let question_lower = question.to_lowercase();
    let matches: Vec<_> = rows
        .into_iter()
        .filter(
            |(_id, name, aliases, _expression, _filter_conditions, _description, _granularity)| {
                if name.to_lowercase().contains(&question_lower)
                    || question_lower.contains(&name.to_lowercase())
                {
                    return true;
                }
                if let Some(arr) = aliases.as_array() {
                    for alias in arr {
                        if let Some(s) = alias.as_str() {
                            let al = s.to_lowercase();
                            if al.contains(&question_lower) || question_lower.contains(&al) {
                                return true;
                            }
                        }
                    }
                }
                false
            },
        )
        .map(
            |(
                id,
                metric_name,
                metric_aliases,
                expression,
                filter_conditions,
                description,
                granularity,
            )| {
                serde_json::json!({
                    "id": id,
                    "metric_name": metric_name,
                    "metric_aliases": metric_aliases,
                    "expression": expression,
                    "filter_conditions": filter_conditions,
                    "description": description,
                    "granularity": granularity,
                })
            },
        )
        .collect();

    Ok(Json(serde_json::json!({ "matches": matches })))
}

// ══════════════════════════════════════════════════════════════════════════════
// P1-3: Join Path Management
// ══════════════════════════════════════════════════════════════════════════════

/// P1-3: A JOIN path entry returned by GET /nl2sql/join-paths/:datasource_id.
/// Aligned with the frontend JoinPathItem type:
///   path[]      — table names parsed from path_text (traversal order)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinPathItem {
    id: i64,
    /// Ordered list of table names in the join traversal.
    path: Vec<String>,
    /// Ordered list of FK column names (pairs: source_col, target_col per hop).
    join_columns: Vec<String>,
    /// Datasource IDs for cross-datasource paths; empty for within-ds paths.
    ds_ids: Vec<String>,
    /// Number of JOIN hops (edges) in this path.
    total_columns: i32,
    verified: bool,
    confidence: f32,
    /// How this path was discovered: 'auto' | 'manual' | 'cross_ds'
    source: String,
    created_at: String,
    /// Raw DB fields for CRUD display/edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    join_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sql_joins: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

impl JoinPathItem {
    /// Parse a `path_text` string like:
    ///   "orders.customer_id → customers.id, customers.region_id → regions.id (2 hops)"
    /// into `path` (table names) and `join_columns` (FK column names).
    fn from_path_text(
        id: i64,
        path_text: &str,
        hops: i32,
        verified: bool,
        confidence: f32,
        source: &str,
        created_at: String,
        source_table: Option<String>,
        target_table: Option<String>,
        source_column: Option<String>,
        target_column: Option<String>,
        join_type: Option<String>,
        notes: Option<String>,
    ) -> Self {
        let _notes = notes;
        let mut path: Vec<String> = Vec::new();
        let mut join_columns: Vec<String> = Vec::new();

        // Split on "→" to extract each leg of the path.
        for leg in path_text.split('→') {
            let leg = leg
                .trim()
                .trim_end_matches(|c: char| c == '(' || c.is_numeric() || c == ' ' || c == ')');
            // Each leg looks like: "orders.customer_id" or "orders.customer_id, ..."
            for part in leg.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                // Format: "table_name.column_name"
                if let Some(dot) = part.rfind('.') {
                    let table = part[..dot].trim().to_string();
                    let col = part[dot + 1..].trim().to_string();
                    if !table.is_empty() && !path.contains(&table) {
                        path.push(table);
                    }
                    if !col.is_empty() {
                        join_columns.push(col);
                    }
                }
            }
        }

        // Deduplicate path tables (multi-hop paths may reference same table twice).
        let mut seen = std::collections::HashSet::new();
        path.retain(|t| seen.insert(t.clone()));

        Self {
            id,
            path,
            join_columns,
            ds_ids: Vec::new(),
            total_columns: hops,
            verified,
            confidence,
            source: source.to_string(),
            created_at,
            source_table,
            target_table,
            source_column,
            target_column,
            join_type,
            path_text: Some(path_text.to_string()),
            sql_joins: None,
            notes: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListJoinPathsResponse {
    paths: Vec<JoinPathItem>,
}

// GET /nl2sql/join-paths/:datasource_id
