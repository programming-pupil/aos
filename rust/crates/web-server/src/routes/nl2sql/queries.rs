use super::{
    build_nl2sql_prompt, classify_sql, conversation_summary_threshold, decode_mysql_cell,
    decode_postgres_cell, extract_schema_tables_and_fks, extract_sql_from_llm_output,
    is_datasource_sensitive, load_conversation_history, load_join_paths_for_datasource,
    load_manual_foreign_keys, mask_sensitive_value, max_operational_retry_attempts,
    max_self_correct_attempts, record_routing_features, should_enable_qu,
    should_enable_result_validation, validate_data_source_access, ColumnInfo, ExecuteRequest,
    ExecuteResponse, ForeignKeyPrompt, InputContentBlock, InputMessage, MessageRequest,
    OutputContentBlock, PaginationParams, QueryHistoryItem, QueryHistoryResponse,
    SelfCorrectContext, SqlExecErrorKind, SqlRepairDecision, SqlSafetyResult,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::data_sources::decrypt_config;
use crate::routes::nl2sql::query_cancel::{
    register_active_query, unregister_active_query, ActiveQueryHandle,
};
use crate::state::AppState;
use axum::extract::{Extension, Json, Query, State};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sqlx::FromRow;
use sqlx::{Column, Row};
use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

fn cached_regex(
    cell: &'static OnceLock<std::result::Result<Regex, regex::Error>>,
    pattern: &'static str,
) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern)).as_ref().ok()
}

fn push_unique_rule_hit(
    target: &mut Vec<super::AppliedRuleHit>,
    seen: &mut BTreeSet<String>,
    rule_key: &str,
    rule_name: &str,
    detail: Option<String>,
) {
    let normalized = rule_key.trim().to_lowercase();
    if normalized.is_empty() || seen.contains(&normalized) {
        return;
    }
    seen.insert(normalized);
    target.push(super::AppliedRuleHit {
        rule_key: rule_key.to_string(),
        rule_name: rule_name.to_string(),
        detail,
    });
}

fn derive_validation_tables(sql: &str, primary_table_hint: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut tables: Vec<String> = Vec::new();

    if !primary_table_hint.trim().is_empty() {
        let t = primary_table_hint.trim().to_string();
        if seen.insert(t.clone()) {
            tables.push(t);
        }
    }

    for t in crate::routes::nl2sql::extract_top_level_tables(sql) {
        let table = t.trim();
        if table.is_empty() {
            continue;
        }
        let canonical = table.to_string();
        if seen.insert(canonical.clone()) {
            tables.push(canonical);
        }
    }

    if tables.is_empty() {
        tables.push("unknown".to_string());
    }
    tables
}

#[derive(Debug, Clone)]
pub(super) struct CandidateSemanticVerification {
    pub intent: nl2sql_core::semantic_ir::AnalyticIntentIR,
    pub verification: serde_json::Value,
    pub release_decision: String,
    pub calibrated_score: f64,
}

/// Compile a candidate against the immutable IR without mutating durable
/// state. Callers that create another durable object can therefore persist the
/// verification and that object in one transaction.
pub(super) async fn compile_candidate_against_canonical_intent(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    query_id: &str,
    datasource_id: Option<&str>,
    sql: &str,
) -> Result<CandidateSemanticVerification> {
    let intent = crate::semantic_kernel_store::load_nl2sql_intent_ir(db, tenant_id, query_id)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .ok_or_else(|| {
            AppError::ValidationError(
                "SQL repair is blocked because the canonical analytic intent is missing".into(),
            )
        })?;
    if intent.security_scope.tenant_id != tenant_id {
        return Err(AppError::ValidationError(
            "SQL execution is blocked because the canonical intent belongs to another tenant"
                .into(),
        ));
    }
    if let Some(datasource_id) = datasource_id {
        if intent.security_scope.datasource_id != datasource_id {
            return Err(AppError::ValidationError(
                "SQL execution is blocked because the canonical intent is bound to another datasource"
                    .into(),
            ));
        }
    }
    let original_release = sqlx::query_scalar::<_, String>(
        "SELECT release_decision FROM semantic_verifications
         WHERE tenant_id = ? AND analytic_intent_id = ?",
    )
    .bind(tenant_id)
    .bind(query_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        AppError::ValidationError(
            "SQL execution is blocked because the original semantic release decision is missing"
                .into(),
        )
    })?;
    super::semantic_audit::require_execution_validation_decision(&original_release)
        .map_err(AppError::ValidationError)?;
    let metric_ids = intent
        .metrics
        .iter()
        .map(|metric| metric.id.clone())
        .collect::<Vec<_>>();
    let metric_contracts = crate::semantic_kernel_store::load_metric_contracts(
        db,
        tenant_id,
        &intent.security_scope.datasource_id,
        &metric_ids,
    )
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?
    .into_iter()
    .map(|stored| stored.contract)
    .collect::<Vec<_>>();
    let join_contracts = crate::semantic_kernel_store::load_join_contracts(
        db,
        tenant_id,
        &intent.security_scope.datasource_id,
    )
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?
    .into_iter()
    .map(|stored| stored.contract)
    .collect::<Vec<_>>();
    let audit = super::semantic_audit::compile_canonical_intent_with_contracts_and_joins(
        &intent,
        sql,
        &metric_contracts,
        &join_contracts,
    )
    .ok_or_else(|| {
        AppError::ValidationError(
            "SQL repair is blocked because it cannot be verified against the canonical analytic intent"
                .into(),
        )
    })?;
    let verification = super::semantic_audit::verification_json(&audit);
    let release_decision = serde_json::to_string(&audit.verification.release_decision)
        .unwrap_or_else(|_| "\"Reject\"".to_string())
        .trim_matches('"')
        .to_string();
    super::semantic_audit::require_execution_validation_decision(&release_decision)
        .map_err(AppError::ValidationError)?;
    Ok(CandidateSemanticVerification {
        intent,
        verification,
        release_decision,
        calibrated_score: f64::from(audit.verification.confidence_basis.calibrated_score),
    })
}

async fn verify_candidate_against_canonical_intent(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    query_id: &str,
    datasource_id: Option<&str>,
    sql: &str,
) -> Result<CandidateSemanticVerification> {
    let candidate = match compile_candidate_against_canonical_intent(
        db,
        tenant_id,
        query_id,
        datasource_id,
        sql,
    )
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            // A rejected repair is still a durable attempt. Keeping the
            // failure in the same append-only audit stream makes retries and
            // post-incident review distinguish "never tried" from "failed
            // semantic verification" without weakening the fail-closed path.
            // Scope/identity failures are deliberately excluded: the same SQL
            // may be retried after the caller supplies the correct datasource
            // binding and must not be poisoned by a prior scope mismatch.
            let error_text = error.to_string();
            let is_scope_or_identity_failure = error_text
                .contains("canonical analytic intent is missing")
                || error_text.contains("another tenant")
                || error_text.contains("another datasource")
                || error_text.contains("original semantic release decision is missing");
            if !is_scope_or_identity_failure {
                let verification = serde_json::json!({
                    "schemaVersion": "nl2sql-repair-verification-v1",
                    "release_decision": "Reject",
                    "status": "semantic_verification_failed",
                    "error": error_text,
                });
                if let Err(persist_error) =
                    crate::semantic_kernel_store::persist_nl2sql_repair_verification(
                        db,
                        tenant_id,
                        query_id,
                        sql,
                        &verification,
                        "Reject",
                        0.0,
                    )
                    .await
                {
                    tracing::warn!(
                        tenant_id,
                        query_id,
                        error = %persist_error,
                        "failed to persist rejected NL2SQL repair attempt"
                    );
                }
            }
            return Err(error);
        }
    };
    crate::semantic_kernel_store::persist_nl2sql_repair_verification(
        db,
        tenant_id,
        query_id,
        sql,
        &candidate.verification,
        &candidate.release_decision,
        candidate.calibrated_score,
    )
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    Ok(candidate)
}

async fn verify_repair_against_canonical_intent(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    query_id: &str,
    sql: &str,
) -> Result<CandidateSemanticVerification> {
    verify_candidate_against_canonical_intent(db, tenant_id, query_id, None, sql).await
}

#[derive(Debug)]
struct ResultValidationOutcome {
    release_decision: String,
    observation_count: usize,
}

async fn finalize_result_invariants(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    query_id: &str,
    sql: &str,
    rows: &[serde_json::Value],
    columns: usize,
    execution_ms: u64,
    candidate: &CandidateSemanticVerification,
) -> Result<ResultValidationOutcome> {
    let metric_ids = candidate
        .intent
        .metrics
        .iter()
        .map(|metric| metric.id.clone())
        .collect::<Vec<_>>();
    let contracts = crate::semantic_kernel_store::load_metric_contracts(
        db,
        tenant_id,
        datasource_id,
        &metric_ids,
    )
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    let mut invariant_keys = BTreeSet::new();
    let invariants = contracts
        .into_iter()
        .flat_map(|stored| stored.contract.invariants)
        .filter(|invariant| {
            invariant_keys.insert(serde_json::to_string(invariant).unwrap_or_default())
        })
        .collect::<Vec<_>>();
    let observations = nl2sql_core::semantic_ir::evaluate_result_invariants(&invariants, rows);
    let has_failed = observations
        .iter()
        .any(|observation| observation.status == nl2sql_core::semantic_ir::CheckStatus::Fail);
    let has_unobserved = observations
        .iter()
        .any(|observation| observation.status == nl2sql_core::semantic_ir::CheckStatus::NotChecked);
    let release_decision = if has_failed {
        "Reject"
    } else if has_unobserved {
        "NeedsClarification"
    } else {
        "Release"
    }
    .to_string();
    let execution_id = uuid::Uuid::new_v4().to_string();
    let sql_hash = hex::encode(sha2::Sha256::digest(sql.as_bytes()));
    let mut verification = candidate.verification.clone();
    let verification_object = verification
        .as_object_mut()
        .ok_or_else(|| AppError::Internal("semantic verification is not a JSON object".into()))?;
    verification_object.insert(
        "result_invariants".into(),
        serde_json::Value::Array(
            observations
                .iter()
                .map(|observation| {
                    serde_json::json!({
                        "status": observation.status,
                        "code": observation.code,
                        "message": observation.message,
                    })
                })
                .collect(),
        ),
    );
    verification_object.insert(
        "executable".into(),
        serde_json::json!({
            "status": "Pass",
            "code": "executed",
            "message": format!(
                "datasource returned {} row(s) across {columns} column(s) in {execution_ms}ms",
                rows.len()
            )
        }),
    );
    verification_object.insert(
        "release_decision".into(),
        serde_json::Value::String(release_decision.clone()),
    );
    if let Some(basis) = verification_object
        .get_mut("confidence_basis")
        .and_then(serde_json::Value::as_object_mut)
    {
        basis.insert("execution_passed".into(), serde_json::Value::Bool(true));
        if let Some(score) = basis
            .get("calibrated_score")
            .and_then(serde_json::Value::as_f64)
        {
            basis.insert(
                "calibrated_score".into(),
                serde_json::Value::from(if release_decision == "Release" {
                    (score + 0.03).min(0.99)
                } else {
                    score.min(0.5)
                }),
            );
        }
    }
    let execution_evidence = serde_json::json!({
        "schemaVersion": "nl2sql-result-validation-v1",
        "executionId": execution_id,
        "queryId": query_id,
        "sqlHash": sql_hash,
        "rows": rows.len(),
        "columns": columns,
        "executionMs": execution_ms,
        "releaseDecision": release_decision,
        "observations": observations,
    });
    let protected_verification =
        runtime::protect_sensitive_json(&verification, runtime::configured_data_protection_mode())
            .0
            .to_string();
    let mut transaction = db.begin().await?;
    for observation in execution_evidence["observations"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let invariant = observation
            .get("invariant")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let invariant_id = hex::encode(sha2::Sha256::digest(
            serde_json::to_vec(&invariant).unwrap_or_default(),
        ));
        let status = match observation.get("status").and_then(|value| value.as_str()) {
            Some("Pass") => "pass",
            Some("Fail") => "fail",
            _ => "not_observed",
        };
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO result_invariant_observations
                (id, tenant_id, datasource_id, analytic_intent_id, query_id,
                 execution_id, sql_hash, invariant_id, status, observation_json,
                 created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(format!("result-invariant:{execution_id}:{invariant_id}"))
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(query_id)
        .bind(query_id)
        .bind(&execution_id)
        .bind(&sql_hash)
        .bind(&invariant_id)
        .bind(status)
        .bind(observation.to_string())
        .execute(&mut *transaction)
        .await?;
    }
    let updated = sqlx::query::<sqlx::Sqlite>(
        "UPDATE semantic_verifications
         SET verification_json = ?, release_decision = ?,
             calibrated_score = CASE
               WHEN ? = 'Release' THEN MIN(calibrated_score + 0.03, 0.99)
               ELSE MIN(calibrated_score, 0.5)
             END
         WHERE tenant_id = ? AND analytic_intent_id = ?",
    )
    .bind(protected_verification)
    .bind(&release_decision)
    .bind(&release_decision)
    .bind(tenant_id)
    .bind(query_id)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::ValidationError(
            "result validation cannot finalize because the semantic audit is missing".into(),
        ));
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE feedback_regression_cases
         SET execution_evidence_json = ?,
             status = CASE
               WHEN EXISTS (
                 SELECT 1 FROM feedback_learning_events e
                 WHERE e.tenant_id = feedback_regression_cases.tenant_id
                   AND e.id = feedback_regression_cases.feedback_event_id
                   AND e.approved = 1
               ) AND ? = 'Release' THEN 'verified'
               ELSE status
             END,
             last_verified_at = IIF(? = 'Release', CURRENT_TIMESTAMP, last_verified_at),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND datasource_id = ? AND analytic_intent_id = ?
           AND corrected_sql_hash = ? AND status != 'revoked'",
    )
    .bind(execution_evidence.to_string())
    .bind(&release_decision)
    .bind(&release_decision)
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(query_id)
    .bind(&sql_hash)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(ResultValidationOutcome {
        release_decision,
        observation_count: invariants.len(),
    })
}

async fn load_released_query_binding(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    query_id: &str,
    requested_datasource_id: &str,
    requested_sql: &str,
) -> Result<(String, String)> {
    let (datasource_id, released_sql): (String, String) = sqlx::query_as(
        "SELECT data_source_id, generated_sql FROM nl2sql_queries
         WHERE id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(query_id)
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| {
        AppError::ValidationError(
            "SQL execution is blocked because this query has no durable released candidate for the current user"
                .into(),
        )
    })?;
    if datasource_id.trim().is_empty() {
        return Err(AppError::ValidationError(
            "SQL execution is blocked because the released query is not bound to a datasource"
                .into(),
        ));
    }
    if !requested_datasource_id.trim().is_empty() && requested_datasource_id.trim() != datasource_id
    {
        return Err(AppError::ValidationError(
            "SQL execution is blocked because the requested datasource does not match the released query"
                .into(),
        ));
    }
    if requested_sql != released_sql {
        return Err(AppError::ValidationError(
            "SQL execution is blocked because the SQL hash does not match the durable released candidate; generate or explicitly verify a repair instead of editing the statement in place"
                .into(),
        ));
    }
    Ok((datasource_id, released_sql))
}

