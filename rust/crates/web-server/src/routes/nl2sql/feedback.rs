use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

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
    pub approved: Option<bool>,
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
    pub labeled_observations: i64,
    pub expected_calibration_error: f64,
    pub brier_score: f64,
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

    let datasource_ids = serde_json::json!([req.datasource_id.clone()]);
    if let Some(corrected) = req.corrected_sql.as_deref() {
        if !super::agent::is_safe_sql(corrected) {
            return Err(crate::error::AppError::ValidationError(
                "corrected_sql must be a read-only SELECT statement".into(),
            ));
        }
    }
    // The legacy feedback DTO exposes a numeric query id, while current query
    // rows use opaque text ids. Resolve the authoritative row from the scoped
    // conversation + SQL so calibration labels attach to the right IR.
    let resolved_query = sqlx::query_as::<sqlx::Sqlite, (String, String)>(
        "SELECT id, question FROM nl2sql_queries
         WHERE tenant_id = ? AND data_source_id = ? AND conversation_id = ?
           AND generated_sql = ?
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&req.datasource_id)
    .bind(&req.conversation_id)
    .bind(&req.generated_sql)
    .fetch_optional(&state.db)
    .await?;
    let question = resolved_query
        .as_ref()
        .map(|(_, question)| question.clone());
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

    let correction = json!({
        "question": question,
        "analyticIntentId": resolved_query.as_ref().map(|(id, _)| id),
        "datasourceId": req.datasource_id.clone(),
        "generatedSql": req.generated_sql.clone(),
        "correctedSql": req.corrected_sql.clone(),
        "feedbackType": req.feedback_type.clone(),
        "note": req.correction_note.clone(),
        "submittedBy": claims.sub.clone(),
    });
    let regression_case_id = format!(
        "nl2sql-feedback-{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&correction).unwrap_or_default()
        ))
    );
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO feedback_learning_events
            (id, tenant_id, scope, correction_json, approved, regression_case_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET correction_json = excluded.correction_json",
    )
    .bind(format!("nl2sql-feedback-event:{id}"))
    .bind(&claims.tenant_id)
    .bind(format!("datasource:{}", req.datasource_id))
    .bind(correction.to_string())
    .bind(i64::from(req.approved.unwrap_or(false)))
    .bind(regression_case_id)
    .execute(&state.db)
    .await?;
    if let Some((query_id, _)) = resolved_query {
        let actual_correct = i64::from(req.feedback_type == "thumbs_up");
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_confidence_observations
             SET actual_correct = ?, feedback_id = ?, labeled_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND analytic_intent_id = ? AND actual_correct IS NULL",
        )
        .bind(actual_correct)
        .bind(crate::sqlite_i64(id))
        .bind(&claims.tenant_id)
        .bind(query_id)
        .execute(&state.db)
        .await?;
    }

    Ok(Json(SubmitFeedbackResponse { id }))
}

pub async fn get_feedback_stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<FeedbackStatsRow>> {
    let row = sqlx::query_as::<sqlx::Sqlite, (i64, i64, i64, i64, Option<f64>)>(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN feedback_type = 'thumbs_up' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN feedback_type = 'thumbs_down' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN feedback_type = 'correction' THEN 1 ELSE 0 END), 0),
                CASE WHEN SUM(CASE WHEN feedback_type IN ('thumbs_up','thumbs_down') THEN 1 ELSE 0 END) = 0
                     THEN NULL
                     ELSE 100.0 * SUM(CASE WHEN feedback_type = 'thumbs_up' THEN 1 ELSE 0 END)
                          / SUM(CASE WHEN feedback_type IN ('thumbs_up','thumbs_down') THEN 1 ELSE 0 END)
                END
         FROM nl2sql_query_feedback f, json_each(f.datasource_ids) j
         WHERE f.tenant_id = ? AND json_valid(f.datasource_ids)
           AND CAST(j.value AS TEXT) = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_one(&state.db)
    .await?;
    let observations = sqlx::query_as::<sqlx::Sqlite, (f32, i64)>(
        "SELECT predicted_score, actual_correct
         FROM nl2sql_confidence_observations
         WHERE tenant_id = ? AND datasource_id = ? AND actual_correct IS NOT NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|(score, label)| (score, label != 0))
    .collect::<Vec<_>>();
    let (ece, brier) = nl2sql_core::semantic_ir::calibration_metrics(&observations, 10);
    Ok(Json(FeedbackStatsRow {
        ds_id: datasource_id,
        total_feedback: row.0,
        thumbs_up_count: row.1,
        thumbs_down_count: row.2,
        correction_count: row.3,
        satisfaction_rate: row.4,
        labeled_observations: observations.len() as i64,
        expected_calibration_error: f64::from(ece),
        brier_score: f64::from(brier),
    }))
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
