use super::queries::generate_conversation_summary;
use super::{
    ConversationDetailResponse, ConversationItem, ConversationListResponse, ConversationMessage,
    PaginationParams,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use serde::Deserialize;
use sqlx::Row;

pub(crate) async fn list_conversations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(_params): Query<PaginationParams>,
) -> Result<Json<ConversationListResponse>> {
    const FIXED_CONVERSATIONS_LIMIT: i64 = 50;

    let rows: Vec<(String, u64, Option<String>, Option<String>, String, String)> = sqlx::query_as(
        "SELECT id, message_count, summary, last_question, \
             strftime('%Y-%m-%d %H:%M:%S', created_at) as created_at, \
             strftime('%Y-%m-%d %H:%M:%S', updated_at) as updated_at \
             FROM nl2sql_conversations \
             WHERE tenant_id = ? AND user_id = ? AND deleted_at IS NULL \
             ORDER BY updated_at DESC LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(FIXED_CONVERSATIONS_LIMIT)
    .fetch_all(&state.db)
    .await?;

    let conversations: Vec<ConversationItem> = rows
        .into_iter()
        .map(
            |(id, message_count, summary, last_question, created_at, updated_at)| {
                ConversationItem {
                    id,
                    message_count: i64::try_from(message_count).unwrap_or(i64::MAX),
                    summary,
                    last_question,
                    created_at,
                    updated_at,
                }
            },
        )
        .collect();

    Ok(Json(ConversationListResponse {
        total: conversations.len(),
        conversations,
    }))
}

pub(crate) async fn get_conversation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(conversation_id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ConversationDetailResponse>> {
    let page: u32 = params.page.unwrap_or(1).max(1);
    let per_page: u32 = params.per_page.unwrap_or(20).clamp(1, 100);
    let offset: i64 = i64::from((page - 1).saturating_mul(per_page));
    let limit: i64 = i64::from(per_page);

    // Load conversation metadata
    let meta: Option<(String, u64, Option<String>, Option<String>, String, String)> =
        sqlx::query_as(
            "SELECT id, message_count, summary, last_question, \
                 strftime('%Y-%m-%d %H:%M:%S', created_at) as created_at, \
                 strftime('%Y-%m-%d %H:%M:%S', updated_at) as updated_at \
         FROM nl2sql_conversations WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
        )
        .bind(&conversation_id)
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await?;

    let (id, message_count_u64, summary, last_question, created_at, updated_at) =
        meta.ok_or_else(|| AppError::NotFound("Conversation not found".into()))?;
    let message_count = i64::try_from(message_count_u64).unwrap_or(i64::MAX);
    let clarification_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nl2sql_clarification_messages \
         WHERE tenant_id = ? AND conversation_id = ? AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&conversation_id)
    .fetch_one(&state.db)
    .await?;
    let query_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM nl2sql_queries \
         WHERE tenant_id = ? AND conversation_id = ? AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&conversation_id)
    .fetch_one(&state.db)
    .await?;
    let total_messages = query_count.saturating_add(clarification_count);
    let has_more = i64::from(page) * i64::from(per_page) < total_messages;

    // Load a unified timeline from SQL turns + clarification turns.
    // Page newest-first on the merged stream, then reorder ascending for display.
    let event_rows = sqlx::query(
        "SELECT * FROM ( \
            SELECT \
                'query' AS message_type, \
                q.id AS query_id, \
                q.data_source_id, \
                q.question, \
                q.generated_sql, \
                CAST(q.rows_returned AS INTEGER) AS rows_returned, \
                CAST(q.execution_ms AS INTEGER) AS execution_ms, \
                CAST(q.applied_rules_json AS TEXT) AS applied_rules_json, \
                strftime('%Y-%m-%d %H:%M:%S', q.created_at) AS created_at, \
                q.created_at AS created_sort, \
                1 AS event_order, \
                q.rowid AS event_sequence, \
                NULL AS clarification_turn, \
                NULL AS clarification_question, \
                NULL AS clarification_answer, \
                NULL AS confirmed_requirements, \
                NULL AS missing_requirements \
            FROM nl2sql_queries q \
            WHERE q.tenant_id = ? AND q.conversation_id = ? AND q.deleted_at IS NULL \
            UNION ALL \
            SELECT \
                'clarification' AS message_type, \
                cm.id AS query_id, \
                NULL AS data_source_id, \
                cm.original_question AS question, \
                NULL AS generated_sql, \
                CAST(NULL AS INTEGER) AS rows_returned, \
                CAST(NULL AS INTEGER) AS execution_ms, \
                CAST(NULL AS TEXT) AS applied_rules_json, \
                strftime('%Y-%m-%d %H:%M:%S', cm.created_at) AS created_at, \
                cm.created_at AS created_sort, \
                0 AS event_order, \
                cm.rowid AS event_sequence, \
                CAST(cm.turn AS INTEGER) AS clarification_turn, \
                cm.clarification_question AS clarification_question, \
                cm.user_input AS clarification_answer, \
                CAST(cm.confirmed_requirements AS TEXT) AS confirmed_requirements, \
                CAST(cm.missing_requirements AS TEXT) AS missing_requirements \
            FROM nl2sql_clarification_messages cm \
            WHERE cm.tenant_id = ? AND cm.conversation_id = ? AND cm.deleted_at IS NULL \
        ) timeline \
        ORDER BY created_sort DESC, event_order DESC, event_sequence DESC \
        LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&conversation_id)
    .bind(&claims.tenant_id)
    .bind(&conversation_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let mut messages: Vec<ConversationMessage> = event_rows
        .into_iter()
        .map(|row| {
            let message_type = row
                .try_get::<String, _>("message_type")
                .unwrap_or_else(|_| "query".to_string());
            let query_id = row.try_get::<String, _>("query_id").unwrap_or_default();
            let data_source_id = row
                .try_get::<Option<String>, _>("data_source_id")
                .ok()
                .flatten();
            let question = row.try_get::<String, _>("question").unwrap_or_default();
            let generated_sql = row
                .try_get::<Option<String>, _>("generated_sql")
                .ok()
                .flatten();
            let rows_returned = row
                .try_get::<Option<i64>, _>("rows_returned")
                .ok()
                .flatten();
            let execution_ms = row.try_get::<Option<i64>, _>("execution_ms").ok().flatten();
            let created_at = row.try_get::<String, _>("created_at").unwrap_or_default();
            let clarification_turn = row
                .try_get::<Option<i64>, _>("clarification_turn")
                .ok()
                .flatten()
                .map(|v| u32::try_from(v.max(0)).unwrap_or(u32::MAX));
            let clarification_question = row
                .try_get::<Option<String>, _>("clarification_question")
                .ok()
                .flatten();
            let clarification_answer = row
                .try_get::<Option<String>, _>("clarification_answer")
                .ok()
                .flatten();
            let confirmed_requirements = row
                .try_get::<Option<String>, _>("confirmed_requirements")
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok());
            let missing_requirements = row
                .try_get::<Option<String>, _>("missing_requirements")
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok());
            let applied_rules = row
                .try_get::<Option<String>, _>("applied_rules_json")
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str::<Vec<super::AppliedRuleHit>>(&v).ok());

            ConversationMessage {
                message_type,
                query_id,
                data_source_id,
                question,
                generated_sql,
                rows_returned,
                execution_ms,
                created_at,
                clarification_turn,
                clarification_question,
                clarification_answer,
                confirmed_requirements,
                missing_requirements,
                applied_rules,
                used_references: None,
            }
        })
        .collect();
    messages.reverse();

    let query_ids: Vec<String> = messages
        .iter()
        .filter(|msg| msg.message_type == "query")
        .map(|msg| msg.query_id.clone())
        .collect();
    if !query_ids.is_empty() {
        let mut usages =
            super::load_query_reference_usages(&state.db, &claims.tenant_id, &query_ids).await?;
        for msg in &mut messages {
            if msg.message_type == "query" {
                if let Some(items) = usages.remove(&msg.query_id) {
                    if !items.is_empty() {
                        msg.used_references = Some(items);
                    }
                }
            }
        }
    }

    Ok(Json(ConversationDetailResponse {
        id,
        message_count,
        total_messages,
        page,
        per_page,
        has_more,
        summary,
        last_question,
        messages,
        created_at,
        updated_at,
    }))
}

