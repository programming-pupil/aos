use super::auth::require_admin;
use super::{
    validate_data_source_access, CreateSynonymRequest, PaginatedResponse, PaginationParams,
    SynonymItem, UpdateSynonymRequest,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;

fn normalize_term_type(input: &str) -> Option<&'static str> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" => Some("alias"),
        // canonical enum values
        "alias" => Some("alias"),
        "domain_term" => Some("domain_term"),
        "abbreviation" => Some("abbreviation"),
        "foreign_key_alias" => Some("foreign_key_alias"),
        // common english aliases
        "synonym" => Some("alias"),
        "slang" => Some("alias"),
        "abbr" => Some("abbreviation"),
        "domain term" => Some("domain_term"),
        "domain" => Some("domain_term"),
        "fk" => Some("foreign_key_alias"),
        "foreign key alias" => Some("foreign_key_alias"),
        // common chinese aliases (from CSV/UI)
        "同义词" => Some("alias"),
        "别名" => Some("alias"),
        "缩写" => Some("abbreviation"),
        "业务术语" => Some("domain_term"),
        "术语" => Some("domain_term"),
        "外键别名" => Some("foreign_key_alias"),
        "外键关联" => Some("foreign_key_alias"),
        _ => None,
    }
}

fn validate_term_type(input: &str) -> Result<&'static str> {
    normalize_term_type(input).ok_or_else(|| {
        AppError::ValidationError(
            "invalid term_type, expected one of: alias, domain_term, abbreviation, foreign_key_alias"
                .into(),
        )
    })
}

// GET /nl2sql/synonyms/:datasource_id
pub(crate) async fn list_synonyms(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<SynonymItem>>> {
    validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100).max(1);
    let offset = (page - 1) * per_page;

    let total: (i64,) = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT COUNT(*) FROM nl2sql_synonyms WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL"
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_one(&state.db)
    .await?;

    // Keep SQLite INTEGER decoding explicit for the typed tuple below.
    let rows: Vec<(i64, String, String, String, String, Option<String>, String)> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(s.id AS INTEGER), s.term, s.canonical_table, s.canonical_column, s.term_type, \
         COALESCE(NULLIF(u.name, ''), u.email, s.created_by) AS created_by, \
         strftime('%Y-%m-%d %H:%M:%S', s.created_at) \
         FROM nl2sql_synonyms s \
         LEFT JOIN users u ON s.created_by = u.id AND u.tenant_id = s.tenant_id \
         WHERE s.tenant_id = ? AND s.datasource_id = ? AND s.deleted_at IS NULL \
         ORDER BY term LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let synonyms: Vec<SynonymItem> = rows
        .into_iter()
        .map(
            |(id, term, canonical_table, canonical_column, term_type, created_by, created_at)| {
                SynonymItem {
                    id,
                    term,
                    canonical_table,
                    canonical_column,
                    term_type,
                    created_by,
                    created_at,
                }
            },
        )
        .collect();

    Ok(Json(PaginatedResponse::new(
        synonyms, total.0, page, per_page,
    )))
}

// POST /nl2sql/synonyms/:datasource_id
pub(crate) async fn create_synonym(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateSynonymRequest>,
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

    let term_type = validate_term_type(&req.term_type)?;

    let result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_synonyms (tenant_id, datasource_id, term, canonical_table, canonical_column, term_type, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&req.term)
    .bind(&req.canonical_table)
    .bind(&req.canonical_column)
    .bind(term_type)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "id": result.last_insert_rowid() }),
    ))
}