/// Upserts a row in `nl2sql_conversations` after a successful query.
/// Increments `message_count` and records `last_question`.
pub(crate) async fn upsert_nl2sql_conversation(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    conversation_id: &str,
    question: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO nl2sql_conversations (id, tenant_id, user_id, last_question) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           message_count = message_count + 1, \
           last_question = excluded.last_question, \
           updated_at = CURRENT_TIMESTAMP",
    )
    .bind(conversation_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(question)
    .execute(db)
    .await;
}

/// Calls the LLM to generate a 2-3 sentence summary of the conversation
/// history and updates `nl2sql_conversations.summary`.
pub(crate) async fn generate_conversation_summary(
    state: &AppState,
    claims: &Claims,
    conversation_id: &str,
) {
    let history = load_conversation_history(
        &state.db,
        &claims.tenant_id,
        conversation_id,
        conversation_summary_threshold() as usize,
    )
    .await;

    if history.messages.len() < conversation_summary_threshold() as usize {
        return;
    }

    let chat_cfg = match crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "generate_conversation_summary: failed to resolve chat config: {}",
                e
            );
            return;
        }
    };

    let history_text: String = if history.messages.is_empty() {
        String::new()
    } else {
        history
            .messages
            .iter()
            .rev()
            .map(|(q, a)| serde_json::json!({ "Q": q, "A": a }).to_string())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    #[derive(serde::Serialize)]
    struct ConvSummaryPrompt<'a> {
        history: &'a str,
    }
    let Ok(prompt) = serde_json::to_string(&ConvSummaryPrompt {
        history: &history_text,
    }) else {
        tracing::warn!("generate_conversation_summary: failed to serialize prompt");
        return;
    };

    let request = api::MessageRequest {
        model: chat_cfg.model.clone(),
        max_tokens: 128,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt }],
        }],
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.0),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = match chat_cfg.client.send_message(&request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("generate_conversation_summary: LLM call failed: {}", e);
            return;
        }
    };

    let text_content = response
        .content
        .iter()
        .find_map(|block| match block {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let summary = text_content.trim().to_string();
    if summary.is_empty() {
        tracing::warn!("generate_conversation_summary: LLM returned empty response");
        return;
    }

    let _ = sqlx::query(
        "UPDATE nl2sql_conversations \
         SET summary = ?, summary_version = summary_version + 1, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?",
    )
    .bind(&summary)
    .bind(conversation_id)
    .execute(&state.db)
    .await;
}

/// Attempt to correct a failed SQL query by re-prompting the LLM with error context.
/// Retries up to `MAX_LLM_RETRY_ATTEMPTS` on LLM/network failures.
/// P2-5 BUG-FIX: Re-loads QU constraints and metrics so corrections respect
/// structured constraints from the original generation attempt.
pub(crate) async fn correct_sql(
    state: &AppState,
    claims: &Claims,
    failed_sql: &str,
    error_msg: &str,
    question: &str,
    schema: &serde_json::Value,
    foreign_keys: &[ForeignKeyPrompt],
    join_paths: &[(String, String)],
    conversation_id: &str,
    context: &mut SelfCorrectContext,
    clarification_ctx: Option<&crate::nl2sql::ClarificationContext>,
    db_type: &str,
    datasource_id: &str,
    preferred_model: Option<&str>,
    deadline_bounded: bool,
) -> String {
    context.set_last_decision(None);
    let repair_timeout_secs = std::env::var("NL2SQL_SQL_REPAIR_TOTAL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(75)
        .clamp(15, 180);
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(repair_timeout_secs),
        correct_sql_bounded(
            state,
            claims,
            failed_sql,
            error_msg,
            question,
            schema,
            foreign_keys,
            join_paths,
            conversation_id,
            context,
            clarification_ctx,
            db_type,
            datasource_id,
            preferred_model,
            deadline_bounded,
        ),
    )
    .await
    {
        Ok(corrected) => corrected,
        Err(_) => {
            tracing::warn!(
                datasource_id,
                repair_timeout_secs,
                "correct_sql: bounded repair timed out; returning original SQL"
            );
            context.add(failed_sql.to_string(), error_msg.to_string());
            failed_sql.to_string()
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn correct_sql_bounded(
    state: &AppState,
    claims: &Claims,
    failed_sql: &str,
    error_msg: &str,
    question: &str,
    schema: &serde_json::Value,
    foreign_keys: &[ForeignKeyPrompt],
    join_paths: &[(String, String)],
    conversation_id: &str,
    context: &mut SelfCorrectContext,
    clarification_ctx: Option<&crate::nl2sql::ClarificationContext>,
    db_type: &str,
    datasource_id: &str,
    preferred_model: Option<&str>,
    deadline_bounded: bool,
) -> String {
    let mut chat_candidates = match crate::nl2sql::resolve_chat_config_candidates(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "correct_sql: failed to resolve candidate chat keys; returning original SQL"
            );
            context.add(failed_sql.to_string(), error_msg.to_string());
            return failed_sql.to_string();
        }
    };
    crate::nl2sql::prioritize_chat_candidates(&mut chat_candidates, preferred_model);
    if chat_candidates.is_empty() {
        context.add(failed_sql.to_string(), error_msg.to_string());
        return failed_sql.to_string();
    }
    let preferred_chat_cfg = chat_candidates
        .iter()
        .find(|config| !super::nl2sql_candidate_is_suppressed(&claims.tenant_id, config))
        .unwrap_or(&chat_candidates[0])
        .clone();

    // P2-5 + P1-2 BUG-FIX: Re-load QU result and metrics for correction prompt.
    // QU: re-parse the original question to extract constraints.
    let qu_result: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult> =
        if should_enable_qu() && !conversation_id.is_empty() && !deadline_bounded {
            let qu = crate::nl2sql::query_understanding::QueryUnderstanding::new(
                state.db.clone(),
                preferred_chat_cfg.clone(),
            );
            match qu
                .understand(question, datasource_id, &claims.tenant_id, schema)
                .await
            {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(error = %e, "correct_sql: QU understand() failed, skipping");
                    None
                }
            }
        } else {
            None
        };

    // P1-2: Re-load metrics for this datasource so corrections can use metric definitions.
    let metrics: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT metric_name, expression, filter_conditions FROM nl2sql_metrics \
         WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(datasource_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut reference_snippets = match super::reference::resolve_auto_query_references(
        state,
        &claims.tenant_id,
        datasource_id,
        question,
        super::sql_knowledge_prompt_max_snippets().min(12),
    )
    .await
    {
        Ok(snippets) => snippets,
        Err(e) => {
            tracing::warn!(
                error = %e,
                datasource_id,
                "correct_sql: failed to retrieve SQL knowledge references for correction"
            );
            Vec::new()
        }
    };
    if deadline_bounded {
        reference_snippets =
            super::focus_bounded_sql_knowledge_references(question, &reference_snippets);
    } else if !super::should_enable_sql_generation_tool_loop() {
        let tool_question = format!(
            "{question}\n\nFailed SQL:\n{failed_sql}\n\nDatabase error:\n{error_msg}\n\nFind SQL examples, metric definitions, schema facts, or business rules that help repair this SQL."
        );
        let tool_refs = super::run_sql_knowledge_tool_prefetch(
            state,
            claims,
            datasource_id,
            &tool_question,
            schema,
            &preferred_chat_cfg,
        )
        .await;
        if !tool_refs.is_empty() {
            tracing::info!(
                datasource_id,
                snippet_count = tool_refs.len(),
                "correct_sql: SQL knowledge tool loop retrieved repair context"
            );
            reference_snippets = super::merge_reference_snippets(
                &reference_snippets,
                tool_refs,
                super::sql_knowledge_prompt_max_snippets(),
            );
        }
    }

    let has_alternatives = chat_candidates.len() > 1;
    let candidate_attempts = chat_candidates;
    let total_attempts = candidate_attempts.len();
    for (attempt, chat_cfg) in candidate_attempts.into_iter().enumerate() {
        if has_alternatives
            && attempt + 1 < total_attempts
            && super::nl2sql_candidate_is_suppressed(&claims.tenant_id, &chat_cfg)
        {
            tracing::info!(
                candidate_index = attempt + 1,
                total_candidates = total_attempts,
                provider = %chat_cfg.provider,
                model = %chat_cfg.model,
                "correct_sql: skipping temporarily suppressed candidate"
            );
            continue;
        }
        let metrics_refs: Vec<(String, String, Option<&str>)> = metrics
            .iter()
            .map(|(n, e, f)| (n.clone(), e.clone(), f.as_deref()))
            .collect();
        let result = correct_sql_once(
            state,
            claims,
            &chat_cfg,
            failed_sql,
            error_msg,
            question,
            schema,
            foreign_keys,
            join_paths,
            conversation_id,
            &context.history_text(),
            clarification_ctx,
            db_type,
            qu_result.as_ref(),
            &metrics_refs,
            None,
            &reference_snippets,
            datasource_id,
            deadline_bounded,
        )
        .await;

        match result {
            Ok(candidate) => {
                super::clear_nl2sql_candidate_suppression(&claims.tenant_id, &chat_cfg);
                let decision = candidate.decision;
                context.set_last_decision(decision.clone());
                if candidate.sql.is_empty() {
                    tracing::info!(
                        datasource_id,
                        strategy = ?decision.as_ref().and_then(|value| value.strategy.clone()),
                        "correct_sql: model chose to stop data-availability recovery"
                    );
                    return String::new();
                }
                context.add(candidate.sql.clone(), error_msg.to_string());
                return candidate.sql;
            }
            Err(e) => {
                let error_text = e.to_string();
                tracing::warn!(
                    candidate_index = attempt + 1,
                    total_candidates = total_attempts,
                    provider = %chat_cfg.provider,
                    model = %chat_cfg.model,
                    "correct_sql: LLM call failed on attempt {}: {}",
                    attempt + 1,
                    error_text
                );
                if has_alternatives
                    && (error_text.contains("empty response") || error_text.contains("not safe"))
                {
                    super::suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
                }
            }
        }
    }

    tracing::warn!("correct_sql: all LLM attempts exhausted, returning original SQL");
    context.add(failed_sql.to_string(), error_msg.to_string());
    failed_sql.to_string()
}

#[derive(Debug)]
pub(crate) struct SqlRepairCandidate {
    sql: String,
    decision: Option<SqlRepairDecision>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelRecoveryDecision {
    action: String,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    scope_changed: bool,
    #[serde(default)]
    diagnostic_only: bool,
    #[serde(default)]
    rationale: Option<String>,
}

fn parse_model_recovery_decision(text: &str) -> anyhow::Result<SqlRepairCandidate> {
    let unfenced = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```JSON"))
        .unwrap_or(text.trim())
        .trim()
        .strip_suffix("```")
        .unwrap_or_else(|| {
            text.trim()
                .strip_prefix("```json")
                .or_else(|| text.trim().strip_prefix("```JSON"))
                .unwrap_or(text.trim())
                .trim()
        })
        .trim();
    let json = if serde_json::from_str::<serde_json::Value>(unfenced).is_ok() {
        unfenced
    } else {
        let start = unfenced
            .find('{')
            .ok_or_else(|| anyhow::anyhow!("model recovery decision did not contain JSON"))?;
        let end = unfenced
            .rfind('}')
            .ok_or_else(|| anyhow::anyhow!("model recovery decision JSON was incomplete"))?;
        &unfenced[start..=end]
    };
    let parsed: ModelRecoveryDecision = serde_json::from_str(json)
        .map_err(|error| anyhow::anyhow!("invalid model recovery decision: {error}"))?;
    let action = parsed.action.trim().to_ascii_lowercase();
    if action == "stop" {
        return Ok(SqlRepairCandidate {
            sql: String::new(),
            decision: Some(SqlRepairDecision {
                strategy: parsed.strategy,
                scope_changed: parsed.scope_changed,
                diagnostic_only: true,
                rationale: parsed.rationale,
            }),
        });
    }
    if action != "retry" {
        anyhow::bail!("model recovery action must be retry or stop");
    }
    let sql = parsed.sql.unwrap_or_default();
    let sql = extract_sql_from_llm_output(&sql);
    if sql.is_empty() {
        anyhow::bail!("model recovery retry did not contain SQL");
    }
    match classify_sql(&sql) {
        SqlSafetyResult::Safe => Ok(SqlRepairCandidate {
            sql,
            decision: Some(SqlRepairDecision {
                strategy: parsed.strategy,
                scope_changed: parsed.scope_changed,
                // Any changed scope is diagnostic even if a model forgot the
                // flag. The model chooses the strategy; the server enforces
                // evidence honesty.
                diagnostic_only: parsed.diagnostic_only || parsed.scope_changed,
                rationale: parsed.rationale,
            }),
        }),
        other => anyhow::bail!("model recovery SQL not safe: {other:?}"),
    }
}

/// Single-shot LLM correction call. Does not retry — retry is handled by `correct_sql`.
/// P2-5 + P1-2 BUG-FIX: Accepts qu_result and metrics so corrections respect
/// structured constraints and metric definitions from the original generation attempt.
pub(crate) async fn correct_sql_once(
    state: &AppState,
    claims: &Claims,
    chat_cfg: &crate::nl2sql::ChatTenantConfig,
    failed_sql: &str,
    error_msg: &str,
    question: &str,
    schema: &serde_json::Value,
    foreign_keys: &[ForeignKeyPrompt],
    join_paths: &[(String, String)],
    conversation_id: &str,
    prior_attempts: &str,
    clarification_ctx: Option<&crate::nl2sql::ClarificationContext>,
    db_type: &str,
    qu_result: Option<&crate::nl2sql::query_understanding::QueryUnderstandingResult>,
    metrics: &[(String, String, Option<&str>)],
    _selected_tables: Option<&[String]>,
    reference_snippets: &[super::reference::ReferencePromptSnippet],
    datasource_id: &str,
    deadline_bounded: bool,
) -> anyhow::Result<SqlRepairCandidate> {
    let history = load_conversation_history(&state.db, &claims.tenant_id, conversation_id, 3).await;
    // R-9: weave coreference resolution against the most recent turn into the
    // summary slot so follow-ups like "上月呢" / "排除退货的" carry their
    // implicit context into the LLM prompt without changing the prompt
    // builder signature.
    let enriched_summary = history.enriched_summary(question);
    let mut system_prompt = build_nl2sql_prompt(
        schema,
        foreign_keys,
        join_paths,
        enriched_summary.as_deref(),
        clarification_ctx,
        qu_result, // P2-5: inject QU constraints so corrections respect them
        db_type,
        false,   // P1-4: correction uses full schema
        metrics, // P1-2: inject metrics so corrections respect metric definitions
        _selected_tables,
        reference_snippets,
    );
    let recovery_strategy_allowed =
        SqlExecErrorKind::new(error_msg).allows_model_recovery_strategy();
    if recovery_strategy_allowed {
        system_prompt.push_str(
            "\n\nDATA AVAILABILITY RECOVERY MODE: The original question has already passed clarification. Never return CLARIFICATION_NEEDED. \
             The SQL is valid but the engine reports unavailable partition/file data. \
             Decide from the user question, failed SQL, engine error, schema, SQL knowledge, and prior attempts whether a safe retry is useful. \
             You may preserve the requested scope, narrow or shift partitions, exclude a broken partition, or use a recent available sample only when justified by the evidence. \
             No particular time window or strategy is mandatory. Do not invent an available date. \
             If the retry changes the user's requested business/time scope, set scopeChanged=true and diagnosticOnly=true; that result may validate the query path but cannot answer the original metric. \
             If no honest bounded retry exists, choose action=stop. \
             Return JSON only: {\"action\":\"retry|stop\",\"sql\":\"SELECT ... or null\",\"strategy\":\"short label\",\"scopeChanged\":true|false,\"diagnosticOnly\":true|false,\"rationale\":\"concise reason\"}.",
        );
    } else {
        system_prompt.push_str(
            "\n\nSQL REPAIR MODE: The original question has already passed clarification. \
             Never return CLARIFICATION_NEEDED, prose, Markdown, or an explanation. \
             Correct the failed query using only the supplied schema and evidence, and return exactly one safe executable SELECT or WITH statement. \
             When a referenced column is missing, choose a verified schema column with matching semantics; never invent identifiers.",
        );
    }

    let history_section = if history.messages.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = history
            .messages
            .iter()
            .rev()
            .map(|(q, a)| serde_json::json!({ "Q": q, "A": a }).to_string())
            .collect();
        format!("\nConversation history:\n{}\n\n", lines.join("\n"))
    };

    #[derive(serde::Serialize)]
    struct CorrectionPrompt<'a> {
        history: &'a str,
        prior_attempts: String,
        error_msg: &'a str,
        failed_sql: &'a str,
        question: &'a str,
    }
    let correction_prompt = serde_json::to_string(&CorrectionPrompt {
        history: &history_section,
        prior_attempts: prior_attempts.to_string(),
        error_msg: &error_msg,
        failed_sql: &failed_sql,
        question: &question,
    })
    .map_err(|e| AppError::Internal(e.to_string()))?;

    // SQL repair needs enough room for reasoning models to reach the corrected
    // statement, while never exceeding the configured provider/model ceiling.
    let max_tokens = if deadline_bounded { 4_096 } else { 8_192 }
        .min(chat_cfg.max_output_tokens)
        .max(1);

    let mut extra_body = None;
    if deadline_bounded
        && api::supports_official_deepseek_v4_thinking_control(
            &chat_cfg.model,
            chat_cfg.client.base_url(),
        )
    {
        extra_body = Some(serde_json::Map::from_iter([(
            "thinking".to_string(),
            serde_json::json!({"type": "disabled"}),
        )]));
    }

    let request = MessageRequest {
        model: chat_cfg.model.clone(),
        max_tokens,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text {
                text: correction_prompt,
            }],
        }],
        system: Some(system_prompt),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.0),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body,
    };

    let response_timeout_secs = std::env::var("NL2SQL_SQL_REPAIR_MODEL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(55)
        .clamp(10, 120);
    let (response, tool_reference_snippets) = tokio::time::timeout(
        tokio::time::Duration::from_secs(response_timeout_secs),
        super::send_generation_request_with_sql_tools(
            state,
            claims,
            Some(datasource_id),
            question,
            schema,
            chat_cfg,
            &request,
            !deadline_bounded,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("LLM repair timed out after {response_timeout_secs}s"))?
    .map_err(|e| anyhow::anyhow!("LLM call failed: {}", e))?;
    if !tool_reference_snippets.is_empty() {
        tracing::info!(
            datasource_id,
            snippet_count = tool_reference_snippets.len(),
            "correct_sql_once: SQL generation tool loop read repair context"
        );
    }

    let text_content = super::collect_output_text(&response.content);

    if recovery_strategy_allowed {
        return parse_model_recovery_decision(&text_content);
    }

    let corrected = extract_sql_from_llm_output(&text_content);
    if corrected.is_empty() {
        anyhow::bail!("LLM returned empty response");
    }

    match classify_sql(&corrected) {
        SqlSafetyResult::Safe => Ok(SqlRepairCandidate {
            sql: corrected,
            decision: None,
        }),
        other => {
            tracing::warn!("correct_sql: corrected SQL not safe: {:?}", other);
            anyhow::bail!("corrected SQL not safe: {:?}", other)
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn execute(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<ExecuteResponse>> {
    super::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;

    let mut applied_rules: Vec<super::AppliedRuleHit> = Vec::new();
    let mut applied_rule_seen: BTreeSet<String> = BTreeSet::new();

    match classify_sql(&req.sql) {
        SqlSafetyResult::Safe => {}
        SqlSafetyResult::SyntaxError { message } => {
            return Ok(Json(ExecuteResponse {
                columns: vec![],
                rows: vec![],
                rows_count: 0,
                total_rows: 0,
                has_more: false,
                limit: req.limit,
                offset: req.offset,
                execution_ms: 0,
                error: Some(format!(
                    "[syntax_error] The SQL could not be parsed: {message}"
                )),
                corrected_sql: None,
                self_correct_failed: None,
                diagnostic_only: false,
                recovery_note: None,
                result_score: None,
                warnings: None,
                suggestions: None,
                applied_rules: Vec::new(),
            }));
        }
        SqlSafetyResult::ForbiddenOperation { statement_type } => {
            return Ok(Json(ExecuteResponse {
                columns: vec![],
                rows: vec![],
                rows_count: 0,
                total_rows: 0,
                has_more: false,
                limit: req.limit,
                offset: req.offset,
                execution_ms: 0,
                error: Some(format!(
                    "[forbidden_operation] Only SELECT statements are allowed; {statement_type} is not permitted."
                )),
                corrected_sql: None,
                self_correct_failed: None,
                diagnostic_only: false,
                recovery_note: None,
                result_score: None,
                warnings: None,
                suggestions: None,
                applied_rules: Vec::new(),
            }));
        }
        SqlSafetyResult::MultipleStatements => {
            return Ok(Json(ExecuteResponse {
                columns: vec![],
                rows: vec![],
                rows_count: 0,
                total_rows: 0,
                has_more: false,
                limit: req.limit,
                offset: req.offset,
                execution_ms: 0,
                error: Some(
                    "[multiple_statements] Only a single statement is allowed. \
                     Split your query into separate requests."
                        .to_string(),
                ),
                corrected_sql: None,
                self_correct_failed: None,
                diagnostic_only: false,
                recovery_note: None,
                result_score: None,
                warnings: None,
                suggestions: None,
                applied_rules: Vec::new(),
            }));
        }
        SqlSafetyResult::ForbiddenFunction { function_name } => {
            return Ok(Json(ExecuteResponse {
                columns: vec![],
                rows: vec![],
                rows_count: 0,
                total_rows: 0,
                has_more: false,
                limit: req.limit,
                offset: req.offset,
                execution_ms: 0,
                error: Some(format!(
                    "[forbidden_function] The function {function_name}() is not permitted in NL2SQL queries."
                )),
                corrected_sql: None,
                self_correct_failed: None,
                diagnostic_only: false,
                recovery_note: None,
                result_score: None,
                warnings: None,
                suggestions: None,
                applied_rules: Vec::new(),
            }));
        }
        SqlSafetyResult::ForbiddenIntoClause => {
            return Ok(Json(ExecuteResponse {
                columns: vec![],
                rows: vec![],
                rows_count: 0,
                total_rows: 0,
                has_more: false,
                limit: req.limit,
                offset: req.offset,
                execution_ms: 0,
                error: Some(
                    "[forbidden_into_clause] INTO OUTFILE / INTO DUMPFILE writes to the database server's filesystem and is not permitted."
                        .to_string(),
                ),
                corrected_sql: None,
                self_correct_failed: None,
                diagnostic_only: false,
                recovery_note: None,
                result_score: None,
                warnings: None,
                suggestions: None,
                applied_rules: Vec::new(),
            }));
        }
    }

    let requested_data_source_id = req.data_source_id.trim().to_string();
    let (effective_data_source_id, _released_sql) = load_released_query_binding(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &req.query_id,
        &requested_data_source_id,
        &req.sql,
    )
    .await?;
    let _exec_span = tracing::info_span!(
        "nl2sql_execute",
        tenant_id = %claims.tenant_id,
        requested_datasource_id = %requested_data_source_id,
        datasource_id = %effective_data_source_id,
    );

    // B-06: Secondary schema-based validation — extract referenced tables from the parsed SQL AST.
    // This creates a security audit log entry for every SQL execution, enabling later correlation
    // if the target DB reports "table not found" — useful for detecting probing attacks.
    {
        use sqlparser::ast::Statement;
        use sqlparser::dialect::GenericDialect;
        use sqlparser::parser::Parser;

        let dialect = GenericDialect {};
        if let Ok(statements) = Parser::parse_sql(&dialect, &req.sql) {
            if statements.len() == 1 {
                if let Statement::Query(q) = &statements[0] {
                    // B-06: Extract referenced tables for audit trail via simple FROM extraction.
                    // Parse the SQL to find all table references. We iterate over the body
                    // which is a Box<SetExpr> — only the Select variant has FROM.
                    let mut tables: Vec<String> = Vec::new();
                    if let sqlparser::ast::SetExpr::Select(s) = q.body.as_ref() {
                        for t in &s.from {
                            tables.push(t.relation.to_string().to_lowercase());
                        }
                    }
                    if !tables.is_empty() {
                        tracing::debug!(
                            target_tables = ?tables,
                            query_id = %req.query_id,
                            "B-06: SQL table references for audit trail"
                        );
                    }
                }
            }
        }
    }

    let db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &effective_data_source_id,
    )
    .await?;

    // Hard guardrail: enforce query policy at execution time as well.
    // This blocks direct /execute calls and SQL edits that bypass /query stage checks.
    let exec_target_tables = extract_tables_from_sql(&req.sql);
    let exec_target_columns = extract_columns_from_sql(&req.sql);
    let exec_policy_decision = enforce_query_policy(
        &state.db,
        &claims.tenant_id,
        &effective_data_source_id,
        &claims.sub,
        &claims.email,
        &exec_target_tables,
        &exec_target_columns,
    )
    .await?;
    if exec_policy_decision.is_denied() {
        let error = query_policy_denial_message(&exec_policy_decision);
        return Ok(Json(ExecuteResponse {
            columns: Vec::new(),
            rows: Vec::new(),
            rows_count: 0,
            total_rows: 0,
            has_more: false,
            limit: req.limit,
            offset: req.offset,
            execution_ms: 0,
            error: Some(error),
            corrected_sql: None,
            self_correct_failed: None,
            diagnostic_only: false,
            recovery_note: None,
            result_score: None,
            warnings: None,
            suggestions: None,
            applied_rules: Vec::new(),
        }));
    }
    if exec_policy_decision.had_policy {
        push_unique_rule_hit(
            &mut applied_rules,
            &mut applied_rule_seen,
            "query_policy_applied_execute",
            "Query Policy (Execute Guard)",
            Some("execution-time policy guard applied".to_string()),
        );
    }
    let policy_row_filter = exec_policy_decision.row_filter_expr.clone();
    if let Some(row_filter) = policy_row_filter.as_deref() {
        push_unique_rule_hit(
            &mut applied_rules,
            &mut applied_rule_seen,
            "query_policy_row_filter_enforced_execute",
            "Query Policy Row Filter (Execute Guard)",
            Some(row_filter.to_string()),
        );
    }

    // Reject non-executable db_types before we touch the DB or spin up
    // a connection pool. This replaces the old behaviour where `http_api`,
    // `mcp` and `hive` rows would pass access validation only to hit
    // `Unimplemented` in a downstream branch — giving users the confusing
    // "permission OK but executing failed" experience. Now the rejection
    // is upfront and self-describing.
    if !matches!(
        db_type.as_str(),
        "mysql" | "tidb" | "postgres" | "clickhouse" | "presto" | "trino" | "mongodb"
    ) {
        return Err(AppError::ValidationError(format!(
            "execution not supported for db_type: {db_type}. Supported types: mysql, tidb, postgres, clickhouse, presto, trino, mongodb."
        )));
    }

    // B-11: Fetch config (and confirm db_type) in a single query — avoids double round-trip.
    // `updated_at` doubles as the pool-cache version: any credential/host change
    // flips it forward and the cached pool transparently rolls over.
    let row = sqlx::query(
        "SELECT db_type, config, schema_info, sensitive_columns, updated_at FROM data_sources WHERE id = ?",
    )
    .bind(&effective_data_source_id)
    .fetch_optional(&state.db)
    .await?;

    let (db_type, config_json, sensitive_columns, ds_version): (
        String,
        serde_json::Value,
        serde_json::Value,
        i64,
    ) = match row {
        Some(r) => {
            let updated_at: chrono::NaiveDateTime = r
                .try_get("updated_at")
                .unwrap_or_else(|_| chrono::Utc::now().naive_utc());
            (
                r.get("db_type"),
                r.get("config"),
                r.get("sensitive_columns"),
                updated_at.and_utc().timestamp_millis(),
            )
        }
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    let datasource_sensitive_patterns: Vec<String> = match sensitive_columns {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    if !datasource_sensitive_patterns.is_empty() {
        push_unique_rule_hit(
            &mut applied_rules,
            &mut applied_rule_seen,
            "datasource_sensitive_masking",
            "Datasource Sensitive Masking",
            Some(format!(
                "{} pattern(s) configured",
                datasource_sensitive_patterns.len()
            )),
        );
    }

    // Load schema info for self-correction (P1-1).
    // Done here so we have it available if a schema error occurs.
    let schema_info: serde_json::Value = {
        let row = sqlx::query("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(&effective_data_source_id)
            .fetch_optional(&state.db)
            .await?;
        match row {
            Some(r) => r
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or(serde_json::json!({"tables": [], "foreign_keys": []})),
            None => return Err(AppError::NotFound("data source not found".into())),
        }
    };
    let (schema_tables, foreign_keys) = extract_schema_tables_and_fks(&schema_info);
    let mut foreign_key_prompts: Vec<ForeignKeyPrompt> = foreign_keys
        .into_iter()
        .map(|fk| ForeignKeyPrompt {
            source_table: fk.source_table,
            source_column: fk.source_column,
            source_type: fk.source_column_type,
            target_table: fk.target_table,
            target_column: fk.target_column,
            target_type: fk.target_column_type,
        })
        .collect();

    // Append manual user-defined FKs (user-defined FKs take precedence).
    let manual_fks =
        load_manual_foreign_keys(&state.db, &claims.tenant_id, &effective_data_source_id).await;
    foreign_key_prompts.extend(manual_fks);

    // Load pre-computed JOIN paths for SQL correction prompts.
    let join_paths = load_join_paths_for_datasource(&state.db, &effective_data_source_id).await;

    // P1-1: Self-correction loop — retry schema errors up to MAX_SELF_CORRECT_ATTEMPTS.
    let mut current_sql = match policy_row_filter.as_deref() {
        Some(row_filter) => {
            super::inject_query_policy_row_filter(&req.sql, row_filter).map_err(|error| {
                AppError::ValidationError(format!(
                    "query policy row filter could not be applied safely: {error}"
                ))
            })?
        }
        None => req.sql.clone(),
    };
    let mut current_candidate = verify_candidate_against_canonical_intent(
        &state.db,
        &claims.tenant_id,
        &req.query_id,
        Some(&effective_data_source_id),
        &current_sql,
    )
    .await?;
    push_unique_rule_hit(
        &mut applied_rules,
        &mut applied_rule_seen,
        "semantic_release_guard_execute",
        "Semantic Release Guard",
        Some("canonical intent, tenant, datasource and SQL candidate re-verified before datasource connection".into()),
    );
    let mut corrected_sql: Option<String> = None;
    let mut self_correct_failed = false;
    let mut current_repair_decision: Option<SqlRepairDecision> = None;
    let mut attempts = 0;
    let mut operational_attempts = 0;
    // COUNT, SELECT, and any repair attempts share one concurrency gate. A
    // completed statement releases its slot; retry counts and timeouts remain
    // bounded separately by the self-correction policy below.
    let request_budget = crate::routes::nl2sql::agent_executor::DatasourceRequestBudget::new(3);
    let mut correct_context = SelfCorrectContext::default();
    let timeout_secs = req.timeout_seconds.unwrap_or(30);

    let masking_rules = crate::routes::nl2sql::masking_rules::load_active_rules(
        &state.db,
        &claims.tenant_id,
        &effective_data_source_id,
    )
    .await;
    if !masking_rules.is_empty() {
        push_unique_rule_hit(
            &mut applied_rules,
            &mut applied_rule_seen,
            "tenant_masking_rules_loaded",
            "Tenant Masking Rules",
            Some(format!("{} active rule(s)", masking_rules.len())),
        );
    }

    let primary_table_hint = crate::routes::nl2sql::extract_top_level_tables(&req.sql)
        .into_iter()
        .next()
        .unwrap_or_default();

    loop {
        let sql_result = execute_once(
            &state,
            &claims,
            &req.query_id,
            &current_sql,
            &effective_data_source_id,
            &db_type,
            &config_json,
            ds_version,
            timeout_secs,
            req.limit,
            req.offset,
            &datasource_sensitive_patterns,
            request_budget.clone(),
        )
        .await;

        match sql_result {
            Ok(resp) => {
                let result_validation = finalize_result_invariants(
                    &state.db,
                    &claims.tenant_id,
                    &effective_data_source_id,
                    &req.query_id,
                    &current_sql,
                    &resp.rows,
                    resp.columns.len(),
                    resp.execution_ms,
                    &current_candidate,
                )
                .await?;
                if result_validation.release_decision != "Release" {
                    return Err(AppError::ValidationError(format!(
                        "query result did not pass semantic release (decision={}; result_invariants={})",
                        result_validation.release_decision,
                        result_validation.observation_count,
                    )));
                }
                // Enrich masking rule hit details based on actual returned columns
                // (more reliable than SQL parse for aliases/expressions).
                if !masking_rules.is_empty() && !resp.columns.is_empty() {
                    let mut masked_cols: Vec<String> = Vec::new();
                    let mut mask_types: BTreeSet<String> = BTreeSet::new();
                    for col in &resp.columns {
                        if let Some(rule) =
                            crate::routes::nl2sql::masking_rules::find_first_matching_rule(
                                &masking_rules,
                                &primary_table_hint,
                                &col.name,
                            )
                        {
                            masked_cols.push(col.name.clone());
                            mask_types.insert(rule.mask_type.clone());
                        }
                    }
                    if !masked_cols.is_empty() {
                        let preview = masked_cols
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        let suffix = if masked_cols.len() > 3 {
                            format!(" (+{} more)", masked_cols.len() - 3)
                        } else {
                            String::new()
                        };
                        let mask_type_text = if mask_types.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; masks={}",
                                mask_types.into_iter().collect::<Vec<_>>().join("|")
                            )
                        };
                        push_unique_rule_hit(
                            &mut applied_rules,
                            &mut applied_rule_seen,
                            "tenant_masking_rule_applied",
                            "Tenant Masking Rule Applied",
                            Some(format!(
                                "table={}; columns={}{}{}",
                                if primary_table_hint.is_empty() {
                                    "(unknown)"
                                } else {
                                    &primary_table_hint
                                },
                                preview,
                                suffix,
                                mask_type_text
                            )),
                        );
                    }
                }

                if !datasource_sensitive_patterns.is_empty() && !resp.columns.is_empty() {
                    let sensitive_cols = resp
                        .columns
                        .iter()
                        .filter(|c| {
                            crate::routes::nl2sql::is_datasource_sensitive(
                                &c.name,
                                &datasource_sensitive_patterns,
                            )
                        })
                        .map(|c| c.name.clone())
                        .collect::<Vec<_>>();
                    if !sensitive_cols.is_empty() {
                        let preview = sensitive_cols
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        let suffix = if sensitive_cols.len() > 3 {
                            format!(" (+{} more)", sensitive_cols.len() - 3)
                        } else {
                            String::new()
                        };
                        push_unique_rule_hit(
                            &mut applied_rules,
                            &mut applied_rule_seen,
                            "datasource_sensitive_masking",
                            "Datasource Sensitive Masking",
                            Some(format!("columns={}{}", preview, suffix)),
                        );
                    }
                }

                // Record routing features (query count) for matched tables so future
                // routing can boost popular tables via `frequency_boost()`.
                let tables_to_record: Vec<String> = schema_tables
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.get("table_name")?.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                if !tables_to_record.is_empty() {
                    let tenant_id = claims.tenant_id.clone();
                    let ds_id = effective_data_source_id.clone();
                    let db_for_stats = state.db.clone();
                    let tables = tables_to_record.clone();
                    // B-05: Background feature recording task — log if the task itself fails to complete.
                    tokio::spawn(async move {
                        record_routing_features(&db_for_stats, &tenant_id, &ds_id, &tables).await;
                    });
                }

                // ── Result Validation (optional, controlled by env) ────────────────────
                let (val_score, val_warnings, val_suggestions) =
                    if should_enable_result_validation() {
                        let validator =
                            crate::nl2sql::result_validator::ResultValidator::new(state.db.clone());
                        let result_columns = resp
                            .columns
                            .iter()
                            .map(|c| c.name.clone())
                            .collect::<Vec<_>>();
                        let validation_tables =
                            derive_validation_tables(&current_sql, &primary_table_hint);

                        let mut merged_warnings: Vec<
                            crate::nl2sql::result_validator::ValidationWarning,
                        > = Vec::new();
                        let mut merged_suggestions: Vec<String> = Vec::new();
                        let mut score_acc = 0.0f32;
                        let mut score_count = 0usize;

                        let mut validation_failed = false;
                        for table_name in &validation_tables {
                            match validator
                                .validate(
                                    &claims.tenant_id,
                                    &effective_data_source_id,
                                    table_name,
                                    &resp.rows,
                                    &result_columns,
                                )
                                .await
                            {
                                Ok(v) => {
                                    score_acc += v.score;
                                    score_count += 1;
                                    merged_warnings.extend(v.warnings);
                                    merged_suggestions.extend(v.suggestions);
                                }
                                Err(e) => {
                                    validation_failed = true;
                                    tracing::warn!(
                                        error = %e,
                                        table = %table_name,
                                        "execute: result validation failed for table"
                                    );
                                }
                            }
                        }

                        if validation_failed && score_count == 0 {
                            (None, None, None)
                        } else {
                            if !merged_warnings.is_empty() {
                                push_unique_rule_hit(
                                    &mut applied_rules,
                                    &mut applied_rule_seen,
                                    "result_validation_warning",
                                    "Result Validation Warning",
                                    Some(format!(
                                        "warnings={}, tables={}",
                                        merged_warnings.len(),
                                        validation_tables.join(", ")
                                    )),
                                );
                            } else if !validation_tables.is_empty() {
                                push_unique_rule_hit(
                                    &mut applied_rules,
                                    &mut applied_rule_seen,
                                    "result_validation_passed",
                                    "Result Validation Passed",
                                    Some(format!("tables={}", validation_tables.join(", "))),
                                );
                            }
                            (
                                if score_count > 0 {
                                    Some(score_acc / score_count as f32)
                                } else {
                                    None
                                },
                                if merged_warnings.is_empty() {
                                    None
                                } else {
                                    Some(merged_warnings)
                                },
                                if merged_suggestions.is_empty() {
                                    None
                                } else {
                                    Some(merged_suggestions)
                                },
                            )
                        }
                    } else {
                        (None, None, None)
                    };

                // Update result cache with actual rows for the explain endpoint.
                // Keep this synchronous so users who immediately click "Explain"
                // always see the latest execution result (avoid race with background task).
                crate::nl2sql::result_cache::update_snapshot(
                    &state.db,
                    &claims.tenant_id,
                    &req.query_id,
                    &effective_data_source_id,
                    &current_sql,
                    &resp.rows,
                )
                .await;

                if let Err(e) =
                    sqlx::query("UPDATE nl2sql_queries SET applied_rules_json = ? WHERE id = ?")
                        .bind(super::applied_rules_json_value(&applied_rules))
                        .bind(&req.query_id)
                        .execute(&state.db)
                        .await
                {
                    tracing::warn!(
                        error = %e,
                        query_id = %req.query_id,
                        "failed to persist execute-stage applied rules"
                    );
                }

                return Ok(Json(ExecuteResponse {
                    columns: resp.columns,
                    rows: resp.rows,
                    rows_count: resp.rows_count,
                    total_rows: resp.total_rows,
                    has_more: false,
                    limit: i64::try_from(resp.rows_count).unwrap_or(i64::MAX),
                    offset: 0,
                    execution_ms: resp.execution_ms,
                    error: resp.error,
                    corrected_sql,
                    self_correct_failed: if self_correct_failed {
                        Some(true)
                    } else {
                        None
                    },
                    diagnostic_only: current_repair_decision
                        .as_ref()
                        .is_some_and(|decision| decision.diagnostic_only),
                    recovery_note: current_repair_decision.as_ref().and_then(|decision| {
                        decision
                            .rationale
                            .clone()
                            .or_else(|| decision.strategy.clone())
                    }),
                    result_score: val_score,
                    warnings: val_warnings,
                    suggestions: val_suggestions,
                    applied_rules: applied_rules.clone(),
                }));
            }
            Err(exec_err) => {
                let err_msg = exec_err.to_string();
                let err_kind = SqlExecErrorKind::new(&err_msg);

                if err_kind.is_transient_operational()
                    && operational_attempts < max_operational_retry_attempts()
                {
                    operational_attempts += 1;
                    let delay_secs = 1u64 << operational_attempts.saturating_sub(1).min(3);
                    tracing::warn!(
                        query_id = %req.query_id,
                        datasource_id = %effective_data_source_id,
                        attempt = operational_attempts,
                        max_attempts = max_operational_retry_attempts(),
                        delay_secs,
                        error = %err_msg,
                        "execute: transient datasource failure; retrying unchanged SQL"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    continue;
                }

                if err_kind.is_retryable() && attempts < max_self_correct_attempts() {
                    attempts += 1;
                    tracing::warn!(
                        query_id = %req.query_id,
                        datasource_id = %effective_data_source_id,
                        attempt = attempts,
                        max_attempts = max_self_correct_attempts(),
                        error = %err_msg,
                        "execute: SQL failed with schema error, attempting correction"
                    );

                    // Fetch the question from nl2sql_queries if not provided
                    let question: String =
                        sqlx::query_scalar("SELECT question FROM nl2sql_queries WHERE id = ?")
                            .bind(&req.query_id)
                            .fetch_optional(&state.db)
                            .await
                            .unwrap_or(None)
                            .unwrap_or_else(|| "SQL query".to_string());

                    let conversation_id: String = sqlx::query_scalar(
                        "SELECT COALESCE(conversation_id, '') FROM nl2sql_queries WHERE id = ?",
                    )
                    .bind(&req.query_id)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None)
                    .unwrap_or_default();

                    let new_sql = correct_sql(
                        &state,
                        &claims,
                        &current_sql,
                        &err_msg,
                        &question,
                        &schema_tables,
                        &foreign_key_prompts,
                        &join_paths,
                        &conversation_id,
                        &mut correct_context,
                        None,
                        &db_type,
                        &effective_data_source_id,
                        None,
                        false,
                    )
                    .await;
                    let repair_decision = correct_context.take_last_decision();
                    current_repair_decision = repair_decision;

                    if !new_sql.trim().is_empty() && new_sql != current_sql {
                        let candidate_sql = match policy_row_filter.as_deref() {
                            Some(row_filter) => {
                                super::inject_query_policy_row_filter(&new_sql, row_filter)
                                    .map_err(|error| {
                                        AppError::ValidationError(format!(
                                            "query policy row filter could not be reapplied after SQL correction: {error}"
                                        ))
                                    })?
                            }
                            None => new_sql,
                        };
                        match verify_repair_against_canonical_intent(
                            &state.db,
                            &claims.tenant_id,
                            &req.query_id,
                            &candidate_sql,
                        )
                        .await
                        {
                            Err(error) => {
                                tracing::warn!(
                                    tenant_id = %claims.tenant_id,
                                    query_id = %req.query_id,
                                    error = %error,
                                    "execute: repaired SQL failed canonical semantic verification"
                                );
                                push_unique_rule_hit(
                                    &mut applied_rules,
                                    &mut applied_rule_seen,
                                    "semantic_repair_blocked",
                                    "Semantic Repair Verification",
                                    Some(error.to_string()),
                                );
                                self_correct_failed = true;
                            }
                            Ok(candidate) => {
                                current_candidate = candidate;
                                current_sql = candidate_sql;
                                corrected_sql = Some(current_sql.clone());
                                continue;
                            }
                        }
                    } else {
                        self_correct_failed = true;
                    }
                } else if !err_kind.is_retryable() {
                    self_correct_failed = true;
                }

                if let Err(e) =
                    sqlx::query("UPDATE nl2sql_queries SET applied_rules_json = ? WHERE id = ?")
                        .bind(super::applied_rules_json_value(&applied_rules))
                        .bind(&req.query_id)
                        .execute(&state.db)
                        .await
                {
                    tracing::warn!(
                        error = %e,
                        query_id = %req.query_id,
                        "failed to persist failed execute-stage applied rules"
                    );
                }

                // Return the error (either non-retryable, exhausted retries, or correction failed)
                return Ok(Json(ExecuteResponse {
                    columns: vec![],
                    rows: vec![],
                    rows_count: 0,
                    total_rows: 0,
                    has_more: false,
                    limit: req.limit,
                    offset: req.offset,
                    execution_ms: 0,
                    error: Some(err_msg),
                    corrected_sql,
                    self_correct_failed: if self_correct_failed {
                        Some(true)
                    } else {
                        None
                    },
                    diagnostic_only: false,
                    recovery_note: current_repair_decision.as_ref().and_then(|decision| {
                        decision
                            .rationale
                            .clone()
                            .or_else(|| decision.strategy.clone())
                    }),
                    result_score: None,
                    warnings: None,
                    suggestions: None,
                    applied_rules: applied_rules.clone(),
                }));
            }
        }
    }
}

/// Inner execution without self-correction loop.
/// Separated so the retry logic in `execute` can call it cleanly.
struct ExecOnceResult {
    columns: Vec<ColumnInfo>,
    rows: Vec<serde_json::Value>,
    rows_count: usize,
    total_rows: i64,
    execution_ms: u64,
    error: Option<String>,
}

struct ActiveQueryGuard {
    query_id: String,
}

impl ActiveQueryGuard {
    fn register(handle: ActiveQueryHandle) -> Self {
        let query_id = handle.query_id.clone();
        register_active_query(handle);
        Self { query_id }
    }
}

impl Drop for ActiveQueryGuard {
    fn drop(&mut self) {
        unregister_active_query(&self.query_id);
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_once(
    state: &AppState,
    claims: &Claims,
    query_id: &str,
    sql: &str,
    data_source_id: &str,
    db_type: &str,
    config_json: &serde_json::Value,
    ds_version: i64,
    timeout_secs: u32,
    _limit: i64,
    _offset: i64,
    datasource_sensitive_patterns: &[String],
    request_budget: std::sync::Arc<crate::routes::nl2sql::agent_executor::DatasourceRequestBudget>,
) -> Result<ExecOnceResult> {
    let start = std::time::Instant::now();
    // Normalize trailing separators once so every dialect-specific execution
    // path uses a SQL string safe for subquery wrapping (`SELECT COUNT(*) FROM (...)`).
    let normalized_sql = normalize_sql_for_execution(sql);

    // R-7/R-8: load tenant column-masking rules once up-front so each row
    // applies them deterministically. The primary FROM-table is extracted
    // by a cheap regex pass and threaded into per-row matches; multi-table
    // joins fall back to wildcard `%` rules.
    let masking_rules = crate::routes::nl2sql::masking_rules::load_active_rules(
        &state.db,
        &claims.tenant_id,
        data_source_id,
    )
    .await;
    let primary_table_hint = crate::routes::nl2sql::extract_top_level_tables(&normalized_sql)
        .into_iter()
        .next()
        .unwrap_or_default();

    match db_type {
        "mysql" | "tidb" => {
            #[derive(Debug, serde::Deserialize)]
            struct SqlConfig {
                host: String,
                port: u16,
                database: String,
                username: String,
                password: String,
            }
            let config_val = decrypt_config(config_json, &state.data_dir)?;
            let cfg: SqlConfig = serde_json::from_value(config_val)
                .map_err(|_| AppError::ValidationError("invalid config".into()))?;

            let url = crate::routes::data_sources::build_mysql_url_parts(
                &cfg.username,
                &cfg.password,
                &cfg.host,
                cfg.port,
                &cfg.database,
            );
            // Pool cache: keyed by (tenant_id, data_source_id, updated_at).
            // A credential/host edit on data_sources bumps updated_at and the
            // old pool is automatically evicted on the next call.
            let pool = state
                .nl2sql_pool_cache
                .get_mysql(&claims.tenant_id, data_source_id, ds_version, &url)
                .await
                .map_err(|e| {
                    // Defensive: a corrupt cached pool is force-evicted so the
                    // next caller starts clean.
                    state
                        .nl2sql_pool_cache
                        .evict(&claims.tenant_id, data_source_id, ds_version);
                    AppError::Internal(format!("mysql connection failed: {e}"))
                })?;
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| AppError::Internal(format!("mysql acquire failed: {e}")))?;
            let thread_id = sqlx::query("SELECT CONNECTION_ID()")
                .fetch_one(&mut *conn)
                .await
                .ok()
                .and_then(|row| row.try_get::<u64, _>(0).ok());
            let _active_query_guard = ActiveQueryGuard::register(ActiveQueryHandle {
                tenant_id: claims.tenant_id.clone(),
                data_source_id: data_source_id.to_string(),
                db_type: db_type.to_string(),
                query_id: query_id.to_string(),
                mysql_thread_id: thread_id,
                postgres_pid: None,
            });

            // COUNT query using subquery to count against the same WHERE clause
            let count_sql = format!(
                "SELECT CAST(COUNT(*) AS SIGNED) FROM ({}) AS t",
                normalized_sql
            );
            let count_row = {
                let _count_permit = request_budget
                    .acquire("MySQL count query")
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs as u64),
                    sqlx::query(&count_sql).fetch_one(&mut *conn),
                )
                .await
                {
                    Ok(result) => result
                        .map_err(|e| AppError::Internal(format!("count query failed: {e}")))?,
                    Err(_) => {
                        conn.close_on_drop();
                        return Err(AppError::Internal("Count query timed out".to_string()));
                    }
                }
            };
            let total_rows: i64 = count_row.get(0);

            // Execute exactly the SQL shown in the editor. Do not append hidden
            // LIMIT/OFFSET clauses; users must be able to see and edit the real SQL.
            // We still inject the MySQL optimizer hint because it is not a result
            // shaping clause and prevents server-side runaway execution.
            let hinted_sql = inject_mysql_max_execution_time(
                &normalized_sql,
                (timeout_secs as u64).saturating_mul(1000),
            );
            let mut sql_rows = {
                let _query_permit = request_budget
                    .acquire("MySQL result query")
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs as u64),
                    sqlx::query(&hinted_sql).fetch_all(&mut *conn),
                )
                .await
                {
                    Ok(result) => result.map_err(|e| AppError::Internal(e.to_string()))?,
                    Err(_) => {
                        conn.close_on_drop();
                        return Err(AppError::Internal("Query execution timed out".to_string()));
                    }
                }
            };

            // Belt-and-suspenders: enforce the hard row cap even when the SQL
            // carried its own LIMIT larger than our ceiling.
            enforce_row_cap_after_fetch(&mut sql_rows);

            // NOTE: do NOT call `pool.close()` — the pool is shared via the
            // cache and will be reused by the next execution. The cache's
            // idle-timeout reaps unused pools.

            let columns: Vec<ColumnInfo> = if sql_rows.is_empty() {
                vec![]
            } else {
                sql_rows[0]
                    .columns()
                    .iter()
                    .map(|c| ColumnInfo {
                        name: c.name().to_string(),
                        type_: "text".to_string(),
                        masked: Some(is_datasource_sensitive(
                            c.name(),
                            datasource_sensitive_patterns,
                        )),
                    })
                    .collect()
            };

            let json_rows: Vec<serde_json::Value> = sql_rows
                .iter()
                .map(|row| {
                    let mut map = serde_json::Map::new();
                    for (i, col) in row.columns().iter().enumerate() {
                        let raw_value = decode_mysql_cell(row, i);
                        // Step 1: built-in / datasource sensitive-column masking.
                        let after_sensitive =
                            if is_datasource_sensitive(col.name(), datasource_sensitive_patterns) {
                                mask_sensitive_value(&raw_value)
                            } else {
                                raw_value
                            };
                        // Step 2: tenant-defined column masking rules (R-8).
                        // Higher precedence than sensitive defaults — if a rule
                        // matches, its outcome wins.
                        let final_value =
                            crate::routes::nl2sql::masking_rules::apply_rules_to_value(
                                &masking_rules,
                                &claims.tenant_id,
                                &primary_table_hint,
                                col.name(),
                                &after_sensitive,
                            );
                        map.insert(col.name().to_string(), final_value);
                    }
                    serde_json::Value::Object(map)
                })
                .collect();

            let rows_count = json_rows.len();

            sqlx::query(
                "UPDATE nl2sql_queries SET executed = 1, rows_returned = ?, execution_ms = ? WHERE id = ?",
            )
            .bind(i32::try_from(total_rows).unwrap_or(i32::MAX))
            .bind(i32::try_from(start.elapsed().as_millis()).unwrap_or(i32::MAX))
            .bind(query_id)
            .execute(&state.db)
            .await?;

            Ok(ExecOnceResult {
                columns,
                rows: json_rows,
                rows_count,
                total_rows,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: None,
            })
        }
        "clickhouse" => {
            #[derive(Debug, serde::Deserialize)]
            struct ClickHouseConfig {
                host: String,
                port: u16,
                database: String,
                username: String,
                password: String,
            }
            let config_val = decrypt_config(config_json, &state.data_dir)?;
            let cfg: ClickHouseConfig = serde_json::from_value(config_val)
                .map_err(|_| AppError::ValidationError("invalid clickhouse config".into()))?;

            let addr = format!("http://{}:{}", cfg.host, cfg.port);
            let client = clickhouse::Client::default()
                .with_url(&addr)
                .with_user(&cfg.username)
                .with_password(&cfg.password)
                .with_database(&cfg.database);

            // ClickHouse returns count through JSON parsing here, not MySQL unsigned BIGINT decoding.
            let count_sql = format!("SELECT COUNT(*) FROM ({}) AS t", normalized_sql);
            let count_row: serde_json::Value =
                tokio::time::timeout(std::time::Duration::from_secs(timeout_secs as u64), async {
                    let cursor = client
                        .query(&count_sql)
                        .fetch_bytes("JSONEachRow")
                        .map_err(|e| AppError::Internal(format!("clickhouse count failed: {e}")))?;
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = cursor.lines();
                    if let Some(line) = lines
                        .next_line()
                        .await
                        .map_err(|e| AppError::Internal(e.to_string()))?
                    {
                        serde_json::from_str(&line).map_err(|e| {
                            AppError::Internal(format!("clickhouse count parse failed: {e}"))
                        })
                    } else {
                        Ok(serde_json::json!({}))
                    }
                })
                .await
                .map_err(|_| AppError::Internal("Count query timed out".to_string()))?
                .map_err(|e| AppError::Internal(e.to_string()))?;
            let total_rows: i64 = count_row
                .as_object()
                .and_then(|m| m.values().next())
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            // Execute exactly the SQL shown in the editor. Do not append hidden
            // LIMIT/OFFSET clauses.
            let rows_data: Vec<serde_json::Value> =
                tokio::time::timeout(std::time::Duration::from_secs(timeout_secs as u64), async {
                    let cursor = client
                        .query(&normalized_sql)
                        .fetch_bytes("JSONEachRow")
                        .map_err(|e| AppError::Internal(format!("clickhouse query failed: {e}")))?;

                    use tokio::io::AsyncBufReadExt;
                    let mut lines = cursor.lines();
                    let mut rows: Vec<serde_json::Value> = Vec::new();
                    while let Some(line) = lines
                        .next_line()
                        .await
                        .map_err(|e| AppError::Internal(e.to_string()))?
                    {
                        if line.is_empty() {
                            continue;
                        }
                        let value: serde_json::Value =
                            serde_json::from_str(&line).map_err(|e| {
                                AppError::Internal(format!("clickhouse parse failed: {e}"))
                            })?;
                        rows.push(value);
                    }
                    Ok::<_, AppError>(rows)
                })
                .await
                .map_err(|_| AppError::Internal("Query execution timed out".to_string()))?
                .map_err(|e| AppError::Internal(e.to_string()))?;

            let columns: Vec<ColumnInfo> = if rows_data.is_empty() {
                vec![]
            } else if let Some(first) = rows_data[0].as_object() {
                first
                    .keys()
                    .map(|name| ColumnInfo {
                        name: name.clone(),
                        type_: "text".to_string(),
                        masked: Some(is_datasource_sensitive(name, datasource_sensitive_patterns)),
                    })
                    .collect()
            } else {
                vec![]
            };

            let json_rows: Vec<serde_json::Value> = rows_data
                .into_iter()
                .map(|row| {
                    if let serde_json::Value::Object(mut map) = row {
                        for (key, value) in map.iter_mut() {
                            if is_datasource_sensitive(key, datasource_sensitive_patterns) {
                                *value = mask_sensitive_value(value);
                            }
                        }
                        serde_json::Value::Object(map)
                    } else {
                        row
                    }
                })
                .collect();

            let rows_count = json_rows.len();

            sqlx::query(
                "UPDATE nl2sql_queries SET executed = 1, rows_returned = ?, execution_ms = ? WHERE id = ?",
            )
            .bind(i32::try_from(total_rows).unwrap_or(i32::MAX))
            .bind(i32::try_from(start.elapsed().as_millis()).unwrap_or(i32::MAX))
            .bind(query_id)
            .execute(&state.db)
            .await?;

            Ok(ExecOnceResult {
                columns,
                rows: json_rows,
                rows_count,
                total_rows,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: None,
            })
        }
        "presto" | "trino" => {
            #[derive(Debug, serde::Deserialize)]
            struct TrinoConfig {
                host: String,
                port: u16,
                catalog: String,
                schema: String,
                username: String,
                #[serde(default)]
                password: String,
                #[serde(default)]
                ssl: Option<bool>,
                #[serde(default)]
                basic_auth: Option<bool>,
            }
            let config_val = decrypt_config(config_json, &state.data_dir)?;
            let cfg: TrinoConfig = serde_json::from_value(config_val)
                .map_err(|_| AppError::ValidationError("invalid trino/presto config".into()))?;

            let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(&cfg.host);
            let port = normalized_host.port.unwrap_or(cfg.port);
            let secure = cfg.ssl.or(normalized_host.secure).unwrap_or(port == 443);
            let mut builder =
                trino_rust_client::ClientBuilder::new(&cfg.username, &normalized_host.host)
                    .port(port)
                    .catalog(&cfg.catalog)
                    .schema(&cfg.schema)
                    .secure(secure);
            if cfg.basic_auth.unwrap_or(!cfg.password.is_empty()) {
                builder = builder.auth(trino_rust_client::auth::Auth::Basic(
                    cfg.username.clone(),
                    Some(cfg.password.clone()),
                ));
            }
            let cli = builder
                .max_attempt(0)
                .build()
                .map_err(|e| AppError::Internal(format!("trino client build failed: {e}")))?;

            // Execute exactly the SQL shown in the editor. Do not append hidden
            // LIMIT/OFFSET clauses.
            let dataset = crate::routes::nl2sql::agent_executor::execute_trino_query_bounded(
                cli,
                normalized_sql.clone(),
                timeout_secs as u64,
                &claims.tenant_id,
                &claims.sub,
                "Trino result query",
                request_budget,
            )
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

            let (types, rows) = dataset.split();
            let column_count = types.len();
            let columns: Vec<ColumnInfo> = types
                .iter()
                .map(|(name, _)| ColumnInfo {
                    name: name.clone(),
                    type_: "text".to_string(),
                    masked: Some(is_datasource_sensitive(name, datasource_sensitive_patterns)),
                })
                .collect();

            let json_rows: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|row| {
                    let row_data: Vec<serde_json::Value> = row.into_json();
                    let mut map = serde_json::Map::new();
                    for (i, v) in row_data.into_iter().enumerate() {
                        let (col_name, final_value) = if i < column_count {
                            let name = types[i].0.clone();
                            let val =
                                if is_datasource_sensitive(&name, datasource_sensitive_patterns) {
                                    mask_sensitive_value(&v)
                                } else {
                                    v
                                };
                            (name, val)
                        } else {
                            (format!("col_{}", i), v)
                        };
                        map.insert(col_name, final_value);
                    }
                    serde_json::Value::Object(map)
                })
                .collect();

            let rows_count = json_rows.len();
            // Avoid a second COUNT statement against Trino. The result itself
            // is authoritative for this request, and the direct endpoint uses
            // the bounded preview size by design.
            let total_rows = i64::try_from(rows_count).unwrap_or(i64::MAX);

            sqlx::query(
                "UPDATE nl2sql_queries SET executed = 1, rows_returned = ?, execution_ms = ? WHERE id = ?",
            )
            .bind(i32::try_from(total_rows).unwrap_or(i32::MAX))
            .bind(i32::try_from(start.elapsed().as_millis()).unwrap_or(i32::MAX))
            .bind(query_id)
            .execute(&state.db)
            .await?;

            Ok(ExecOnceResult {
                columns,
                rows: json_rows,
                rows_count,
                total_rows,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: None,
            })
        }
        "postgres" => {
            #[derive(Debug, serde::Deserialize)]
            struct PgConfig {
                host: String,
                port: u16,
                database: String,
                username: String,
                password: String,
            }
            let config_val = decrypt_config(config_json, &state.data_dir)?;
            let cfg: PgConfig = serde_json::from_value(config_val)
                .map_err(|_| AppError::ValidationError("invalid postgres config".into()))?;

            let url = crate::routes::data_sources::build_postgres_url_parts(
                &cfg.username,
                &cfg.password,
                &cfg.host,
                cfg.port,
                &cfg.database,
            );
            // Pool cache: keyed by (tenant_id, data_source_id, updated_at).
            let pool = state
                .nl2sql_pool_cache
                .get_postgres(&claims.tenant_id, data_source_id, ds_version, &url)
                .await
                .map_err(|e| {
                    state
                        .nl2sql_pool_cache
                        .evict(&claims.tenant_id, data_source_id, ds_version);
                    AppError::Internal(format!("postgres connection failed: {e}"))
                })?;

            // We need `SET statement_timeout` and the SELECT to share the same
            // physical connection (it's session-scoped). With a multi-conn
            // cached pool the only safe way is to acquire one connection and
            // pin both operations to it via `&mut *conn`.
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| AppError::Internal(format!("postgres acquire failed: {e}")))?;
            let postgres_pid = sqlx::query("SELECT pg_backend_pid()")
                .fetch_one(&mut *conn)
                .await
                .ok()
                .and_then(|row| row.try_get::<i32, _>(0).ok());
            let _active_query_guard = ActiveQueryGuard::register(ActiveQueryHandle {
                tenant_id: claims.tenant_id.clone(),
                data_source_id: data_source_id.to_string(),
                db_type: db_type.to_string(),
                query_id: query_id.to_string(),
                mysql_thread_id: None,
                postgres_pid,
            });

            // Session-scoped statement timeout — applies to the COUNT and the
            // paginated SELECT below, then the conn is released back to the
            // pool. sqlx resets session state on release (`DISCARD ALL` is
            // NOT issued by default though, so for paranoia we explicitly
            // reset before releasing — see end of branch).
            let _ = sqlx::query(&format!(
                "SET statement_timeout = {}",
                (timeout_secs as u64).saturating_mul(1000)
            ))
            .execute(&mut *conn)
            .await;

            // Postgres COUNT(*) is signed BIGINT, so it does not need the MySQL CAST guard.
            let count_sql = format!("SELECT COUNT(*) FROM ({}) AS t", normalized_sql);
            let count_row = {
                let _count_permit = request_budget
                    .acquire("Postgres count query")
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs as u64),
                    sqlx::query(&count_sql).fetch_one(&mut *conn),
                )
                .await
                {
                    Ok(result) => result
                        .map_err(|e| AppError::Internal(format!("count query failed: {e}")))?,
                    Err(_) => {
                        conn.close_on_drop();
                        return Err(AppError::Internal("Count query timed out".to_string()));
                    }
                }
            };
            let total_rows: i64 = count_row.get(0);

            // Execute exactly the SQL shown in the editor. Do not append hidden
            // LIMIT/OFFSET clauses.
            let mut sql_rows = {
                let _query_permit = request_budget
                    .acquire("Postgres result query")
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs as u64),
                    sqlx::query(&normalized_sql).fetch_all(&mut *conn),
                )
                .await
                {
                    Ok(result) => result
                        .map_err(|e| AppError::Internal(format!("postgres query failed: {e}")))?,
                    Err(_) => {
                        conn.close_on_drop();
                        return Err(AppError::Internal("Query execution timed out".to_string()));
                    }
                }
            };

            enforce_row_cap_after_fetch(&mut sql_rows);

            // Reset session state so the next caller of this pooled conn
            // doesn't inherit our `statement_timeout`. `RESET` is cheap.
            let _ = sqlx::query("RESET statement_timeout")
                .execute(&mut *conn)
                .await;
            drop(conn);

            let columns: Vec<ColumnInfo> = if sql_rows.is_empty() {
                vec![]
            } else {
                sql_rows[0]
                    .columns()
                    .iter()
                    .map(|c| ColumnInfo {
                        name: c.name().to_string(),
                        type_: "text".to_string(),
                        masked: Some(is_datasource_sensitive(
                            c.name(),
                            datasource_sensitive_patterns,
                        )),
                    })
                    .collect()
            };

            let json_rows: Vec<serde_json::Value> = sql_rows
                .iter()
                .map(|row| {
                    let mut map = serde_json::Map::new();
                    for (i, col) in row.columns().iter().enumerate() {
                        let raw_value = decode_postgres_cell(row, i);
                        let after_sensitive =
                            if is_datasource_sensitive(col.name(), datasource_sensitive_patterns) {
                                mask_sensitive_value(&raw_value)
                            } else {
                                raw_value
                            };
                        let final_value =
                            crate::routes::nl2sql::masking_rules::apply_rules_to_value(
                                &masking_rules,
                                &claims.tenant_id,
                                &primary_table_hint,
                                col.name(),
                                &after_sensitive,
                            );
                        map.insert(col.name().to_string(), final_value);
                    }
                    serde_json::Value::Object(map)
                })
                .collect();

            let rows_count = json_rows.len();

            sqlx::query(
                "UPDATE nl2sql_queries SET executed = 1, rows_returned = ?, execution_ms = ? WHERE id = ?",
            )
            .bind(i32::try_from(total_rows).unwrap_or(i32::MAX))
            .bind(i32::try_from(start.elapsed().as_millis()).unwrap_or(i32::MAX))
            .bind(query_id)
            .execute(&state.db)
            .await?;

            Ok(ExecOnceResult {
                columns,
                rows: json_rows,
                rows_count,
                total_rows,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: None,
            })
        }
        "mongodb" => {
            let config_val = decrypt_config(config_json, &state.data_dir)?;
            let cfg: nl2sql_domain::datasource_config::MongoConfig =
                serde_json::from_value(config_val).map_err(|error| {
                    AppError::ValidationError(format!("invalid MongoDB config: {error}"))
                })?;
            let result = super::mongodb_query::execute(
                &cfg,
                &normalized_sql,
                1_000,
                std::time::Duration::from_secs(u64::from(timeout_secs)),
            )
            .await
            .map_err(AppError::ValidationError)?;

            let columns = result
                .columns
                .iter()
                .map(|name| ColumnInfo {
                    name: name.clone(),
                    type_: "document".to_string(),
                    masked: Some(
                        is_datasource_sensitive(name, datasource_sensitive_patterns)
                            || datasource_sensitive_patterns.iter().any(|pattern| {
                                pattern
                                    .to_ascii_lowercase()
                                    .starts_with(&format!("{}.", name.to_ascii_lowercase()))
                            }),
                    ),
                })
                .collect::<Vec<_>>();
            let mut protected_rows = result.rows;
            crate::routes::nl2sql::apply_datasource_masking(
                &mut protected_rows,
                datasource_sensitive_patterns,
            );
            let json_rows = protected_rows
                .into_iter()
                .map(|row| {
                    let Some(values) = row.as_object() else {
                        return row;
                    };
                    let mut masked = serde_json::Map::with_capacity(values.len());
                    for (name, value) in values {
                        let final_value =
                            crate::routes::nl2sql::masking_rules::apply_rules_to_value(
                                &masking_rules,
                                &claims.tenant_id,
                                &primary_table_hint,
                                name,
                                value,
                            );
                        masked.insert(name.clone(), final_value);
                    }
                    serde_json::Value::Object(masked)
                })
                .collect::<Vec<_>>();
            let rows_count = json_rows.len();

            sqlx::query(
                "UPDATE nl2sql_queries SET executed = 1, rows_returned = ?, execution_ms = ? WHERE id = ?",
            )
            .bind(i32::try_from(result.total_rows).unwrap_or(i32::MAX))
            .bind(i32::try_from(start.elapsed().as_millis()).unwrap_or(i32::MAX))
            .bind(query_id)
            .execute(&state.db)
            .await?;

            Ok(ExecOnceResult {
                columns,
                rows: json_rows,
                rows_count,
                total_rows: result.total_rows,
                execution_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                error: None,
            })
        }
        "http_api" => Err(AppError::ValidationError(
            "HTTP API execution was removed; convert to a SQL data source".to_string(),
        )),
        _ => Err(AppError::ValidationError(format!(
            "execution not supported for db_type: {db_type}",
        ))),
    }
}

