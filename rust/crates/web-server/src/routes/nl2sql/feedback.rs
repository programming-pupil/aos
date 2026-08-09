use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitFeedbackRequest {
    pub conversation_id: String,
    pub query_id: Option<u64>,
    pub datasource_id: String,
    pub generated_sql: String,
    pub feedback_type: String, // "thumbs_up" | "thumbs_down" | "correction"
    pub corrected_sql: Option<String>,
    pub correction_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitFeedbackResponse {
    pub id: u64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackStatsRow {
    pub ds_id: String,
    pub total_feedback: i64,
    pub thumbs_up_count: i64,
    pub thumbs_down_count: i64,
    pub correction_count: i64,
    pub satisfaction_rate: Option<f64>,
}

pub async fn submit_feedback(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SubmitFeedbackRequest>,
) -> Result<Json<SubmitFeedbackResponse>> {
    let allowed = [
        "thumbs_up",
        "thumbs_down",
        "correction",
        "clarification_needed",
    ];
    if !allowed.contains(&req.feedback_type.as_str()) {
        return Err(crate::error::AppError::ValidationError(
            "invalid feedback_type".into(),
        ));
    }

    let datasource_ids = serde_json::json!([req.datasource_id]);
    let id: u64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "INSERT INTO nl2sql_query_feedback \
           (tenant_id, conversation_id, query_id, generated_sql, feedback_type, \
            corrected_sql, correction_note, datasource_ids, created_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING id",
    )
    .bind(&claims.tenant_id)
    .bind(&req.conversation_id)
    .bind(req.query_id.map(crate::sqlite_i64))
    .bind(&req.generated_sql)
    .bind(&req.feedback_type)
    .bind(&req.corrected_sql)
    .bind(&req.correction_note)
    .bind(datasource_ids.to_string())
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(SubmitFeedbackResponse { id }))
}

pub async fn get_feedback_stats(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<FeedbackStatsRow>> {
    let row: Option<FeedbackStatsRow> = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT ds_id, total_feedback, thumbs_up_count, thumbs_down_count, \
                correction_count, satisfaction_rate \
         FROM nl2sql_feedback_stats \
         WHERE ds_id = ? \
         LIMIT 1",
    )
    .bind(&datasource_id)
    .fetch_optional(&state.db)
    .await?;

    Ok(Json(row.unwrap_or(FeedbackStatsRow {
        ds_id: datasource_id,
        total_feedback: 0,
        thumbs_up_count: 0,
        thumbs_down_count: 0,
        correction_count: 0,
        satisfaction_rate: None,
    })))
}

pub async fn clear_result_cache(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    crate::nl2sql::result_cache::invalidate_datasource(
        &state.db,
        &claims.tenant_id,
        &datasource_id,
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