// POST /nl2sql/synonyms/:datasource_id/bulk — bulk-create synonyms from CSV import.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BulkCreateSynonymRequest {
    synonyms: Vec<CreateSynonymRequest>,
}
pub(crate) async fn bulk_create_synonyms(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<BulkCreateSynonymRequest>,
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

    // Atomic bulk insert: all-or-nothing so a mid-loop failure does not leave the synonym set
    // in a partially-applied state that confuses downstream RAG lookups.
    let mut tx = state.db.begin().await?;
    let mut created = 0usize;
    let mut skipped = 0usize;
    for syn in &req.synonyms {
        let term_type = validate_term_type(&syn.term_type)?;
        let result = sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_synonyms (tenant_id, datasource_id, term, canonical_table, canonical_column, term_type, created_by) \
             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO UPDATE SET \
             canonical_table = excluded.canonical_table, canonical_column = excluded.canonical_column, term_type = excluded.term_type",
        )
        .bind(&claims.tenant_id)
        .bind(&datasource_id)
        .bind(&syn.term)
        .bind(&syn.canonical_table)
        .bind(&syn.canonical_column)
        .bind(term_type)
        .bind(&claims.sub)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() > 0 {
            created += 1;
        } else {
            skipped += 1;
        }
    }
    tx.commit().await?;

    Ok(Json(
        serde_json::json!({ "created": created, "skipped": skipped }),
    ))
}

// PATCH /nl2sql/synonyms/:datasource_id/:id
pub(crate) async fn update_synonym(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, synonym_id)): Path<(String, String)>,
    Json(req): Json<UpdateSynonymRequest>,
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

    let synonym_id = synonym_id
        .parse::<u64>()
        .map_err(|_| AppError::ValidationError("invalid synonym id".into()))?;

    if req.term.is_none()
        && req.canonical_table.is_none()
        && req.canonical_column.is_none()
        && req.term_type.is_none()
    {
        return Err(AppError::ValidationError("No fields to update".into()));
    }

    let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new("UPDATE nl2sql_synonyms SET ");

    let mut needs_comma = false;
    if let Some(ref v) = req.term {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("term = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.canonical_table {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("canonical_table = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.canonical_column {
        if needs_comma {
            qb.push(", ");
        }
        qb.push("canonical_column = ");
        qb.push_bind(v);
        needs_comma = true;
    }
    if let Some(ref v) = req.term_type {
        let normalized = validate_term_type(v)?;
        if needs_comma {
            qb.push(", ");
        }
        qb.push("term_type = ");
        qb.push_bind(normalized);
    }

    qb.push(" WHERE id = ");
    qb.push_bind(crate::sqlite_i64(synonym_id));
    qb.push(" AND tenant_id = ");
    qb.push_bind(&claims.tenant_id);
    qb.push(" AND datasource_id = ");
    qb.push_bind(&datasource_id);

    let result = qb.build().execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "synonym not found or access denied".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "updated": true })))
}

// DELETE /nl2sql/synonyms/:datasource_id/:id
pub(crate) async fn delete_synonym(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, synonym_id)): Path<(String, String)>,
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

    sqlx::query::<sqlx::Sqlite>("DELETE FROM nl2sql_synonyms WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL")
        .bind(
            synonym_id
                .parse::<i64>()
                .map_err(|_| AppError::ValidationError("invalid synonym id".into()))?,
        )
        .bind(&claims.tenant_id)
        .bind(&datasource_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

// ══════════════════════════════════════════════════════════════════════════════
// P1-2: Metrics / Measure Semantic Layer
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetricItem {
    id: i64,
    metric_name: String,
    metric_aliases: serde_json::Value,
    expression: String,
    filter_conditions: Option<serde_json::Value>,
    description: Option<String>,
    granularity: String,
    created_by: Option<String>,
    created_at: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListMetricsResponse {
    metrics: Vec<MetricItem>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateMetricRequest {
    metric_name: String,
    #[serde(default)]
    metric_aliases: serde_json::Value,
    expression: String,
    #[serde(default)]
    filter_conditions: Option<serde_json::Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_granularity")]
    granularity: String,
}
fn default_granularity() -> String {
    "day".to_string()
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMetricRequest {
    metric_name: Option<String>,
    metric_aliases: Option<serde_json::Value>,
    expression: Option<String>,
    filter_conditions: Option<serde_json::Value>,
    description: Option<String>,
    granularity: Option<String>,
}

// GET /nl2sql/metrics/:datasource_id