/// PATCH /api/v1/nl2sql/conversations/{conversation_id} — update conversation metadata.
#[derive(Debug, Deserialize)]
pub(crate) struct PatchConversationRequest {
    /// Optional new summary to set (triggers LLM regeneration if absent).
    pub summary: Option<String>,
    /// If true, forces re-generation of the conversation summary via LLM.
    pub regenerate_summary: Option<bool>,
}

pub(crate) async fn patch_conversation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(conversation_id): Path<String>,
    Json(req): Json<PatchConversationRequest>,
) -> Result<Json<ConversationDetailResponse>> {
    // Verify conversation belongs to this tenant
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nl2sql_conversations WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(&conversation_id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;
    existing.ok_or_else(|| AppError::NotFound("Conversation not found".into()))?;

    if req.regenerate_summary == Some(true) {
        generate_conversation_summary(&state, &claims, &conversation_id).await;
    } else if let Some(summary) = req.summary {
        let _ = sqlx::query(
            "UPDATE nl2sql_conversations SET summary = ?, summary_version = summary_version + 1 \
             WHERE id = ? AND tenant_id = ?",
        )
        .bind(&summary)
        .bind(&conversation_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await;
    }

    // Re-fetch and return updated conversation
    get_conversation(
        State(state),
        Extension(claims),
        Path(conversation_id),
        Query(PaginationParams::default()),
    )
    .await
}

/// DELETE /api/v1/nl2sql/conversations/{conversation_id} — soft-delete a conversation.
pub(crate) async fn delete_conversation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(conversation_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let affected = sqlx::query(
        "UPDATE nl2sql_conversations SET deleted_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(&conversation_id)
    .bind(&claims.tenant_id)
    .execute(&state.db)
    .await?;

    if affected.rows_affected() == 0 {
        return Err(AppError::NotFound("Conversation not found".into()));
    }

    Ok(Json(
        serde_json::json!({ "deleted": true, "id": conversation_id }),
    ))
}

// ── P0-2: Multi-datasource Agent ─────────────────────────────────────────────

// ── Agent config helpers ───────────────────────────────────────────────────────

fn max_agent_steps() -> usize {
    nl2sql_domain::config::max_agent_steps()
}

fn max_cross_ds_tables() -> usize {
    nl2sql_domain::config::max_cross_ds_tables()
}

fn max_cross_ds_rows() -> usize {
    nl2sql_domain::config::max_cross_ds_rows()
}

fn max_rows_per_step() -> usize {
    nl2sql_domain::config::max_rows_per_step()
}
