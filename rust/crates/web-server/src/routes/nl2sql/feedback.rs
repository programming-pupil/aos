use crate::auth::Claims;
use crate::error::Result;
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct FeedbackRegressionMaterial {
    case_id: String,
    original_ir_hash: String,
    original_sql_hash: String,
    corrected_sql_hash: String,
    semantic_diff: serde_json::Value,
    verification: super::queries::CandidateSemanticVerification,
    fixture: serde_json::Value,
}

fn sha256_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn semantic_verification_diff(
    ir_hash: &str,
    original: &serde_json::Value,
    corrected: &serde_json::Value,
) -> serde_json::Value {
    let keys = [
        "safety",
        "schema_binding",
        "metric_equivalence",
        "denominator_equivalence",
        "population_equivalence",
        "grain_consistency",
        "time_consistency",
        "comparison_consistency",
        "join_cardinality",
        "filter_completeness",
        "null_semantics",
        "ordering_consistency",
        "limit_consistency",
        "policy_compliance",
        "result_invariants",
        "release_decision",
    ];
    let changes = keys
        .iter()
        .filter_map(|key| {
            let before = original
                .get(*key)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let after = corrected
                .get(*key)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            (before != after).then(|| {
                serde_json::json!({
                    "field": key,
                    "before": before,
                    "after": after,
                })
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schemaVersion": "nl2sql-feedback-semantic-diff-v1",
        "canonicalIrHash": ir_hash,
        "changedChecks": changes,
    })
}

async fn compile_feedback_regression_material(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    query_id: &str,
    question: &str,
    generated_sql: &str,
    corrected_sql: &str,
) -> Result<FeedbackRegressionMaterial> {
    let original = super::queries::compile_candidate_against_canonical_intent(
        db,
        tenant_id,
        query_id,
        Some(datasource_id),
        generated_sql,
    )
    .await?;
    let corrected = super::queries::compile_candidate_against_canonical_intent(
        db,
        tenant_id,
        query_id,
        Some(datasource_id),
        corrected_sql,
    )
    .await?;
    if original.intent != corrected.intent {
        return Err(crate::error::AppError::ValidationError(
            "feedback correction changed the immutable analytic intent".into(),
        ));
    }
    let original_ir_hash = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT ir_hash FROM analytic_intent_ir
         WHERE tenant_id = ? AND turn_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(query_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        crate::error::AppError::ValidationError(
            "feedback correction is missing its immutable analytic IR hash".into(),
        )
    })?;
    let loaded_ir_hash = super::semantic_audit::hash_json(
        &serde_json::to_value(&corrected.intent)
            .map_err(|error| crate::error::AppError::Internal(error.to_string()))?,
    );
    if original_ir_hash != loaded_ir_hash {
        return Err(crate::error::AppError::ValidationError(
            "feedback correction canonical IR hash does not match durable storage".into(),
        ));
    }
    let original_sql_hash = sha256_text(generated_sql);
    let corrected_sql_hash = sha256_text(corrected_sql);
    let semantic_diff = semantic_verification_diff(
        &original_ir_hash,
        &original.verification,
        &corrected.verification,
    );
    let fixture = serde_json::json!({
        "schemaVersion": "nl2sql-feedback-regression-v1",
        "analyticIntentId": query_id,
        "canonicalIrHash": original_ir_hash,
        "datasourceId": datasource_id,
        "question": question,
        "originalSql": generated_sql,
        "originalSqlHash": original_sql_hash,
        "correctedSql": corrected_sql,
        "correctedSqlHash": corrected_sql_hash,
        "expectedStaticDecision": corrected.release_decision,
        "requiresExecutionEvidence": corrected.release_decision == "ValidateResult",
    });
    let case_id = format!(
        "nl2sql-feedback-{}",
        sha256_text(&serde_json::to_string(&fixture).unwrap_or_default())
    );
    Ok(FeedbackRegressionMaterial {
        case_id,
        original_ir_hash,
        original_sql_hash,
        corrected_sql_hash,
        semantic_diff,
        verification: corrected,
        fixture,
    })
}

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFeedbackApprovalRequest {
    pub approved: bool,
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

fn can_approve_feedback_learning(
    role: &str,
    user_id: &str,
    datasource_owner: Option<&str>,
    datasource_creator: Option<&str>,
) -> bool {
    matches!(role, "admin" | "superadmin")
        || datasource_owner == Some(user_id)
        || datasource_creator == Some(user_id)
}

fn validate_feedback_learning_request(
    request: &SubmitFeedbackRequest,
    can_approve: bool,
) -> Result<bool> {
    let corrected_sql = request
        .corrected_sql
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if request.feedback_type == "correction" && corrected_sql.is_none() {
        return Err(crate::error::AppError::ValidationError(
            "correction feedback requires corrected_sql".into(),
        ));
    }
    if request.feedback_type != "correction" && corrected_sql.is_some() {
        return Err(crate::error::AppError::ValidationError(
            "corrected_sql is only valid for correction feedback".into(),
        ));
    }
    if let Some(corrected) = corrected_sql {
        if !super::agent::is_safe_sql(corrected) {
            return Err(crate::error::AppError::ValidationError(
                "corrected_sql must be a read-only SELECT statement".into(),
            ));
        }
    }
    let approved = request.approved.unwrap_or(false);
    if approved && request.feedback_type != "correction" {
        return Err(crate::error::AppError::ValidationError(
            "only correction feedback can become an approved exemplar".into(),
        ));
    }
    if approved && !can_approve {
        return Err(crate::error::AppError::Forbidden);
    }
    Ok(approved)
}

async fn apply_feedback_learning_approval(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    feedback_event_id: &str,
    decided_by: &str,
    approved: bool,
) -> Result<()> {
    let updated = sqlx::query::<sqlx::Sqlite>(
        "UPDATE feedback_learning_events
         SET approved = ?, approved_by = ?,
             approved_at = IIF(? = 1, CURRENT_TIMESTAMP, NULL)
         WHERE id = ? AND tenant_id = ?",
    )
    .bind(i64::from(approved))
    .bind(approved.then_some(decided_by))
    .bind(i64::from(approved))
    .bind(feedback_event_id)
    .bind(tenant_id)
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(crate::error::AppError::NotFound(
            "feedback learning event not found".into(),
        ));
    }
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO feedback_learning_approval_events
            (id, tenant_id, feedback_event_id, decision, decided_by, created_at)
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(feedback_event_id)
    .bind(if approved { "approved" } else { "revoked" })
    .bind(decided_by)
    .execute(&mut **transaction)
    .await?;
    let regression_status = if approved { "approved" } else { "revoked" };
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE feedback_regression_cases
         SET status = CASE
               WHEN ? = 'approved'
                    AND json_extract(execution_evidence_json, '$.releaseDecision') = 'Release'
                 THEN 'verified'
               ELSE ?
             END,
             approved_by = ?,
             approved_at = IIF(? = 'approved', CURRENT_TIMESTAMP, NULL),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND feedback_event_id = ?",
    )
    .bind(regression_status)
    .bind(regression_status)
    .bind(approved.then_some(decided_by))
    .bind(regression_status)
    .bind(tenant_id)
    .bind(feedback_event_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn load_calibration_metrics(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Result<(usize, f64, f64)> {
    crate::behavior_trace("SQL-004");
    let observations = sqlx::query_as::<sqlx::Sqlite, (f32, i64)>(
        "SELECT predicted_score, actual_correct
         FROM nl2sql_confidence_observations
         WHERE tenant_id = ? AND datasource_id = ? AND actual_correct IS NOT NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|(score, label)| (score, label != 0))
    .collect::<Vec<_>>();
    let (ece, brier) = nl2sql_core::semantic_ir::calibration_metrics(&observations, 10);
    Ok((observations.len(), f64::from(ece), f64::from(brier)))
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
    super::validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &req.datasource_id,
    )
    .await?;
    let (datasource_owner, datasource_creator) =
        sqlx::query_as::<sqlx::Sqlite, (Option<String>, Option<String>)>(
            "SELECT user_id, created_by FROM data_sources
             WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(&req.datasource_id)
        .bind(&claims.tenant_id)
        .fetch_one(&state.db)
        .await?;
    let approved = validate_feedback_learning_request(
        &req,
        can_approve_feedback_learning(
            &claims.role,
            &claims.sub,
            datasource_owner.as_deref(),
            datasource_creator.as_deref(),
        ),
    )?;
    let datasource_ids = serde_json::json!([req.datasource_id.clone()]);
    // The legacy feedback DTO exposes a numeric query id, while current query
    // rows use opaque text ids. Resolve the authoritative row from the scoped
    // conversation + SQL so calibration labels attach to the right IR.
    let resolved_query = sqlx::query_as::<sqlx::Sqlite, (String, String)>(
        "SELECT id, question FROM nl2sql_queries
         WHERE tenant_id = ? AND data_source_id = ? AND conversation_id = ?
           AND user_id = ? AND generated_sql = ?
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&req.datasource_id)
    .bind(&req.conversation_id)
    .bind(&claims.sub)
    .bind(&req.generated_sql)
    .fetch_optional(&state.db)
    .await?;
    let (resolved_query_id, question) = resolved_query.ok_or_else(|| {
        crate::error::AppError::ValidationError(
            "feedback must reference an existing query owned by the current user".into(),
        )
    })?;
    let regression_material = if req.feedback_type == "correction" {
        let corrected_sql = req
            .corrected_sql
            .as_deref()
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .expect("correction SQL validated above");
        Some(
            compile_feedback_regression_material(
                &state.db,
                &claims.tenant_id,
                &req.datasource_id,
                &resolved_query_id,
                &question,
                &req.generated_sql,
                corrected_sql,
            )
            .await?,
        )
    } else {
        None
    };
    let mut transaction = state.db.begin().await?;
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
    .fetch_one(&mut *transaction)
    .await?;

    let correction = json!({
        "question": question,
        "analyticIntentId": resolved_query_id.clone(),
        "datasourceId": req.datasource_id.clone(),
        "generatedSql": req.generated_sql.clone(),
        "correctedSql": req.corrected_sql.clone(),
        "feedbackType": req.feedback_type.clone(),
        "note": req.correction_note.clone(),
        "submittedBy": claims.sub.clone(),
        "approved": approved,
        "approvedBy": approved.then(|| claims.sub.clone()),
    });
    let regression_case_id = regression_material
        .as_ref()
        .map(|material| material.case_id.clone());
    let feedback_event_id = format!("nl2sql-feedback-event:{id}");
    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO feedback_learning_events
            (id, tenant_id, scope, correction_json, approved, regression_case_id,
             approved_by, approved_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, IIF(? = 1, CURRENT_TIMESTAMP, NULL), CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET correction_json = excluded.correction_json",
    )
    .bind(&feedback_event_id)
    .bind(&claims.tenant_id)
    .bind(format!("datasource:{}", req.datasource_id))
    .bind(correction.to_string())
    .bind(0_i64)
    .bind(regression_case_id.as_deref())
    .bind(Option::<&str>::None)
    .bind(0_i64)
    .execute(&mut *transaction)
    .await?;
    if let Some(material) = regression_material.as_ref() {
        crate::semantic_kernel_store::persist_nl2sql_repair_verification_in_transaction(
            &mut transaction,
            &claims.tenant_id,
            &resolved_query_id,
            req.corrected_sql.as_deref().unwrap_or_default(),
            &material.verification.verification,
            &material.verification.release_decision,
            material.verification.calibrated_score,
        )
        .await
        .map_err(|error| crate::error::AppError::Internal(error.to_string()))?;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO feedback_regression_cases
                (id, tenant_id, datasource_id, feedback_event_id, analytic_intent_id,
                 original_ir_hash, original_sql_hash, corrected_sql_hash,
                 semantic_diff_json, verification_json, execution_evidence_json,
                 fixture_json, status, approved_by, approved_at, last_verified_at,
                 created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, 'proposed', NULL,
                     NULL, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&material.case_id)
        .bind(&claims.tenant_id)
        .bind(&req.datasource_id)
        .bind(&feedback_event_id)
        .bind(&resolved_query_id)
        .bind(&material.original_ir_hash)
        .bind(&material.original_sql_hash)
        .bind(&material.corrected_sql_hash)
        .bind(material.semantic_diff.to_string())
        .bind(material.verification.verification.to_string())
        .bind(material.fixture.to_string())
        .execute(&mut *transaction)
        .await?;
    }
    if approved {
        apply_feedback_learning_approval(
            &mut transaction,
            &claims.tenant_id,
            &feedback_event_id,
            &claims.sub,
            true,
        )
        .await?;
    }
    let actual_correct = i64::from(req.feedback_type == "thumbs_up");
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_confidence_observations
         SET actual_correct = ?, feedback_id = ?, labeled_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND analytic_intent_id = ? AND actual_correct IS NULL",
    )
    .bind(actual_correct)
    .bind(crate::sqlite_i64(id))
    .bind(&claims.tenant_id)
    .bind(&resolved_query_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    Ok(Json(SubmitFeedbackResponse { id }))
}

pub async fn set_feedback_learning_approval(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(feedback_id): Path<u64>,
    Json(request): Json<SetFeedbackApprovalRequest>,
) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query_as::<sqlx::Sqlite, (String, Option<String>, String)>(
        "SELECT feedback_type, corrected_sql, datasource_ids
         FROM nl2sql_query_feedback WHERE id = ? AND tenant_id = ?",
    )
    .bind(crate::sqlite_i64(feedback_id))
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| crate::error::AppError::NotFound("feedback not found".into()))?;
    if row.0 != "correction" || row.1.as_deref().is_none_or(|sql| sql.trim().is_empty()) {
        return Err(crate::error::AppError::ValidationError(
            "only feedback with a corrected SQL statement can be approved".into(),
        ));
    }
    let datasource_ids = serde_json::from_str::<Vec<String>>(&row.2).map_err(|_| {
        crate::error::AppError::ValidationError("feedback has malformed datasource scope".into())
    })?;
    let [datasource_id] = datasource_ids.as_slice() else {
        return Err(crate::error::AppError::ValidationError(
            "feedback approval requires exactly one datasource scope".into(),
        ));
    };
    super::validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        datasource_id,
    )
    .await?;
    let (datasource_owner, datasource_creator) =
        sqlx::query_as::<sqlx::Sqlite, (Option<String>, Option<String>)>(
            "SELECT user_id, created_by FROM data_sources
             WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(datasource_id)
        .bind(&claims.tenant_id)
        .fetch_one(&state.db)
        .await?;
    if !can_approve_feedback_learning(
        &claims.role,
        &claims.sub,
        datasource_owner.as_deref(),
        datasource_creator.as_deref(),
    ) {
        return Err(crate::error::AppError::Forbidden);
    }

    let feedback_event_id = format!("nl2sql-feedback-event:{feedback_id}");
    let mut transaction = state.db.begin().await?;
    apply_feedback_learning_approval(
        &mut transaction,
        &claims.tenant_id,
        &feedback_event_id,
        &claims.sub,
        request.approved,
    )
    .await?;
    transaction.commit().await?;

    Ok(Json(serde_json::json!({
        "feedbackId": feedback_id,
        "approved": request.approved,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        feedback_type: &str,
        corrected_sql: Option<&str>,
        approved: bool,
    ) -> SubmitFeedbackRequest {
        SubmitFeedbackRequest {
            conversation_id: "conversation".into(),
            query_id: None,
            datasource_id: "datasource".into(),
            generated_sql: "SELECT 1".into(),
            feedback_type: feedback_type.into(),
            corrected_sql: corrected_sql.map(str::to_string),
            correction_note: None,
            approved: Some(approved),
        }
    }

    #[test]
    fn approved_exemplar_requires_a_safe_correction_and_owner_authority() {
        let missing_sql = request("correction", None, false);
        assert!(validate_feedback_learning_request(&missing_sql, true).is_err());

        let unsafe_sql = request("correction", Some("DELETE FROM orders"), false);
        assert!(validate_feedback_learning_request(&unsafe_sql, true).is_err());

        let unprivileged = request("correction", Some("SELECT * FROM orders"), true);
        assert!(matches!(
            validate_feedback_learning_request(&unprivileged, false),
            Err(crate::error::AppError::Forbidden)
        ));

        let owner = request("correction", Some("SELECT * FROM orders"), true);
        assert!(validate_feedback_learning_request(&owner, true).unwrap());
        assert!(can_approve_feedback_learning(
            "viewer",
            "owner",
            Some("owner"),
            None
        ));
        assert!(can_approve_feedback_learning("admin", "admin", None, None));
    }

    #[tokio::test]
    async fn feedback_learning_and_calibration_are_scoped_and_owner_approved() {
        let pool = crate::test_sqlite_pool().await;
        assert!(can_approve_feedback_learning(
            "viewer",
            "owner",
            Some("owner"),
            None
        ));
        sqlx::query(
            "INSERT INTO feedback_learning_events
                (id, tenant_id, scope, correction_json, approved, regression_case_id, created_at)
             VALUES ('feedback-event', 'tenant', 'datasource:ds', '{}', 0, 'case', CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO feedback_regression_cases
                (id, tenant_id, datasource_id, feedback_event_id, analytic_intent_id,
                 original_ir_hash, original_sql_hash, corrected_sql_hash,
                 semantic_diff_json, verification_json, fixture_json, status)
             VALUES ('case', 'tenant', 'ds', 'feedback-event', 'intent',
                     'ir', 'old', 'new', '{}', '{}', '{}', 'proposed')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        apply_feedback_learning_approval(
            &mut transaction,
            "tenant",
            "feedback-event",
            "owner",
            true,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        let approved: (i64, Option<String>) = sqlx::query_as(
            "SELECT approved, approved_by FROM feedback_learning_events
             WHERE id = 'feedback-event'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(approved, (1, Some("owner".into())));
        let approved_case: String =
            sqlx::query_scalar("SELECT status FROM feedback_regression_cases WHERE id = 'case'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(approved_case, "approved");

        let mut transaction = pool.begin().await.unwrap();
        apply_feedback_learning_approval(
            &mut transaction,
            "tenant",
            "feedback-event",
            "admin",
            false,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        let current: (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT approved, approved_by, approved_at FROM feedback_learning_events
             WHERE id = 'feedback-event'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current, (0, None, None));
        let revoked_case: String =
            sqlx::query_scalar("SELECT status FROM feedback_regression_cases WHERE id = 'case'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(revoked_case, "revoked");
        let decisions = sqlx::query_scalar::<_, String>(
            "SELECT decision FROM feedback_learning_approval_events
             WHERE tenant_id = 'tenant' AND feedback_event_id = 'feedback-event'
             ORDER BY created_at, id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(decisions.iter().any(|decision| decision == "approved"));
        assert!(decisions.iter().any(|decision| decision == "revoked"));

        for (id, tenant, datasource, score, actual) in [
            ("o1", "tenant", "ds", 0.9_f32, 1_i64),
            ("o2", "tenant", "ds", 0.2_f32, 0_i64),
            ("o3", "other", "ds", 0.99_f32, 0_i64),
            ("o4", "tenant", "other-ds", 0.99_f32, 0_i64),
        ] {
            sqlx::query(
                "INSERT INTO nl2sql_confidence_observations
                    (id, tenant_id, datasource_id, analytic_intent_id, predicted_score,
                     actual_correct, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
            )
            .bind(id)
            .bind(tenant)
            .bind(datasource)
            .bind(format!("intent-{id}"))
            .bind(score)
            .bind(actual)
            .execute(&pool)
            .await
            .unwrap();
        }
        let (count, ece, brier) = load_calibration_metrics(&pool, "tenant", "ds")
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert!((0.0..=1.0).contains(&ece));
        assert!((0.0..=1.0).contains(&brier));
    }

    #[tokio::test]
    async fn approval_does_not_verify_a_regression_case_rejected_by_execution() {
        let pool = crate::test_sqlite_pool().await;
        sqlx::query(
            "INSERT INTO feedback_learning_events
                (id, tenant_id, scope, correction_json, approved, regression_case_id, created_at)
             VALUES ('rejected-feedback', 'tenant', 'datasource:ds', '{}', 0, 'rejected-case', CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO feedback_regression_cases
                (id, tenant_id, datasource_id, feedback_event_id, analytic_intent_id,
                 original_ir_hash, original_sql_hash, corrected_sql_hash,
                 semantic_diff_json, verification_json, execution_evidence_json,
                 fixture_json, status)
             VALUES ('rejected-case', 'tenant', 'ds', 'rejected-feedback', 'intent',
                     'ir', 'old', 'new', '{}', '{}',
                     '{\"releaseDecision\":\"Reject\",\"reason\":\"result invariant failed\"}',
                     '{}', 'proposed')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        apply_feedback_learning_approval(
            &mut transaction,
            "tenant",
            "rejected-feedback",
            "owner",
            true,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let status: String = sqlx::query_scalar(
            "SELECT status FROM feedback_regression_cases WHERE id = 'rejected-case'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "approved");
    }

    #[tokio::test]
    async fn feedback_correction_is_rejected_when_it_drifts_from_the_canonical_ir() {
        let pool = crate::test_sqlite_pool().await;
        let mut intent = super::super::semantic_audit::compile_question_intent(
            "tenant",
            "ds",
            "按设备统计订单数",
            &[],
        );
        super::super::semantic_audit::bind_schema_dimensions(
            &mut intent,
            &serde_json::json!([{
                "table_name": "orders",
                "columns": [
                    {"name": "executor_device_id"},
                    {"name": "order_id"}
                ]
            }]),
            &[],
        );
        let metric_contract = super::super::semantic_audit::bind_test_count_metric_contract(
            &mut intent,
            "created_at",
        );
        crate::semantic_kernel_store::persist_nl2sql_intent_ir(
            &pool,
            "tenant",
            "conversation",
            "query",
            "query",
            &serde_json::to_value(&intent).unwrap(),
        )
        .await
        .unwrap();
        let original = "SELECT executor_device_id, COUNT(*) AS order_count \
                        FROM orders GROUP BY executor_device_id";
        let audit =
            super::super::semantic_audit::compile_canonical_intent_with_contracts_and_joins(
                &intent,
                original,
                std::slice::from_ref(&metric_contract),
                &[],
            )
            .unwrap();
        let decision = serde_json::to_string(&audit.verification.release_decision)
            .unwrap()
            .trim_matches('"')
            .to_string();
        assert_eq!(decision, "Release");
        crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
            &pool,
            "tenant",
            "ds",
            "conversation",
            "query",
            &serde_json::to_value(&intent).unwrap(),
            &super::super::semantic_audit::verification_json(&audit),
            &decision,
            f64::from(audit.verification.confidence_basis.calibrated_score),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metric_contracts
                (id, tenant_id, datasource_id, source_metric_id, version, status,
                 contract_json, lineage_json, valid_from, valid_until)
             VALUES (?, 'tenant', 'ds', NULL, 1, 'active', ?, '{}',
                     '2026-01-01', NULL)",
        )
        .bind(&metric_contract.id)
        .bind(serde_json::to_string(&metric_contract).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let material = compile_feedback_regression_material(
            &pool,
            "tenant",
            "ds",
            "query",
            "按设备统计订单数",
            original,
            "SELECT COUNT(*) AS order_count FROM orders",
        )
        .await;
        assert!(material.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feedback_regression_cases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
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
    let (labeled_observations, ece, brier) =
        load_calibration_metrics(&state.db, &claims.tenant_id, &datasource_id).await?;
    Ok(Json(FeedbackStatsRow {
        ds_id: datasource_id,
        total_feedback: row.0,
        thumbs_up_count: row.1,
        thumbs_down_count: row.2,
        correction_count: row.3,
        satisfaction_rate: row.4,
        labeled_observations: i64::try_from(labeled_observations).unwrap_or(i64::MAX),
        expected_calibration_error: ece,
        brier_score: brier,
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
