use super::{PaginationParams, SaveViewRequest, SavedView, SavedViewsResponse};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::Deserialize;
use sqlx::Row;

/// GET /api/v1/nl2sql/views — list saved views for the current user.
pub(crate) async fn views(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<SavedViewsResponse>> {
    let offset = params.offset();
    let limit = params.limit();

    #[derive(sqlx::FromRow)]
    struct SavedViewRow {
        id: String,
        data_source_id: Option<String>,
        conversation_id: Option<String>,
        generated_sql: String,
        saved_view_name: Option<String>,
        saved_view_description: Option<String>,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    let rows: Vec<SavedViewRow> = sqlx::query_as(
        "SELECT id, data_source_id, conversation_id, generated_sql, saved_view_name, saved_view_description, created_at \
         FROM nl2sql_queries \
         WHERE tenant_id = ? AND user_id = ? AND saved_view_name IS NOT NULL AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let views: Vec<SavedView> = rows
        .into_iter()
        .map(|row| SavedView {
            query_id: row.id,
            data_source_id: row.data_source_id,
            conversation_id: row.conversation_id,
            sql: row.generated_sql,
            name: row.saved_view_name.unwrap_or_default(),
            description: row.saved_view_description,
            created_at: row.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(SavedViewsResponse { views }))
}

/// POST /api/v1/nl2sql/views — save a query as a named view.
pub(crate) async fn save_view(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SaveViewRequest>,
) -> Result<Json<SavedView>> {
    let rows = sqlx::query(
        "SELECT id, data_source_id, conversation_id, generated_sql, saved_view_name, saved_view_description, created_at \
         FROM nl2sql_queries \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND generated_sql IS NOT NULL AND deleted_at IS NULL",
    )
    .bind(&req.query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("query not found".into()))?;

    let existing_desc: Option<String> = rows.get("saved_view_description");
    let existing_ds_id: Option<String> = rows.get("data_source_id");
    let existing_conversation_id: Option<String> = rows.get("conversation_id");
    let existing_sql: String = rows.get("generated_sql");
    let existing_created: chrono::DateTime<chrono::Utc> = rows.get("created_at");

    sqlx::query(
        "UPDATE nl2sql_queries \
         SET saved_view_name = ?, saved_view_description = ?, conversation_id = COALESCE(?, conversation_id) \
         WHERE id = ?",
    )
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.conversation_id)
    .bind(&req.query_id)
    .execute(&state.db)
    .await?;

    Ok(Json(SavedView {
        query_id: rows.get("id"),
        data_source_id: existing_ds_id,
        conversation_id: req.conversation_id.clone().or(existing_conversation_id),
        sql: existing_sql,
        name: req.name,
        description: req.description.or(existing_desc),
        created_at: existing_created.to_rfc3339(),
    }))
}

/// PATCH /api/v1/nl2sql/views/:query_id — rename or update a saved view.
#[derive(Debug, Deserialize)]
pub(crate) struct PatchSavedViewRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}
pub(crate) async fn patch_saved_view(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(query_id): Path<String>,
    Json(req): Json<PatchSavedViewRequest>,
) -> Result<Json<SavedView>> {
    let rows = sqlx::query(
        "SELECT id, data_source_id, conversation_id, generated_sql, saved_view_name, saved_view_description, created_at \
         FROM nl2sql_queries \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND saved_view_name IS NOT NULL AND deleted_at IS NULL",
    )
    .bind(&query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("saved view not found".into()))?;

    let name: String = req.name.unwrap_or_else(|| rows.get("saved_view_name"));
    let description: Option<String> = req
        .description
        .or_else(|| rows.get("saved_view_description"));
    let ds_id: Option<String> = rows.get("data_source_id");
    let conversation_id: Option<String> = rows.get("conversation_id");
    let sql: String = rows.get("generated_sql");
    let created: chrono::DateTime<chrono::Utc> = rows.get("created_at");

    sqlx::query(
        "UPDATE nl2sql_queries \
         SET saved_view_name = ?, saved_view_description = ? \
         WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(&name)
    .bind(&description)
    .bind(&query_id)
    .bind(&claims.tenant_id)
    .execute(&state.db)
    .await?;

    Ok(Json(SavedView {
        query_id,
        data_source_id: ds_id,
        conversation_id,
        sql,
        name,
        description,
        created_at: created.to_rfc3339(),
    }))
}

/// DELETE /api/v1/nl2sql/views/:query_id — remove saved-view marker from a query.
pub(crate) async fn delete_saved_view(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(query_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let affected = sqlx::query(
        "UPDATE nl2sql_queries \
         SET saved_view_name = NULL, saved_view_description = NULL \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND saved_view_name IS NOT NULL AND deleted_at IS NULL",
    )
    .bind(&query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(AppError::NotFound("saved view not found".into()));
    }

    Ok(Json(
        serde_json::json!({ "deleted": true, "query_id": query_id }),
    ))
}