/// GET /api/v1/nl2sql/history — query history.
pub(crate) async fn history(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(_params): Query<PaginationParams>,
) -> Result<Json<QueryHistoryResponse>> {
    const FIXED_HISTORY_LIMIT: i64 = 100;

    let rows = sqlx::query(
        "SELECT id, data_source_id, question, generated_sql, executed, CAST(rows_returned AS INTEGER) AS rows_returned, \
         COALESCE(planning_ms, 0) as planning_ms, \
         COALESCE(execution_ms, 0) as execution_ms, \
         error_message, created_at \
         FROM nl2sql_queries \
         WHERE tenant_id = ? AND user_id = ? AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(FIXED_HISTORY_LIMIT)
    .fetch_all(&state.db)
    .await?;

    let queries: Vec<QueryHistoryItem> = rows
        .into_iter()
        .map(|row| QueryHistoryItem {
            id: row.get("id"),
            data_source_id: row.get("data_source_id"),
            question: row.get("question"),
            generated_sql: row.get("generated_sql"),
            executed: row.get::<i8, _>("executed") == 1,
            rows_returned: row.get::<Option<i64>, _>("rows_returned").unwrap_or(0),
            planning_ms: row.get::<Option<i64>, _>("planning_ms").unwrap_or(0),
            execution_ms: row.get::<Option<i64>, _>("execution_ms").unwrap_or(0),
            error_message: row.get("error_message"),
            created_at: row
                .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                .to_rfc3339(),
        })
        .collect();

    Ok(Json(QueryHistoryResponse {
        total: queries.len(),
        queries,
    }))
}

/// Extracts table names referenced in a SELECT SQL string.
/// Matches `FROM table_name` and `JOIN table_name` patterns (case-insensitive).
pub(crate) fn extract_tables_from_sql(sql: &str) -> Vec<String> {
    let mut tables = Vec::new();
    static TABLE_RE: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();

    let Some(table_re) = cached_regex(
        &TABLE_RE,
        r#"(?i)\b(?:FROM|JOIN)\s+((?:"[^"]+"|`[^`]+`|[a-z_][a-z0-9_$]*)(?:\s*\.\s*(?:"[^"]+"|`[^`]+`|[a-z_][a-z0-9_$]*))*)"#,
    ) else {
        return tables;
    };

    for cap in table_re.captures_iter(sql) {
        if let Some(name) = cap.get(1) {
            let table = super::normalize_table_identifier(name.as_str());
            if table.is_empty() {
                continue;
            }
            if !tables.contains(&table) {
                tables.push(table);
            }
        }
    }

    tables
}

/// Extracts column names referenced in a SELECT SQL string.
/// Matches bare column names in SELECT list and `table.column` references.
pub(crate) fn extract_columns_from_sql(sql: &str) -> Vec<String> {
    let mut columns = Vec::new();
    static QUALIFIED_RE: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
    static SELECT_RE: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();
    static BARE_RE: OnceLock<std::result::Result<Regex, regex::Error>> = OnceLock::new();

    // Match table.column references first
    if let Some(qualified_re) = cached_regex(
        &QUALIFIED_RE,
        r"(?i)\b([a-z_][a-z0-9_]*)\.([a-z_][a-z0-9_]*)\b",
    ) {
        for cap in qualified_re.captures_iter(sql) {
            if let Some(col) = cap.get(2) {
                let col_name = col.as_str().to_lowercase();
                if !columns.contains(&col_name) {
                    columns.push(col_name);
                }
            }
        }
    }

    // Match bare column names in SELECT list (between SELECT and FROM)
    if let Some(cap) = cached_regex(&SELECT_RE, r"(?i)\bSELECT\s+(.*?)\s+FROM\b")
        .and_then(|select_re| select_re.captures(sql))
    {
        if let Some(body) = cap.get(1) {
            let select_body = body.as_str();
            // Split by commas, strip aliases, collect bare identifiers
            if let Some(bare_re) = cached_regex(
                &BARE_RE,
                r"(?i)\b([a-z_][a-z0-9_]*)\s*(?:AS\s+\w+)?(?:\s*,\s*|$)",
            ) {
                for cap in bare_re.captures_iter(select_body) {
                    if let Some(name) = cap.get(1) {
                        let n = name.as_str();
                        // Skip SQL keywords that might appear as column-like identifiers
                        if !n.eq_ignore_ascii_case("DISTINCT")
                            && !n.eq_ignore_ascii_case("COUNT")
                            && !n.eq_ignore_ascii_case("SUM")
                            && !n.eq_ignore_ascii_case("AVG")
                            && !n.eq_ignore_ascii_case("MIN")
                            && !n.eq_ignore_ascii_case("MAX")
                        {
                            let col_name = n.to_lowercase();
                            if !columns.contains(&col_name) {
                                columns.push(col_name);
                            }
                        }
                    }
                }
            }
        }
    }

    columns
}

#[derive(Debug, FromRow)]
struct PolicyRow {
    allowed_tables: serde_json::Value,
    denied_tables: serde_json::Value,
    allowed_columns: serde_json::Value,
    denied_columns: serde_json::Value,
    row_filter_expr: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PolicyEnforcementDecision {
    pub had_policy: bool,
    pub row_filter_expr: Option<String>,
    pub denied_tables: Vec<String>,
    pub denied_columns: Vec<String>,
}

impl PolicyEnforcementDecision {
    pub(crate) fn is_denied(&self) -> bool {
        !self.denied_tables.is_empty() || !self.denied_columns.is_empty()
    }
}

pub(crate) async fn query_policy_lineage_hash(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    user_id: &str,
    user_email: &str,
) -> Result<String> {
    let row: Option<PolicyRow> = sqlx::query_as(
        "SELECT allowed_tables, denied_tables, allowed_columns, denied_columns, row_filter_expr
         FROM nl2sql_query_policies
         WHERE tenant_id = ? AND datasource_id = ?
           AND (user_id = ? OR LOWER(user_id) = LOWER(?))
           AND enabled = 1 AND deleted_at IS NULL
         ORDER BY CASE
             WHEN user_id = ? THEN 0
             WHEN LOWER(user_id) = LOWER(?) THEN 1
             ELSE 2
           END
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(user_id)
    .bind(user_email)
    .bind(user_id)
    .bind(user_email)
    .fetch_optional(db)
    .await?;
    let value = row.map_or(serde_json::Value::Null, |policy| {
        serde_json::json!({
            "allowedTables": policy.allowed_tables,
            "deniedTables": policy.denied_tables,
            "allowedColumns": policy.allowed_columns,
            "deniedColumns": policy.denied_columns,
            "rowFilter": policy.row_filter_expr,
        })
    });
    Ok(crate::semantic_kernel_store::sha256_json(&value))
}

pub(crate) fn query_policy_denial_message(decision: &PolicyEnforcementDecision) -> String {
    let mut details = Vec::new();
    if !decision.denied_tables.is_empty() {
        details.push(format!("tables: {}", decision.denied_tables.join(", ")));
    }
    if !decision.denied_columns.is_empty() {
        details.push(format!("columns: {}", decision.denied_columns.join(", ")));
    }
    format!(
        "[query_policy_denied] SQL was generated successfully, but execution is blocked by the current query policy ({})",
        details.join("; ")
    )
}

/// Enforces the query policy for a given tenant/datasource/user.
///
/// Queries `nl2sql_query_policies` for the most specific matching policy,
/// then validates the generated SQL's referenced tables and columns against
/// the policy's allowlists and denylists.  If a `row_filter_expr` is set,
/// callers should inject it as a `WHERE` clause condition.
pub(crate) async fn enforce_query_policy(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    user_id: &str,
    user_email: &str,
    target_tables: &[String],
    target_columns: &[String],
) -> Result<PolicyEnforcementDecision> {
    let row: Option<PolicyRow> = sqlx::query_as(
        "SELECT allowed_tables, denied_tables, allowed_columns, denied_columns, row_filter_expr \
         FROM nl2sql_query_policies \
         WHERE tenant_id = ? AND datasource_id = ? \
           AND (user_id = ? OR LOWER(user_id) = LOWER(?)) \
           AND enabled = 1 AND deleted_at IS NULL \
         ORDER BY CASE \
             WHEN user_id = ? THEN 0 \
             WHEN LOWER(user_id) = LOWER(?) THEN 1 \
             ELSE 2 \
           END \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(user_id)
    .bind(user_email)
    .bind(user_id)
    .bind(user_email)
    .fetch_optional(db)
    .await?;

    let Some(policy) = row else {
        return Ok(PolicyEnforcementDecision {
            had_policy: false,
            row_filter_expr: None,
            denied_tables: Vec::new(),
            denied_columns: Vec::new(),
        });
    };

    let denied_tables: Vec<String> = policy
        .denied_tables
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let allowed_tables: Vec<String> = policy
        .allowed_tables
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let denied_columns: Vec<String> = policy
        .denied_columns
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let allowed_columns: Vec<String> = policy
        .allowed_columns
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    let denied_table_aliases: HashSet<String> = denied_tables
        .iter()
        .flat_map(|table| super::table_name_aliases(table))
        .collect();
    let mut blocked_tables = Vec::new();
    let mut blocked_columns = Vec::new();
    for table in target_tables {
        if super::table_ref_matches_set(table, &denied_table_aliases)
            && !blocked_tables.contains(table)
        {
            blocked_tables.push(table.clone());
        }
    }

    if !allowed_tables.is_empty() {
        let allowed_table_aliases: HashSet<String> = allowed_tables
            .iter()
            .flat_map(|table| super::table_name_aliases(table))
            .collect();
        for table in target_tables {
            if !super::table_ref_matches_set(table, &allowed_table_aliases)
                && !blocked_tables.contains(table)
            {
                blocked_tables.push(table.clone());
            }
        }
    }

    for col in target_columns {
        let normalized = col.trim().to_lowercase();
        if denied_columns.contains(&normalized) && !blocked_columns.contains(&normalized) {
            blocked_columns.push(normalized);
        }
    }

    if !allowed_columns.is_empty() {
        if target_columns.is_empty() {
            blocked_columns.push("*".to_string());
        } else {
            for col in target_columns {
                let normalized = col.trim().to_lowercase();
                if !allowed_columns.contains(&normalized) && !blocked_columns.contains(&normalized)
                {
                    blocked_columns.push(normalized);
                }
            }
        }
    }

    Ok(PolicyEnforcementDecision {
        had_policy: true,
        row_filter_expr: policy.row_filter_expr,
        denied_tables: blocked_tables,
        denied_columns: blocked_columns,
    })
}

// POST /api/v1/nl2sql/explain-sql — Generate natural language explanation of SQL.
#[derive(Debug, Deserialize)]
pub struct ExplainSqlRequest {
    pub sql: String,
    pub datasource_id: String,
    /// Target language for the explanation. Defaults to "en-US" if absent or unknown.
    /// Supported values: "zh-CN" (Chinese), "en-US" (English).
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "en-US".to_string()
}

#[derive(Debug, Serialize)]
pub struct ColumnNote {
    pub column: String,
    pub observation: String,
}

#[derive(Debug, Serialize)]
pub struct ExplainSqlResponse {
    pub explanation: String,
    pub summary: String,
    /// Key insights extracted from the SQL and schema. May be empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub insights: Vec<String>,
    /// Column-level observations. May be empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub column_notes: Vec<ColumnNote>,
}

fn build_explain_system_prompt(language: &str) -> String {
    if language == "zh-CN" {
        "你是一位 SQL 专家。给定一个数据库 schema 和一条 SQL 查询，解释这条查询的作用。\
         请用简洁的中文进行解释。\
         返回的 JSON 必须严格遵循 output_format 中定义的格式，不要包含任何 JSON 之外的内容。"
            .to_string()
    } else {
        "You are a SQL expert. Given a database schema and a SQL query, explain what the query \
         does in plain English. Return ONLY valid JSON with the structure shown in output_format. \
         No markdown, no code fences, no explanation outside the JSON."
            .to_string()
    }
}

fn build_explain_output_format(language: &str) -> serde_json::Value {
    if language == "zh-CN" {
        serde_json::json!({
            "explanation": "用 2-3 句话解释这条 SQL 查询做了什么",
            "summary": "一句话概括这条查询的用途",
            "insights": ["洞察 1", "洞察 2"],
            "column_notes": [
                { "column": "字段名", "observation": "对该字段的简要说明" }
            ]
        })
    } else {
        serde_json::json!({
            "explanation": "2-3 sentence plain-English explanation of what this SQL query does",
            "summary": "one-sentence summary of the query purpose",
            "insights": ["insight 1", "insight 2"],
            "column_notes": [
                { "column": "col_name", "observation": "brief note about this column" }
            ]
        })
    }
}

/// POST /api/v1/nl2sql/explain-sql — Generate natural language explanation of SQL.
pub(crate) async fn explain_sql(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ExplainSqlRequest>,
) -> Result<Json<ExplainSqlResponse>> {
    // Validate that the datasource belongs to the tenant.
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&req.datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }

    // Fetch schema for the datasource.
    let schema_info: serde_json::Value = {
        let row = sqlx::query("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(&req.datasource_id)
            .fetch_optional(&state.db)
            .await?;
        match row {
            Some(r) => r
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or(serde_json::json!({"tables": [], "foreign_keys": []})),
            None => return Err(AppError::NotFound("data source not found".into())),
        }
    };

    let schema_summary = serde_json::to_string_pretty(&schema_info)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let language = if req.language == "zh-CN" {
        "zh-CN"
    } else {
        "en-US"
    };

    let output_format = build_explain_output_format(language);

    #[derive(serde::Serialize)]
    struct ExplainSqlPrompt<'a> {
        schema_json: &'a str,
        sql: &'a str,
        output_format: serde_json::Value,
    }
    let prompt_json = serde_json::to_string(&ExplainSqlPrompt {
        schema_json: &schema_summary,
        sql: &req.sql,
        output_format,
    })
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to resolve LLM config: {}", e)))?;

    let request = MessageRequest {
        model: chat_cfg.model.clone(),
        max_tokens: 1024,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt_json }],
        }],
        system: Some(build_explain_system_prompt(language)),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.3),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = chat_cfg
        .client
        .send_message(&request)
        .await
        .map_err(|e| AppError::Internal(format!("LLM explanation call failed: {}", e)))?;

    let text = response
        .content
        .iter()
        .find_map(|b| match b {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let trimmed = text.trim().trim_start_matches('`').trim();

    #[derive(serde::Deserialize)]
    struct LlmSqlExplanation {
        explanation: String,
        summary: String,
        #[serde(default)]
        insights: Vec<String>,
        #[serde(default)]
        column_notes: Vec<serde_json::Value>,
    }

    let parsed: LlmSqlExplanation = serde_json::from_str(trimmed).unwrap_or(LlmSqlExplanation {
        explanation: if language == "zh-CN" {
            "解析失败，请手动检查 SQL。".to_string()
        } else {
            "Failed to parse explanation. Please review the SQL manually.".to_string()
        },
        summary: if language == "zh-CN" {
            "解析错误".to_string()
        } else {
            "Parse error".to_string()
        },
        insights: Vec::new(),
        column_notes: Vec::new(),
    });

    let column_notes: Vec<ColumnNote> = parsed
        .column_notes
        .into_iter()
        .filter_map(|v| {
            Some(ColumnNote {
                column: v.get("column")?.as_str()?.to_owned(),
                observation: v.get("observation")?.as_str()?.to_owned(),
            })
        })
        .collect();

    Ok(Json(ExplainSqlResponse {
        explanation: parsed.explanation,
        summary: parsed.summary,
        insights: parsed.insights,
        column_notes,
    }))
}

/// Hard ceiling on rows returned to the client, regardless of what LIMIT the
/// LLM or user requested. Acts as the last line of defence against OOM caused
/// by a runaway query. Tunable via `NL2SQL_MAX_RESULT_ROWS` (default: 10000).
pub(crate) fn nl2sql_max_result_rows() -> i64 {
    std::env::var("NL2SQL_MAX_RESULT_ROWS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(10_000)
}

/// Trim trailing whitespace and semicolons so `sql` can be safely embedded
/// into wrapper subqueries (`SELECT COUNT(*) FROM (<sql>) AS t`) across
/// MySQL/Postgres/Trino/ClickHouse.
fn normalize_sql_for_execution(sql: &str) -> String {
    sql.trim_end_matches(|c: char| c.is_ascii_whitespace() || c == ';')
        .to_string()
}

/// Enforces the hard [`nl2sql_max_result_rows`] cap after the database has
/// returned rows. Used as a belt-and-suspenders defence for cases where the
/// SQL carried its own `LIMIT` larger than our cap.
pub(crate) fn enforce_row_cap_after_fetch<T>(rows: &mut Vec<T>) {
    let cap = nl2sql_max_result_rows() as usize;
    if rows.len() > cap {
        rows.truncate(cap);
    }
}

/// Returns the MySQL/TiDB optimizer hint that caps server-side execution at
/// `timeout_ms` milliseconds. Returns an empty string for non-MySQL dialects.
///
/// The hint goes directly after the `SELECT` keyword:
///   `SELECT /*+ MAX_EXECUTION_TIME(5000) */ ...`
///
/// We use a *positional* injection (find the first `SELECT` keyword that is
/// not inside a comment or string) rather than a regex on `\bSELECT\b` because
/// a regex would erroneously match inside string literals.
pub(crate) fn inject_mysql_max_execution_time(sql: &str, timeout_ms: u64) -> String {
    let hint = format!("/*+ MAX_EXECUTION_TIME({}) */", timeout_ms);
    let upper = sql.to_uppercase();
    // Skip leading comments / whitespace.
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // ASCII whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // `--` line comment
        if i + 1 < bytes.len() && &bytes[i..i + 2] == b"--" {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // `/* ... */` block comment
        if i + 1 < bytes.len() && &bytes[i..i + 2] == b"/*" {
            i += 2;
            while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    // After skipping whitespace/comments, we expect either SELECT or WITH.
    // For WITH we inject the hint into the *outer* SELECT, which we approximate
    // by leaving the SQL alone (the LLM nearly always emits a SELECT, not a
    // WITH; the outer Tokio timeout still bounds runtime).
    if i + 6 <= upper.len() && &upper[i..i + 6] == "SELECT" {
        let mut out = String::with_capacity(sql.len() + hint.len() + 2);
        out.push_str(&sql[..i + 6]);
        out.push(' ');
        out.push_str(&hint);
        out.push_str(&sql[i + 6..]);
        return out;
    }
    sql.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tables_from_sql_keeps_trino_qualified_names() {
        let sql = r#"
SELECT bo.order_id, oi.item_id
FROM iceberg.mps_prod.business_order bo
JOIN `hive`.`ods`.`order_item` oi ON oi.order_id = bo.order_id
"#;

        let tables = extract_tables_from_sql(sql);

        assert_eq!(
            tables,
            vec![
                "iceberg.mps_prod.business_order".to_string(),
                "hive.ods.order_item".to_string()
            ]
        );
    }

    #[test]
    fn table_policy_aliases_match_full_schema_table_refs() {
        let allowed: HashSet<String> = ["business_order"]
            .into_iter()
            .flat_map(crate::routes::nl2sql::table_name_aliases)
            .collect();
        let denied: HashSet<String> = ["ods.order_item"]
            .into_iter()
            .flat_map(crate::routes::nl2sql::table_name_aliases)
            .collect();

        assert!(crate::routes::nl2sql::table_ref_matches_set(
            "iceberg.mps_prod.business_order",
            &allowed
        ));
        assert!(crate::routes::nl2sql::table_ref_matches_set(
            "hive.ods.order_item",
            &denied
        ));
        assert!(!crate::routes::nl2sql::table_ref_matches_set(
            "iceberg.mps_prod.business_order",
            &denied
        ));
    }

    #[test]
    fn recovery_scope_change_is_always_diagnostic() {
        let candidate = parse_model_recovery_decision(
            r#"{
                "action": "retry",
                "sql": "SELECT app_id, roi FROM metrics WHERE dt = DATE '2026-08-13'",
                "strategy": "validate with a recent available partition",
                "scopeChanged": true,
                "diagnosticOnly": false,
                "rationale": "the requested partition is missing"
            }"#,
        )
        .expect("valid recovery protocol");
        let decision = candidate.decision.expect("decision");
        assert!(decision.scope_changed);
        assert!(decision.diagnostic_only);
        assert!(candidate.sql.contains("2026-08-13"));
    }

    #[test]
    fn recovery_can_preserve_scope_as_formal_query() {
        let candidate = parse_model_recovery_decision(
            r#"{
                "action": "retry",
                "sql": "SELECT app_id, roi FROM metrics WHERE dt BETWEEN DATE '2026-08-01' AND DATE '2026-08-07' AND dt <> DATE '2026-08-03'",
                "strategy": "exclude an unavailable partition while preserving the requested interval",
                "scopeChanged": false,
                "diagnosticOnly": false,
                "rationale": "the metric definition permits excluding the corrupt duplicate partition"
            }"#,
        )
        .expect("valid recovery protocol");
        let decision = candidate.decision.expect("decision");
        assert!(!decision.scope_changed);
        assert!(!decision.diagnostic_only);
    }

    #[test]
    fn recovery_stop_does_not_submit_another_sql() {
        let candidate = parse_model_recovery_decision(
            r#"{
                "action": "stop",
                "sql": null,
                "strategy": "no honest bounded retry",
                "scopeChanged": false,
                "diagnosticOnly": true,
                "rationale": "available partitions cannot be inferred"
            }"#,
        )
        .expect("valid stop protocol");
        assert!(candidate.sql.is_empty());
        assert!(candidate.decision.expect("decision").diagnostic_only);
    }

    #[tokio::test]
    async fn query_policy_denial_reports_every_blocked_table_and_column() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::query(
            "CREATE TABLE nl2sql_query_policies (
                tenant_id TEXT NOT NULL,
                datasource_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                allowed_tables JSON NOT NULL,
                denied_tables JSON NOT NULL,
                allowed_columns JSON NOT NULL,
                denied_columns JSON NOT NULL,
                row_filter_expr TEXT,
                enabled INTEGER NOT NULL,
                deleted_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create policies");
        sqlx::query(
            "INSERT INTO nl2sql_query_policies
             (tenant_id, datasource_id, user_id, allowed_tables, denied_tables,
              allowed_columns, denied_columns, row_filter_expr, enabled)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, 1)",
        )
        .bind("tenant-a")
        .bind("ds-a")
        .bind("user-a")
        .bind(serde_json::json!(["orders"]))
        .bind(serde_json::json!(["secrets"]))
        .bind(serde_json::json!(["order_id"]))
        .bind(serde_json::json!(["password_hash"]))
        .execute(&pool)
        .await
        .expect("insert policy");

        let decision = enforce_query_policy(
            &pool,
            "tenant-a",
            "ds-a",
            "user-a",
            "user@example.com",
            &["orders".to_string(), "secrets".to_string()],
            &["order_id".to_string(), "password_hash".to_string()],
        )
        .await
        .expect("evaluate policy");

        assert_eq!(decision.denied_tables, vec!["secrets"]);
        assert_eq!(decision.denied_columns, vec!["password_hash"]);
        let message = query_policy_denial_message(&decision);
        assert!(message.starts_with("[query_policy_denied]"));
        assert!(message.contains("tables: secrets"));
        assert!(message.contains("columns: password_hash"));
    }

    #[tokio::test]
    async fn execution_repair_is_reverified_against_the_durable_canonical_intent() {
        let pool = crate::test_sqlite_pool().await;
        let mut intent = super::super::semantic_audit::compile_question_intent(
            "tenant-a",
            "datasource-a",
            "按设备统计订单数",
            &[],
        );
        super::super::semantic_audit::bind_schema_dimensions(
            &mut intent,
            &serde_json::json!([{
                "table_name": "task_offer",
                "columns": [
                    {"name": "executor_device_id"},
                    {"name": "order_id"}
                ]
            }]),
            &[],
        );
        crate::semantic_kernel_store::persist_nl2sql_intent_ir(
            &pool,
            "tenant-a",
            "thread-a",
            "turn-a",
            "query-a",
            &serde_json::to_value(&intent).expect("serialize intent"),
        )
        .await
        .expect("persist canonical intent");

        let preserving = "SELECT executor_device_id, COUNT(*) AS order_count \
                          FROM task_offer GROUP BY executor_device_id";
        let initial_audit =
            super::super::semantic_audit::compile_canonical_intent_with_contracts_and_joins(
                &intent,
                preserving,
                &[],
                &[],
            )
            .expect("compile initial semantic release");
        let initial_decision = serde_json::to_string(&initial_audit.verification.release_decision)
            .unwrap()
            .trim_matches('"')
            .to_string();
        assert_eq!(initial_decision, "Release");
        crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
            &pool,
            "tenant-a",
            "datasource-a",
            "thread-a",
            "query-a",
            &super::super::semantic_audit::intent_json(&initial_audit),
            &super::super::semantic_audit::verification_json(&initial_audit),
            &initial_decision,
            f64::from(initial_audit.verification.confidence_basis.calibrated_score),
        )
        .await
        .expect("persist initial semantic release");
        let missing_ir = verify_candidate_against_canonical_intent(
            &pool,
            "tenant-a",
            "query-missing",
            Some("datasource-a"),
            preserving,
        )
        .await
        .expect_err("execution without canonical IR must fail closed");
        assert!(missing_ir
            .to_string()
            .contains("canonical analytic intent is missing"));
        let datasource_drift = verify_candidate_against_canonical_intent(
            &pool,
            "tenant-a",
            "query-a",
            Some("datasource-b"),
            preserving,
        )
        .await
        .expect_err("execution cannot move an intent across datasources");
        assert!(datasource_drift.to_string().contains("another datasource"));
        verify_repair_against_canonical_intent(&pool, "tenant-a", "query-a", preserving)
            .await
            .expect("scope-preserving repair must pass");
        let overwrite = crate::semantic_kernel_store::persist_nl2sql_repair_verification(
            &pool,
            "tenant-a",
            "query-a",
            preserving,
            &serde_json::json!({"releaseDecision": "Reject"}),
            "Reject",
            0.1,
        )
        .await
        .expect_err("the same repaired SQL cannot acquire a different audit outcome");
        assert!(overwrite.to_string().contains("immutable"));

        let drifting = "SELECT COUNT(*) AS order_count FROM task_offer";
        let error = verify_repair_against_canonical_intent(&pool, "tenant-a", "query-a", drifting)
            .await
            .expect_err("repair that drops the requested grain must be blocked");
        assert!(error.to_string().contains("did not release"));

        let decisions = sqlx::query_scalar::<_, String>(
            "SELECT release_decision FROM nl2sql_repair_verifications
             WHERE tenant_id = 'tenant-a' AND analytic_intent_id = 'query-a'
             ORDER BY created_at, id",
        )
        .fetch_all(&pool)
        .await
        .expect("load repair audit rows");
        assert_eq!(decisions.len(), 2);
        assert!(decisions.iter().any(|decision| decision == "Release"));
        assert!(decisions.iter().any(|decision| decision != "Release"));
    }

    #[tokio::test]
    async fn result_invariants_promote_only_observed_passing_results_to_release() {
        let pool = crate::test_sqlite_pool().await;
        let contract = nl2sql_core::semantic_ir::MetricContract {
            id: "orders".into(),
            version: 1,
            names: vec!["orders".into()],
            expression: nl2sql_core::semantic_ir::MetricExpressionIR::Aggregate {
                function: "COUNT".into(),
                expression: Box::new(nl2sql_core::semantic_ir::MetricExpressionIR::Literal(
                    "*".into(),
                )),
                distinct: false,
            },
            denominator: None,
            population: nl2sql_core::semantic_ir::PopulationDefinition {
                subject: "order".into(),
                dedup_key: None,
                exclude_test_users: false,
                exclude_internal_users: false,
                valid_record_rule: None,
            },
            default_grain: nl2sql_core::semantic_ir::Grain::Row,
            allowed_grains: vec![nl2sql_core::semantic_ir::Grain::Row],
            time_column: "created_at".into(),
            timezone: "UTC".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![nl2sql_core::semantic_ir::ResultInvariant::NonNegative {
                field: "orders".into(),
            }],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: None,
            evidence_refs: vec![],
        };
        sqlx::query(
            "INSERT INTO metric_contracts
                (id, tenant_id, datasource_id, source_metric_id, version, status,
                 contract_json, lineage_json, valid_from, valid_until)
             VALUES ('orders', 'tenant', 'ds', NULL, 1, 'active', ?, '{}',
                     '2026-01-01', NULL)",
        )
        .bind(serde_json::to_string(&contract).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let mut intent = super::super::semantic_audit::compile_question_intent(
            "tenant",
            "ds",
            "订单数",
            &["orders".into()],
        );
        intent.metrics[0].version = Some(1);
        let candidate = CandidateSemanticVerification {
            intent,
            verification: serde_json::json!({
                "result_invariants": [{
                    "status": "NotChecked",
                    "code": "result_invariant_pending",
                    "message": "pending"
                }],
                "executable": null,
                "confidence_basis": {
                    "calibrated_score": 0.8,
                    "execution_passed": false
                },
                "release_decision": "ValidateResult"
            }),
            release_decision: "ValidateResult".into(),
            calibrated_score: 0.8,
        };
        for query_id in ["query-pass", "query-fail", "query-missing"] {
            sqlx::query(
                "INSERT INTO semantic_verifications
                    (id, tenant_id, analytic_intent_id, verification_json,
                     release_decision, calibrated_score, created_at)
                 VALUES (?, 'tenant', ?, ?, 'ValidateResult', 0.8, CURRENT_TIMESTAMP)",
            )
            .bind(format!("verification-{query_id}"))
            .bind(query_id)
            .bind(candidate.verification.to_string())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO feedback_learning_events
                (id, tenant_id, scope, correction_json, approved, regression_case_id,
                 approved_by, approved_at, created_at)
             VALUES ('feedback-pass', 'tenant', 'datasource:ds', '{}', 1,
                     'case-pass', 'owner', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let sql = "SELECT COUNT(*) AS orders FROM orders";
        let sql_hash = hex::encode(sha2::Sha256::digest(sql.as_bytes()));
        sqlx::query(
            "INSERT INTO feedback_regression_cases
                (id, tenant_id, datasource_id, feedback_event_id, analytic_intent_id,
                 original_ir_hash, original_sql_hash, corrected_sql_hash,
                 semantic_diff_json, verification_json, fixture_json, status)
             VALUES ('case-pass', 'tenant', 'ds', 'feedback-pass', 'query-pass',
                     'ir', 'old', ?, '{}', '{}', '{}', 'approved')",
        )
        .bind(&sql_hash)
        .execute(&pool)
        .await
        .unwrap();

        let passed = finalize_result_invariants(
            &pool,
            "tenant",
            "ds",
            "query-pass",
            sql,
            &[serde_json::json!({"orders": 4})],
            1,
            12,
            &candidate,
        )
        .await
        .unwrap();
        assert_eq!(passed.release_decision, "Release");
        let released: String = sqlx::query_scalar(
            "SELECT release_decision FROM semantic_verifications
             WHERE tenant_id = 'tenant' AND analytic_intent_id = 'query-pass'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(released, "Release");
        let case_status: String = sqlx::query_scalar(
            "SELECT status FROM feedback_regression_cases WHERE id = 'case-pass'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(case_status, "verified");

        let failed = finalize_result_invariants(
            &pool,
            "tenant",
            "ds",
            "query-fail",
            sql,
            &[serde_json::json!({"orders": -1})],
            1,
            8,
            &candidate,
        )
        .await
        .unwrap();
        assert_eq!(failed.release_decision, "Reject");
        let missing = finalize_result_invariants(
            &pool,
            "tenant",
            "ds",
            "query-missing",
            sql,
            &[serde_json::json!({"other": 1})],
            1,
            7,
            &candidate,
        )
        .await
        .unwrap();
        assert_eq!(missing.release_decision, "NeedsClarification");
        let statuses = sqlx::query_scalar::<_, String>(
            "SELECT status FROM result_invariant_observations
             WHERE tenant_id = 'tenant' ORDER BY status",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(statuses, vec!["fail", "not_observed", "pass"]);
    }

    #[tokio::test]
    async fn execution_binding_rejects_missing_cross_scope_or_edited_queries() {
        let pool = crate::test_sqlite_pool().await;
        sqlx::query(
            "INSERT INTO tenants (id, name, slug) VALUES ('tenant-a', 'Tenant A', 'tenant-a')",
        )
        .execute(&pool)
        .await
        .expect("insert tenant fixture");
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, name, tenant_id)
             VALUES ('user-a', 'user-a@example.com', 'not-used', 'User A', 'tenant-a')",
        )
        .execute(&pool)
        .await
        .expect("insert user fixture");
        sqlx::query(
            "INSERT INTO data_sources (id, tenant_id, name, db_type, config)
             VALUES ('datasource-a', 'tenant-a', 'Datasource A', 'sqlite', '{}')",
        )
        .execute(&pool)
        .await
        .expect("insert datasource fixture");
        sqlx::query(
            "INSERT INTO nl2sql_queries
                (id, tenant_id, user_id, data_source_id, question, generated_sql, executed)
             VALUES ('query-a', 'tenant-a', 'user-a', 'datasource-a', 'orders',
                     'SELECT COUNT(*) FROM orders', 0)",
        )
        .execute(&pool)
        .await
        .expect("insert released query fixture");

        let missing = load_released_query_binding(
            &pool,
            "tenant-a",
            "user-a",
            "missing",
            "datasource-a",
            "SELECT COUNT(*) FROM orders",
        )
        .await
        .expect_err("unknown query id must fail closed");
        assert!(missing
            .to_string()
            .contains("no durable released candidate"));

        let other_user = load_released_query_binding(
            &pool,
            "tenant-a",
            "user-b",
            "query-a",
            "datasource-a",
            "SELECT COUNT(*) FROM orders",
        )
        .await
        .expect_err("another user cannot execute the candidate");
        assert!(other_user
            .to_string()
            .contains("no durable released candidate"));

        let other_datasource = load_released_query_binding(
            &pool,
            "tenant-a",
            "user-a",
            "query-a",
            "datasource-b",
            "SELECT COUNT(*) FROM orders",
        )
        .await
        .expect_err("datasource drift must fail closed");
        assert!(other_datasource.to_string().contains("does not match"));

        let edited = load_released_query_binding(
            &pool,
            "tenant-a",
            "user-a",
            "query-a",
            "datasource-a",
            "SELECT COUNT(DISTINCT id) FROM orders",
        )
        .await
        .expect_err("in-place SQL edits must not inherit the old release");
        assert!(edited.to_string().contains("SQL hash"));

        let valid = load_released_query_binding(
            &pool,
            "tenant-a",
            "user-a",
            "query-a",
            "datasource-a",
            "SELECT COUNT(*) FROM orders",
        )
        .await
        .expect("exact released candidate remains executable");
        assert_eq!(valid.0, "datasource-a");
    }
}
