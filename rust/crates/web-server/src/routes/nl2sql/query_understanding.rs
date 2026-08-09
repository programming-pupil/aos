use super::auth::require_admin;
use super::validate_data_source_access;
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};

// POST /nl2sql/query-understanding/:datasource_id
pub(crate) async fn query_understanding(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<QueryUnderstandingRequest>,
) -> Result<Json<QueryUnderstandingResponse>> {
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let schema_json = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT schema_info FROM data_sources WHERE id = ?",
    )
    .bind(&datasource_id)
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or_else(super::empty_schema_info);

    let chat_config = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::ValidationError(e.to_string()))?;

    let qu =
        crate::nl2sql::query_understanding::QueryUnderstanding::new(state.db.clone(), chat_config);

    let result = qu
        .understand(&req.question, &datasource_id, tenant_id, &schema_json)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(QueryUnderstandingResponse {
        rewritten_question: result.rewritten_question,
        intent: result.intent.to_string(),
        entities: result.entities,
        confidence: result.confidence,
    }))
}

// DELETE /nl2sql/query-understanding/:datasource_id/cache
pub(crate) async fn clear_qu_cache(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ClearQUCacheResponse>> {
    require_admin(&claims)?;
    let tenant_id = &claims.tenant_id;
    validate_data_source_access(&state, tenant_id, &claims.sub, &claims.role, &datasource_id)
        .await?;

    let result = sqlx::query(
        "DELETE FROM nl2sql_query_understanding_cache WHERE tenant_id = ? AND datasource_id = ?",
    )
    .bind(tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    Ok(Json(ClearQUCacheResponse {
        deleted: result.rows_affected(),
    }))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Request/Response DTOs
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaChangesQuery {
    status: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryUnderstandingRequest {
    question: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryUnderstandingResponse {
    rewritten_question: String,
    intent: String,
    entities: crate::nl2sql::query_understanding::QueryEntities,
    confidence: f32,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClearQUCacheResponse {
    deleted: u64,
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-1: Synonym Management Handlers
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SynonymItem {
    id: i64,
    term: String,
    canonical_table: String,
    canonical_column: String,
    term_type: String,
    created_by: Option<String>,
    created_at: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListSynonymsResponse {
    synonyms: Vec<SynonymItem>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSynonymRequest {
    term: String,
    canonical_table: String,
    canonical_column: String,
    #[serde(default)]
    term_type: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSynonymRequest {
    term: Option<String>,
    canonical_table: Option<String>,
    canonical_column: Option<String>,
    term_type: Option<String>,
}

// GET /nl2sql/synonyms/:datasource_id
