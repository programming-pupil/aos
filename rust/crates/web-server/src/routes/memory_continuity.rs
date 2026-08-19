use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get, patch, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::time::Duration;

use crate::auth::Claims;
use crate::error::AppError;
#[cfg(feature = "nl2sql")]
use crate::nl2sql::embedding::EmbeddingModel;
#[cfg(feature = "nl2sql")]
use crate::nl2sql::resolve_embedding_config;
use crate::state::AppState;

const MEMORY_SEARCH_LIMIT_DEFAULT: usize = 12;
const MEMORY_READ_MAX_CHARS: usize = 12_000;
#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
const MEMORY_PROMPT_LIMIT: usize = 6;
#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
const MEMORY_BASELINE_PROMPT_LIMIT: usize = 3;
const MEMORY_SEMANTIC_WEIGHT: f64 = 0.68;
const MEMORY_LEXICAL_WEIGHT: f64 = 0.22;
const MEMORY_RECENCY_WEIGHT: f64 = 0.05;
const MEMORY_CONFIDENCE_WEIGHT: f64 = 0.05;
const MEMORY_SEMANTIC_MIN_RELEVANCE: f64 = 0.56;
const EXACT_CONTEXT_ARCHIVE_MAX_CHARS: usize = 2_000_000;

// `Deserialize` + `PartialEq` are derived so the memory-record wire format can
// be round-tripped (`serialize → parse → serialize`) for the Super_Assistant
// serialization guarantees (Requirement 7.2). Fields that are omitted from the
// serialized form (`embedding_dimensions` when `None`, and the never-serialized
// `embedding_vector`) carry `#[serde(default)]` so parsing back is total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMemoryItem {
    pub id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub scope: String,
    pub app: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub memory_type: String,
    pub content: String,
    pub source_type: String,
    pub confidence: f64,
    pub pinned: bool,
    pub enabled: bool,
    #[serde(default)]
    pub stale_at: Option<String>,
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_dimensions: Option<i32>,
    #[serde(default, skip_serializing)]
    pub embedding_vector: Option<Vec<f32>>,
    pub created_at: String,
    pub updated_at: String,
    pub virtual_path: String,
    #[serde(default)]
    pub legacy_source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryListQuery {
    scope: Option<String>,
    app: Option<String>,
    session_id: Option<String>,
    include_disabled: Option<bool>,
    include_legacy: Option<bool>,
    source_group: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemorySourceGroup {
    Manual,
    Automatic,
}

impl MemorySourceGroup {
    fn parse(value: Option<&str>) -> Result<Option<Self>, AppError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(None),
            Some("manual") => Ok(Some(Self::Manual)),
            Some("automatic") => Ok(Some(Self::Automatic)),
            Some(_) => Err(AppError::ValidationError(
                "sourceGroup must be manual or automatic".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryListCursor {
    pinned: bool,
    updated_at: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUpsertRequest {
    pub scope: Option<String>,
    pub app: Option<String>,
    pub session_id: Option<String>,
    pub memory_type: Option<String>,
    pub content: String,
    pub source_type: Option<String>,
    pub confidence: Option<f64>,
    pub pinned: Option<bool>,
    pub enabled: Option<bool>,
    pub stale_at: Option<String>,
    pub verified_at: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MemoryPatchRequest {
    pub scope: Option<String>,
    pub app: Option<String>,
    pub session_id: Option<String>,
    pub memory_type: Option<String>,
    pub content: Option<String>,
    pub source_type: Option<String>,
    pub confidence: Option<f64>,
    pub pinned: Option<bool>,
    pub enabled: Option<bool>,
    pub stale_at: Option<Option<String>>,
    pub verified_at: Option<Option<String>>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchRequest {
    pub query: String,
    pub scope: Option<String>,
    pub app: Option<String>,
    pub session_id: Option<String>,
    pub include_legacy: Option<bool>,
    pub pinned_only: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResult {
    pub id: String,
    pub path: String,
    pub memory_type: String,
    pub app: String,
    pub scope: String,
    pub session_id: Option<String>,
    pub excerpt: String,
    pub line_start: usize,
    pub line_end: usize,
    pub score: f64,
    pub retrieval_mode: String,
    pub lexical_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<f64>,
    pub pinned: bool,
    pub source_type: String,
    pub legacy_source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExactArchiveEntry {
    pub role: String,
    pub content_kind: String,
    pub title: Option<String>,
    pub content: String,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryReadRequest {
    path: String,
    line_start: Option<usize>,
    line_end: Option<usize>,
    max_chars: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryReadResponse {
    path: String,
    content: String,
    line_start: usize,
    line_end: usize,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryNoteRequest {
    content: String,
    app: Option<String>,
    session_id: Option<String>,
    memory_type: Option<String>,
    pinned: Option<bool>,
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryConsolidateRequest {
    app: Option<String>,
    session_id: Option<String>,
    summary: Option<String>,
    turn_count: Option<i32>,
    metadata: Option<Value>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    relations: Vec<MemoryConsolidationRelation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryConsolidationRelation {
    from_memory_id: String,
    to_memory_id: String,
    relation: String,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub struct ThreadMemoryState {
    pub session_id: String,
    pub use_memories: bool,
    pub generate_memories: bool,
    pub pollution_state: String,
    pub pollution_reason: Option<String>,
    pub last_external_context_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryListResponse {
    items: Vec<AgentMemoryItem>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemorySearchResponse {
    items: Vec<MemorySearchResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemorySummaryRecord {
    id: String,
    scope: String,
    app: String,
    session_id: Option<String>,
    summary: String,
    source_type: String,
    turn_count: i32,
    metadata: Option<Value>,
    updated_at: String,
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/list", get(list_memory_items))
        .route("/items", get(list_memory_items).post(create_memory_item))
        .route(
            "/items/{memory_id}",
            patch(update_memory_item).delete(delete_memory_item),
        )
        .route("/search", post(search_memory))
        .route("/read", post(read_memory_path))
        .route("/note", post(note_memory))
        .route("/summaries", get(list_memory_summaries))
        .route("/consolidate", post(consolidate_memory))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

pub fn memory_is_sensitive(text: &str) -> bool {
    memory_engine::MemoryEngine::is_sensitive(text)
}

fn redact_exact_archive_secrets(content: &str) -> (String, bool) {
    let protected =
        runtime::protect_sensitive_text(content, runtime::configured_data_protection_mode());
    (protected.value, protected.report.redacted)
}

#[cfg(any(feature = "agent", test))]
pub fn should_consult_memory(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.chars().count() < 12 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let simple_needles = [
        "translate",
        "翻译",
        "改写",
        "format this",
        "格式化",
        "现在几点",
        "what time",
    ];
    if simple_needles.iter().any(|needle| lower.contains(needle)) && trimmed.chars().count() < 120 {
        return false;
    }
    let memory_needles = [
        "我们",
        "项目",
        "产品",
        "业务",
        "口径",
        "历史",
        "之前",
        "以后",
        "默认",
        "偏好",
        "our ",
        "project",
        "product",
        "business",
        "history",
        "previous",
        "preference",
        "default",
        "remember",
    ];
    memory_needles
        .iter()
        .any(|needle| lower.contains(needle) || trimmed.contains(needle))
        || trimmed.chars().count() > 300
}

#[cfg(any(feature = "agent", test))]
fn should_skip_memory_for_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.chars().count() < 2 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    let simple_needles = [
        "translate",
        "翻译",
        "改写",
        "format this",
        "格式化",
        "现在几点",
        "what time",
    ];
    simple_needles.iter().any(|needle| lower.contains(needle)) && trimmed.chars().count() < 120
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn build_unified_memory_prompt(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    app: &str,
    message: &str,
    mode: &str,
) -> Option<(String, Value)> {
    if mode.eq_ignore_ascii_case("off") || should_skip_memory_for_message(message) {
        return None;
    }
    if let Some(session_id) = session_id {
        let state = get_thread_memory_state(db, tenant_id, user_id, session_id).await;
        if !state.use_memories {
            return None;
        }
    }

    let pinned_only = mode.eq_ignore_ascii_case("pinned_only");
    let mut results = baseline_memory_results(
        db,
        tenant_id,
        user_id,
        app,
        session_id,
        message,
        pinned_only,
    )
    .await
    .unwrap_or_default();
    if should_consult_memory(message) {
        let request = MemorySearchRequest {
            query: message.to_string(),
            scope: None,
            app: Some(app.to_string()),
            session_id: session_id.map(ToOwned::to_owned),
            include_legacy: Some(true),
            pinned_only: pinned_only.then_some(true),
            limit: Some(MEMORY_PROMPT_LIMIT),
        };
        let searched = search_memory_internal(db, tenant_id, user_id, &request)
            .await
            .ok()?;
        let mut seen = results
            .iter()
            .map(|item| item.id.clone())
            .collect::<std::collections::HashSet<_>>();
        for item in searched {
            if seen.insert(item.id.clone()) {
                results.push(item);
            }
        }
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results.truncate(MEMORY_PROMPT_LIMIT);
    }
    if results.is_empty() {
        return None;
    }
    let lines = results
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            format!(
                "{}. [{}:{}] {}",
                idx + 1,
                item.app,
                item.memory_type,
                item.excerpt.trim()
            )
        })
        .collect::<Vec<_>>();
    let language_contract = memory_answer_language_contract(&results);
    let prompt = format!(
        "[Memory context]\nUse these memories only when relevant. The user's latest message and attached first-party data are fresher than memory. Context archive items are exact source-of-truth prior content; summaries are only orientation. Do not reveal hidden memory mechanics. If memory conflicts with the current request, prefer the current request.{language_contract}\n{}\n[/Memory context]",
        lines.join("\n")
    );
    let artifact = json!({
        "mode": mode,
        "count": results.len(),
        "items": results.iter().map(|item| json!({
            "id": item.id,
            "path": item.path,
            "type": item.memory_type,
            "app": item.app,
            "scope": item.scope,
            "sessionId": item.session_id,
            "pinned": item.pinned,
            "sourceType": item.source_type,
            "legacySource": item.legacy_source,
            "retrievalMode": item.retrieval_mode,
            "score": item.score,
            "lexicalScore": item.lexical_score,
            "semanticScore": item.semantic_score,
            "lineStart": item.line_start,
            "lineEnd": item.line_end,
        })).collect::<Vec<_>>()
    });
    Some((prompt, artifact))
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn persist_unified_memory_candidate(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    app: &str,
    user_message: &str,
) {
    let Some((memory_type, content)) = extract_explicit_memory_candidate(user_message, app) else {
        return;
    };
    if let Some(session_id) = session_id {
        let state = get_thread_memory_state(db, tenant_id, user_id, session_id).await;
        if !state.generate_memories
            || matches!(state.pollution_state.as_str(), "polluted" | "disabled")
        {
            return;
        }
    }
    let scope = if session_id.is_some() {
        "session"
    } else {
        "global"
    };
    let req = MemoryUpsertRequest {
        scope: Some(scope.to_string()),
        app: Some(app.to_string()),
        session_id: session_id.map(ToOwned::to_owned),
        memory_type: Some(memory_type),
        content,
        source_type: Some("explicit_user".to_string()),
        confidence: Some(0.94),
        pinned: Some(false),
        enabled: Some(true),
        stale_at: None,
        verified_at: None,
        metadata: Some(json!({ "createdFrom": "explicit_user_instruction" })),
    };
    let _ = create_memory_item_internal(db, tenant_id, user_id, req).await;
}

pub fn exact_archive_entry(
    role: impl Into<String>,
    content_kind: impl Into<String>,
    title: impl Into<Option<String>>,
    content: impl Into<String>,
    metadata: Option<Value>,
) -> ExactArchiveEntry {
    ExactArchiveEntry {
        role: role.into(),
        content_kind: content_kind.into(),
        title: title.into(),
        content: content.into(),
        metadata,
    }
}

pub async fn persist_exact_context_archive_entries(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    window_id: &str,
    source: &str,
    entries: Vec<ExactArchiveEntry>,
) -> usize {
    if entries.is_empty() || session_id.trim().is_empty() || window_id.trim().is_empty() {
        return 0;
    }
    let source = normalize_exact_archive_source(source);
    let window_id = normalize_exact_archive_window_id(window_id);
    let mut transaction = match db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::warn!(tenant_id, user_id, session_id, window_id = %window_id, source = %source, error = %error, "failed to begin exact context archive transaction");
            return 0;
        }
    };
    if source == "exact_turn" {
        if let Err(error) = sqlx::query(
            "DELETE FROM agent_context_archives
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?
               AND window_id = ? AND source = 'exact_turn'",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(&window_id)
        .execute(&mut *transaction)
        .await
        {
            tracing::warn!(tenant_id, user_id, session_id, window_id = %window_id, error = %error, "failed to replace exact turn archive window");
            return 0;
        }
    }
    let mut inserted = 0usize;
    for (ordinal, entry) in entries.into_iter().enumerate() {
        let role = normalize_exact_archive_role(&entry.role);
        let content_kind = normalize_exact_archive_kind(&entry.content_kind);
        let content = render_exact_archive_content(entry.title.as_deref(), &entry.content);
        if content.trim().is_empty() {
            continue;
        }
        let (content, secret_redacted) = redact_exact_archive_secrets(&content);
        let content = truncate_exact_archive_content(&content);
        let content_hash = exact_context_archive_hash(&role, ordinal, &content);
        let metadata_json = entry
            .metadata
            .or_else(|| secret_redacted.then(|| json!({})))
            .map(|value| {
                let mut object = value.as_object().cloned().unwrap_or_default();
                object
                    .entry("archivePolicy".to_string())
                    .or_insert_with(|| Value::String("exact_turn_source_of_truth".to_string()));
                if secret_redacted {
                    object.insert("secretRedacted".to_string(), Value::Bool(true));
                }
                Value::Object(object)
            })
            .and_then(|value| serde_json::to_string(&value).ok());
        let result = sqlx::query(
            r#"
            INSERT INTO agent_context_archives
              (id, tenant_id, user_id, session_id, window_id, source, role, ordinal,
               content, content_hash, content_kind, char_count, metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?))
            ON CONFLICT DO UPDATE SET
              metadata_json = COALESCE(excluded.metadata_json, metadata_json)
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(&window_id)
        .bind(&source)
        .bind(&role)
        .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
        .bind(&content)
        .bind(content_hash)
        .bind(&content_kind)
        .bind(i64::try_from(content.chars().count()).unwrap_or(i64::MAX))
        .bind(metadata_json)
        .execute(&mut *transaction)
        .await;
        match result {
            Ok(done) => {
                inserted = inserted.saturating_add(done.rows_affected() as usize);
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id,
                    user_id,
                    session_id,
                    window_id = %window_id,
                    source = %source,
                    ordinal,
                    error = %error,
                    "failed to persist exact context archive entry"
                );
                return 0;
            }
        }
    }
    if inserted == 0 {
        return 0;
    }
    match transaction.commit().await {
        Ok(()) => inserted,
        Err(error) => {
            tracing::warn!(tenant_id, user_id, session_id, window_id = %window_id, source = %source, error = %error, "failed to commit exact context archive transaction");
            0
        }
    }
}

/// Atomically replace the single exact-archive record for a logical trace
/// slot. Used for mutable observability state such as a route decision that can
/// be refined after the initial router result. Conversation/tool archives still
/// use the append-only multi-entry function above.
pub async fn replace_exact_context_archive_entry(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    window_id: &str,
    source: &str,
    entry: ExactArchiveEntry,
) -> usize {
    if session_id.trim().is_empty() || window_id.trim().is_empty() {
        return 0;
    }
    let source = normalize_exact_archive_source(source);
    let window_id = normalize_exact_archive_window_id(window_id);
    let role = normalize_exact_archive_role(&entry.role);
    let content_kind = normalize_exact_archive_kind(&entry.content_kind);
    let content = render_exact_archive_content(entry.title.as_deref(), &entry.content);
    if content.trim().is_empty() {
        return 0;
    }
    let (content, secret_redacted) = redact_exact_archive_secrets(&content);
    let content = truncate_exact_archive_content(&content);
    let content_hash = exact_context_archive_hash(&role, 0, &content);
    let metadata_json = entry
        .metadata
        .or_else(|| secret_redacted.then(|| json!({})))
        .map(|value| {
            let mut object = value.as_object().cloned().unwrap_or_default();
            object
                .entry("archivePolicy".to_string())
                .or_insert_with(|| Value::String("exact_turn_source_of_truth".to_string()));
            if secret_redacted {
                object.insert("secretRedacted".to_string(), Value::Bool(true));
            }
            Value::Object(object)
        })
        .and_then(|value| serde_json::to_string(&value).ok());

    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(error) => {
            tracing::warn!(tenant_id, user_id, session_id, error = %error, "failed to begin exact context replacement");
            return 0;
        }
    };
    let replaced = async {
        sqlx::query(
            "DELETE FROM agent_context_archives
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?
               AND window_id = ? AND source = ? AND content_kind = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(&window_id)
        .bind(&source)
        .bind(&content_kind)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO agent_context_archives
              (id, tenant_id, user_id, session_id, window_id, source, role, ordinal,
               content, content_hash, content_kind, char_count, metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, json(?))
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(&window_id)
        .bind(&source)
        .bind(&role)
        .bind(&content)
        .bind(content_hash)
        .bind(&content_kind)
        .bind(i64::try_from(content.chars().count()).unwrap_or(i64::MAX))
        .bind(metadata_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;
    match replaced {
        Ok(()) => 1,
        Err(error) => {
            tracing::warn!(
                tenant_id,
                user_id,
                session_id,
                window_id = %window_id,
                source = %source,
                content_kind = %content_kind,
                error = %error,
                "failed to replace exact context archive entry"
            );
            0
        }
    }
}

pub async fn persist_agent_turn_exact_archive(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    app: &str,
    turn_id: &str,
    user_message: &str,
    effective_user_message: Option<&str>,
    assistant_text: &str,
    tool_calls: &[agent_gateway::ToolCallRecord],
    metadata: Option<Value>,
) -> usize {
    let app = normalize_exact_archive_app(app);
    let window_id = exact_archive_window_id(&app, turn_id);
    let base_metadata = metadata.unwrap_or_else(|| json!({}));
    let mut entries = Vec::new();
    entries.push(exact_archive_entry(
        "user",
        "user_message",
        Some("User message".to_string()),
        user_message.to_string(),
        Some(merge_exact_archive_metadata(
            &base_metadata,
            json!({ "app": app, "turnId": turn_id, "part": "user_message" }),
        )),
    ));
    if let Some(effective) = effective_user_message
        .filter(|value| value.trim() != user_message.trim())
        .filter(|value| !value.trim().is_empty())
    {
        entries.push(exact_archive_entry(
            "user",
            "user_context",
            Some("Effective user message with attached/contextual evidence".to_string()),
            effective.to_string(),
            Some(merge_exact_archive_metadata(
                &base_metadata,
                json!({ "app": app, "turnId": turn_id, "part": "effective_user_message" }),
            )),
        ));
    }
    if !assistant_text.trim().is_empty() {
        entries.push(exact_archive_entry(
            "assistant",
            "assistant_answer",
            Some("Assistant final answer".to_string()),
            assistant_text.to_string(),
            Some(merge_exact_archive_metadata(
                &base_metadata,
                json!({ "app": app, "turnId": turn_id, "part": "assistant_answer" }),
            )),
        ));
    }
    for call in tool_calls {
        let content = serde_json::to_string_pretty(&json!({
            "index": call.index,
            "toolName": call.tool_name,
            "source": call.source,
            "sourceName": call.source_name,
            "input": call.input,
            "output": call.output,
            "isError": call.is_error,
            "durationMs": call.duration_ms,
        }))
        .unwrap_or_else(|_| {
            format!(
                "tool={} input=\n{}\n\noutput=\n{}\n\nisError={}",
                call.tool_name, call.input, call.output, call.is_error
            )
        });
        entries.push(exact_archive_entry(
            "tool",
            if call.is_error {
                "tool_error"
            } else {
                "tool_call"
            },
            Some(format!("Tool call #{}: {}", call.index, call.tool_name)),
            content,
            Some(merge_exact_archive_metadata(
                &base_metadata,
                json!({
                    "app": app,
                    "turnId": turn_id,
                    "part": "tool_call",
                    "toolName": call.tool_name,
                    "isError": call.is_error
                }),
            )),
        ));
    }
    persist_exact_context_archive_entries(
        db,
        tenant_id,
        user_id,
        session_id,
        &window_id,
        "exact_turn",
        entries,
    )
    .await
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn mark_thread_memory_polluted(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    reason: &str,
    metadata: Option<Value>,
) {
    let metadata_json = metadata
        .map(|value| {
            let mut object = value.as_object().cloned().unwrap_or_default();
            object
                .entry("event".to_string())
                .or_insert_with(|| Value::String("memory.polluted".to_string()));
            Value::Object(object)
        })
        .and_then(|value| serde_json::to_string(&value).ok());
    let _ = sqlx::query(
        r#"
        INSERT INTO agent_thread_memory_state
          (tenant_id, user_id, session_id, use_memories, generate_memories, pollution_state,
           pollution_reason, last_external_context_at, metadata_json)
        VALUES (?, ?, ?, 1, 1, 'polluted', ?, CURRENT_TIMESTAMP, json(?))
        ON CONFLICT DO UPDATE SET
          pollution_state = 'polluted',
          pollution_reason = excluded.pollution_reason,
          last_external_context_at = CURRENT_TIMESTAMP,
          metadata_json = excluded.metadata_json,
          updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(reason)
    .bind(metadata_json)
    .execute(db)
    .await;
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn get_thread_memory_state(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> ThreadMemoryState {
    let row = sqlx::query(
        r#"
        SELECT use_memories, generate_memories, pollution_state, pollution_reason,
               CAST(last_external_context_at AS TEXT) AS last_external_context_at
        FROM agent_thread_memory_state
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    if let Some(row) = row {
        ThreadMemoryState {
            session_id: session_id.to_string(),
            use_memories: row.get::<i8, _>("use_memories") != 0,
            generate_memories: row.get::<i8, _>("generate_memories") != 0,
            pollution_state: row.get("pollution_state"),
            pollution_reason: row.get("pollution_reason"),
            last_external_context_at: row.get("last_external_context_at"),
        }
    } else {
        ThreadMemoryState {
            session_id: session_id.to_string(),
            use_memories: true,
            generate_memories: true,
            pollution_state: "clean".to_string(),
            pollution_reason: None,
            last_external_context_at: None,
        }
    }
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn set_thread_memory_mode(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    use_memories: Option<bool>,
    generate_memories: Option<bool>,
    pollution_state: Option<&str>,
    pollution_reason: Option<&str>,
) -> Result<ThreadMemoryState, AppError> {
    let current = get_thread_memory_state(db, tenant_id, user_id, session_id).await;
    let use_memories = use_memories.unwrap_or(current.use_memories);
    let generate_memories = generate_memories.unwrap_or(current.generate_memories);
    let pollution_state = pollution_state.unwrap_or(&current.pollution_state);
    if !matches!(pollution_state, "clean" | "polluted" | "disabled") {
        return Err(AppError::ValidationError(
            "pollutionState must be clean, polluted, or disabled".to_string(),
        ));
    }
    let pollution_reason = pollution_reason
        .map(ToOwned::to_owned)
        .or(current.pollution_reason);
    sqlx::query(
        r#"
        INSERT INTO agent_thread_memory_state
          (tenant_id, user_id, session_id, use_memories, generate_memories,
           pollution_state, pollution_reason)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT DO UPDATE SET
          use_memories = excluded.use_memories,
          generate_memories = excluded.generate_memories,
          pollution_state = excluded.pollution_state,
          pollution_reason = excluded.pollution_reason,
          updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(if use_memories { 1 } else { 0 })
    .bind(if generate_memories { 1 } else { 0 })
    .bind(pollution_state)
    .bind(pollution_reason)
    .execute(db)
    .await?;
    Ok(get_thread_memory_state(db, tenant_id, user_id, session_id).await)
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn record_memory_citations(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    turn_id: Option<&str>,
    artifact: &Value,
) {
    let mut items = Vec::new();
    collect_memory_citation_items(artifact, &mut items);
    if items.is_empty() {
        return;
    };
    for item in items {
        let memory_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("MEMORY.md");
        let (line_start, line_end) = memory_citation_line_range(item);
        if memory_id.is_empty() {
            continue;
        }
        let _ = sqlx::query(
            r#"
            INSERT INTO agent_memory_citations
              (id, tenant_id, user_id, session_id, turn_id, memory_id, path, line_start, line_end,
               note, metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'memory context injected', json(?))
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(turn_id)
        .bind(memory_id)
        .bind(path)
        .bind(line_start)
        .bind(line_end)
        .bind(serde_json::to_string(item).ok())
        .execute(db)
        .await;
    }
}

#[cfg(any(feature = "agent", test))]
fn collect_memory_citation_items<'a>(artifact: &'a Value, out: &mut Vec<&'a Value>) {
    if let Some(sources) = artifact.get("sources").and_then(Value::as_array) {
        for source in sources {
            if source
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "recent_history_context")
            {
                continue;
            }
            if let Some(nested) = source.get("artifact") {
                collect_memory_citation_items(nested, out);
            }
        }
    }
    if let Some(items) = artifact.get("items").and_then(Value::as_array) {
        out.extend(items.iter());
    }
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
pub async fn record_memory_tool_citations(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    turn_id: Option<&str>,
    tool_calls: &[agent_gateway::ToolCallRecord],
) {
    for call in tool_calls {
        if !matches!(
            call.tool_name.as_str(),
            "memory_search"
                | "memory.search"
                | "memory_list"
                | "memory.list"
                | "memory_read"
                | "memory.read"
        ) || call.is_error
        {
            continue;
        }
        if let Some(artifact) = memory_artifact_from_tool_call(call) {
            record_memory_citations(db, tenant_id, user_id, session_id, turn_id, &artifact).await;
        }
    }
}

#[cfg(any(feature = "agent", test))]
fn memory_artifact_from_tool_call(call: &agent_gateway::ToolCallRecord) -> Option<Value> {
    let output = serde_json::from_str::<Value>(&call.output).ok()?;
    let items = if let Some(items) = output.get("items").and_then(Value::as_array) {
        items
            .iter()
            .filter_map(memory_tool_item_to_citation)
            .collect::<Vec<_>>()
    } else if let Some(path) = output.get("path").and_then(Value::as_str) {
        let line_start = output
            .get("lineStart")
            .or_else(|| output.get("line_start"))
            .or_else(|| output.get("start_line_number"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let line_end = output
            .get("lineEnd")
            .or_else(|| output.get("line_end"))
            .and_then(Value::as_u64)
            .unwrap_or(line_start);
        vec![json!({
            "id": format!("memory-path:{path}"),
            "path": path,
            "lineStart": line_start,
            "lineEnd": line_end,
            "note": "memory read by runtime tool",
            "sourceType": "runtime_memory_tool",
        })]
    } else {
        Vec::new()
    };
    (!items.is_empty()).then(|| {
        json!({
            "mode": "runtime_tool",
            "tool": call.tool_name,
            "count": items.len(),
            "items": items,
        })
    })
}

#[cfg(any(feature = "agent", test))]
fn memory_tool_item_to_citation(item: &Value) -> Option<Value> {
    let path = item.get("path").and_then(Value::as_str)?;
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("memory-path:{path}"));
    Some(json!({
        "id": id,
        "path": path,
        "type": item.get("memoryType").or_else(|| item.get("memory_type")).cloned(),
        "app": item.get("app").cloned(),
        "scope": item.get("scope").cloned(),
        "sessionId": item.get("sessionId").or_else(|| item.get("session_id")).cloned(),
        "lineStart": item.get("lineStart").or_else(|| item.get("line_start")).cloned(),
        "lineEnd": item.get("lineEnd").or_else(|| item.get("line_end")).cloned(),
        "sourceType": item.get("sourceType").or_else(|| item.get("source_type")).cloned(),
        "legacySource": item.get("legacySource").or_else(|| item.get("legacy_source")).cloned(),
        "note": "memory used by runtime tool",
    }))
}

#[cfg(any(feature = "agent", test))]
fn memory_citation_line_range(item: &Value) -> (Option<i32>, Option<i32>) {
    let line_start = item
        .get("lineStart")
        .and_then(Value::as_u64)
        .and_then(|value| i32::try_from(value).ok());
    let line_end = item
        .get("lineEnd")
        .and_then(Value::as_u64)
        .and_then(|value| i32::try_from(value).ok());
    (line_start, line_end)
}

#[cfg(any(feature = "agent", test))]
fn extract_explicit_memory_candidate(message: &str, app: &str) -> Option<(String, String)> {
    let trimmed = message.trim();
    if trimmed.chars().count() < 6 || memory_is_sensitive(trimmed) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let conditional_preference = extract_memory_condition_phrase(trimmed).is_some()
        && (looks_like_preference_memory(trimmed)
            || detect_memory_answer_language(trimmed).is_some());
    let explicit_markers = [
        "以后",
        "今后",
        "记住",
        "默认",
        "偏好",
        "每当",
        "只要",
        "业务背景",
        "口径",
        "我们产品",
        "remember",
        "remember that",
        "remember this",
        "from now on",
        "always",
        "by default",
        "prefer",
        "our product",
        "business context",
    ];
    let explicit = conditional_preference
        || explicit_markers
            .iter()
            .any(|needle| lower.contains(needle) || trimmed.contains(needle));
    if !explicit {
        return None;
    }
    let memory_type = if lower.contains("format")
        || lower.contains("language")
        || lower.contains("concise")
        || trimmed.contains("格式")
        || trimmed.contains("口吻")
        || trimmed.contains("中文")
        || trimmed.contains("英文")
        || trimmed.contains("日文")
        || trimmed.contains("日语")
        || trimmed.contains("日語")
        || trimmed.contains("日本語")
        || trimmed.contains("俄文")
        || trimmed.contains("俄语")
        || trimmed.contains("俄語")
        || trimmed.contains("俄罗斯语")
        || trimmed.contains("俄羅斯語")
        || trimmed.contains("回复")
        || trimmed.contains("回答")
        || trimmed.contains("简洁")
        || lower.contains("japanese")
        || lower.contains("russian")
        || lower.contains("русский")
        || lower.contains("reply")
        || lower.contains("respond")
        || lower.contains("answer")
    {
        "preference"
    } else if app.eq_ignore_ascii_case("pm")
        || trimmed.contains("业务")
        || trimmed.contains("产品")
        || lower.contains("business")
        || lower.contains("product")
    {
        "business_context"
    } else {
        "project_fact"
    };
    let content = compact_text(trimmed, 900);
    (!content.is_empty() && !memory_is_sensitive(&content))
        .then_some((memory_type.to_string(), content))
}

async fn list_memory_items(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<MemoryListResponse>, AppError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let source_group = MemorySourceGroup::parse(query.source_group.as_deref())?;
    let cursor = query
        .cursor
        .as_deref()
        .map(decode_memory_list_cursor)
        .transpose()?;
    let fetch_limit = limit.saturating_add(1);
    let mut items = list_unified_memory_items_page(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        query.scope.as_deref(),
        query.app.as_deref(),
        query.session_id.as_deref(),
        query.include_disabled.unwrap_or(false),
        source_group,
        cursor.as_ref(),
        fetch_limit,
    )
    .await?;
    if query.include_legacy.unwrap_or(true) {
        items.extend(
            list_legacy_memory_items_page(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                query.app.as_deref(),
                query.session_id.as_deref(),
                query.include_disabled.unwrap_or(false),
                source_group,
                cursor.as_ref(),
                fetch_limit,
            )
            .await?,
        );
    }
    items.sort_by(memory_list_order);
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = has_more
        .then(|| items.last().map(encode_memory_list_cursor))
        .flatten();
    Ok(Json(MemoryListResponse {
        items,
        next_cursor,
        has_more,
    }))
}

/// Materialize every new Unified Memory write as a structured fact. The text
/// row remains a searchable projection (and may carry an embedding), while
/// current/superseded semantics are read from `structured_memory_facts`.
/// Write the structured fact and disable superseded text projections on the
/// caller's transaction. This is deliberately separate from the pool wrapper
/// so a normal Memory upsert can commit the fact and searchable row atomically.
async fn persist_structured_memory_projection_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    user_id: &str,
    item_id: &str,
    scope: &str,
    app: &str,
    session_id: Option<&str>,
    memory_type: &str,
    content: &str,
    source_type: &str,
    confidence: f64,
    pinned: bool,
    metadata_json: Option<&str>,
) -> Result<(), sqlx::Error> {
    let content_hash = memory_engine::stable_source_hash(content);
    let session_key = session_id.unwrap_or_default();
    let id = format!(
        "structured-memory:{}",
        memory_engine::stable_source_hash(&format!(
            "{tenant_id}:{user_id}:{scope}:{app}:{session_key}:{memory_type}:{content_hash}"
        ))
    );
    let subject = serde_json::json!({
        "memoryType": memory_type,
        "sessionId": session_id,
        "sourceType": source_type,
    });
    let subject_json = subject.to_string();
    let predicate = memory_type.to_string();
    let evidence_id = metadata_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("sourceEvidence").cloned())
        .and_then(|value| {
            value
                .get("turnId")
                .and_then(serde_json::Value::as_str)
                .map(|turn| format!("turn:{turn}"))
        })
        .unwrap_or_else(|| format!("memory:{item_id}"));
    let candidate_json = serde_json::json!({
        "projectionMemoryId": item_id,
        "content": content,
        "sourceType": source_type,
        "pinned": pinned,
        "confidence": confidence,
    });
    let result = sqlx::query(
        "INSERT INTO structured_memory_facts
            (id, tenant_id, user_id, scope, app, session_id, channel, kind,
             subject_json, predicate, value_json, text, evidence_id, evidence_hash,
             observed_at, valid_until, confidence, sensitivity, current,
             conflict_group, projection_memory_id, candidate_json,
             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP,
                 NULL, ?, 'internal', 1, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET
             text = excluded.text, value_json = excluded.value_json,
             confidence = MAX(structured_memory_facts.confidence, excluded.confidence),
             current = 1, projection_memory_id = excluded.projection_memory_id,
             candidate_json = excluded.candidate_json,
             updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(scope)
    .bind(app)
    .bind(session_id)
    .bind(if source_type == "compaction" {
        "continuity_state"
    } else {
        "long_term_memory"
    })
    .bind(memory_type)
    .bind(&subject_json)
    .bind(&predicate)
    .bind(serde_json::Value::String(content.to_string()).to_string())
    .bind(content)
    .bind(&evidence_id)
    .bind(&content_hash)
    .bind(confidence.clamp(0.0, 1.0))
    .bind(memory_engine::stable_source_hash(&format!(
        "{subject_json}:{predicate}"
    )))
    .bind(item_id)
    .bind(candidate_json.to_string())
    .execute(&mut **tx)
    .await;
    match result {
        Ok(_) => {
            let old_projection_ids = sqlx::query_scalar::<_, String>(
                "SELECT projection_memory_id FROM structured_memory_facts
                 WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ?
                   AND (session_id = ? OR (session_id IS NULL AND ? IS NULL))
                   AND subject_json = ? AND predicate = ? AND current = 1 AND id <> ?
                   AND projection_memory_id IS NOT NULL",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(scope)
            .bind(app)
            .bind(session_id)
            .bind(session_id)
            .bind(&subject_json)
            .bind(&predicate)
            .bind(&id)
            .fetch_all(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE structured_memory_facts
                 SET current = 0, superseded_by = ?, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ?
                   AND (session_id = ? OR (session_id IS NULL AND ? IS NULL))
                   AND subject_json = ? AND predicate = ? AND current = 1 AND id <> ?",
            )
            .bind(&id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(scope)
            .bind(app)
            .bind(session_id)
            .bind(session_id)
            .bind(&subject_json)
            .bind(&predicate)
            .bind(&id)
            .execute(&mut **tx)
            .await?;
            for projection_id in old_projection_ids {
                if projection_id == item_id {
                    continue;
                }
                sqlx::query(
                    "UPDATE agent_memory_items SET enabled = 0, updated_at = CURRENT_TIMESTAMP
                     WHERE tenant_id = ? AND user_id = ? AND id = ?",
                )
                .bind(tenant_id)
                .bind(user_id)
                .bind(projection_id)
                .execute(&mut **tx)
                .await?;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn memory_list_order(left: &AgentMemoryItem, right: &AgentMemoryItem) -> Ordering {
    right
        .pinned
        .cmp(&left.pinned)
        .then_with(|| right.updated_at.cmp(&left.updated_at))
        .then_with(|| right.id.cmp(&left.id))
}

fn encode_memory_list_cursor(item: &AgentMemoryItem) -> String {
    let cursor = MemoryListCursor {
        pinned: item.pinned,
        updated_at: item.updated_at.clone(),
        id: item.id.clone(),
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor).unwrap_or_default())
}

fn decode_memory_list_cursor(value: &str) -> Result<MemoryListCursor, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppError::ValidationError("invalid memory list cursor".to_string()))?;
    let cursor: MemoryListCursor = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::ValidationError("invalid memory list cursor".to_string()))?;
    if cursor.updated_at.trim().is_empty() || cursor.id.trim().is_empty() {
        return Err(AppError::ValidationError(
            "invalid memory list cursor".to_string(),
        ));
    }
    Ok(cursor)
}

async fn list_unified_memory_items_page(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    scope: Option<&str>,
    app: Option<&str>,
    session_id: Option<&str>,
    include_disabled: bool,
    source_group: Option<MemorySourceGroup>,
    cursor: Option<&MemoryListCursor>,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        SELECT id, tenant_id, user_id, scope, app, session_id, memory_type, content,
               source_type, CAST(confidence AS DOUBLE) AS confidence, pinned, enabled,
               CAST(stale_at AS TEXT) AS stale_at, CAST(verified_at AS TEXT) AS verified_at,
               CAST(metadata_json AS TEXT) AS metadata_json,
               NULL AS embedding_model, NULL AS embedding_dimensions, NULL AS embedding_json,
               CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_memory_items
        WHERE tenant_id = "#,
    );
    query
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id);
    if !include_disabled {
        query.push(
            " AND EXISTS (SELECT 1 FROM structured_memory_facts fact WHERE fact.projection_memory_id = agent_memory_items.id AND fact.tenant_id = agent_memory_items.tenant_id AND fact.user_id = agent_memory_items.user_id AND fact.current = 1)",
        );
    }
    if let Some(scope) = scope {
        query.push(" AND scope = ").push_bind(scope);
    }
    if let Some(app) = app {
        query
            .push(" AND (app = ")
            .push_bind(app)
            .push(" OR app = 'shared')");
    }
    if let Some(session_id) = session_id {
        query
            .push(" AND (session_id = ")
            .push_bind(session_id)
            .push(" OR session_id IS NULL)");
    }
    if !include_disabled {
        query.push(" AND enabled = 1");
    }
    match source_group {
        Some(MemorySourceGroup::Manual) => query.push(" AND source_type = 'manual'"),
        Some(MemorySourceGroup::Automatic) => {
            query.push(" AND (source_type IS NULL OR source_type <> 'manual')")
        }
        None => &mut query,
    };
    if let Some(cursor) = cursor {
        let pinned = if cursor.pinned { 1_i8 } else { 0_i8 };
        query
            .push(" AND (pinned < ")
            .push_bind(pinned)
            .push(" OR (pinned = ")
            .push_bind(pinned)
            .push(" AND updated_at < ")
            .push_bind(&cursor.updated_at)
            .push(") OR (pinned = ")
            .push_bind(pinned)
            .push(" AND updated_at = ")
            .push_bind(&cursor.updated_at)
            .push(" AND id < ")
            .push_bind(&cursor.id)
            .push("))");
    }
    query
        .push(" ORDER BY pinned DESC, updated_at DESC, id DESC LIMIT ")
        .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
    let rows = query.build().fetch_all(db).await?;
    Ok(rows
        .iter()
        .map(|row| row_to_memory_item(row, None))
        .collect())
}

/// UI/compatibility listing path. It intentionally omits embedding payloads;
/// agent recall continues to use `list_unified_memory_items` below.
pub(crate) async fn list_unified_memory_items_lightweight(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    scope: Option<&str>,
    app: Option<&str>,
    session_id: Option<&str>,
    include_disabled: bool,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    list_unified_memory_items_page(
        db,
        tenant_id,
        user_id,
        scope,
        app,
        session_id,
        include_disabled,
        None,
        None,
        limit,
    )
    .await
}

async fn list_legacy_memory_items_page(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    app: Option<&str>,
    session_id: Option<&str>,
    include_disabled: bool,
    source_group: Option<MemorySourceGroup>,
    cursor: Option<&MemoryListCursor>,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let mut items = Vec::new();
    if app.is_none_or(|value| value.eq_ignore_ascii_case("chat") || value == "shared") {
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            r#"
            SELECT id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
                   pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json,
                   CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
            FROM chat_memories
            WHERE tenant_id = "#,
        );
        query
            .push_bind(tenant_id)
            .push(" AND user_id = ")
            .push_bind(user_id);
        if !include_disabled {
            query.push(" AND enabled = 1");
        }
        match source_group {
            Some(MemorySourceGroup::Manual) => query.push(" AND source = 'manual'"),
            Some(MemorySourceGroup::Automatic) => {
                query.push(" AND (source IS NULL OR source <> 'manual')")
            }
            None => &mut query,
        };
        if let Some(cursor) = cursor {
            let pinned = if cursor.pinned { 1_i8 } else { 0_i8 };
            query
                .push(" AND (pinned < ")
                .push_bind(pinned)
                .push(" OR (pinned = ")
                .push_bind(pinned)
                .push(" AND updated_at < ")
                .push_bind(&cursor.updated_at)
                .push(") OR (pinned = ")
                .push_bind(pinned)
                .push(" AND updated_at = ")
                .push_bind(&cursor.updated_at)
                .push(" AND ('legacy-chat-' || id) < ")
                .push_bind(&cursor.id)
                .push("))");
        }
        query
            .push(" ORDER BY pinned DESC, updated_at DESC, id DESC LIMIT ")
            .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
        let rows = query.build().fetch_all(db).await?;
        items.extend(
            rows.iter()
                .map(|row| legacy_chat_row_to_item(row, tenant_id, user_id)),
        );
    }
    if app.is_none_or(|value| value.eq_ignore_ascii_case("pm") || value == "shared") {
        if let Some(session_id) = session_id {
            let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                r#"
                SELECT id, session_id, memory_type, content, source,
                       CAST(confidence AS DOUBLE) AS confidence, pinned, enabled,
                       CAST(metadata_json AS TEXT) AS metadata_json,
                       CAST(created_at AS TEXT) AS created_at,
                       CAST(updated_at AS TEXT) AS updated_at
                FROM pm_session_memories
                WHERE tenant_id = "#,
            );
            query
                .push_bind(tenant_id)
                .push(" AND user_id = ")
                .push_bind(user_id)
                .push(" AND session_id = ")
                .push_bind(session_id);
            if !include_disabled {
                query.push(" AND enabled = 1");
            }
            match source_group {
                Some(MemorySourceGroup::Manual) => query.push(" AND source = 'manual'"),
                Some(MemorySourceGroup::Automatic) => {
                    query.push(" AND (source IS NULL OR source <> 'manual')")
                }
                None => &mut query,
            };
            if let Some(cursor) = cursor {
                let pinned = if cursor.pinned { 1_i8 } else { 0_i8 };
                query
                    .push(" AND (pinned < ")
                    .push_bind(pinned)
                    .push(" OR (pinned = ")
                    .push_bind(pinned)
                    .push(" AND updated_at < ")
                    .push_bind(&cursor.updated_at)
                    .push(") OR (pinned = ")
                    .push_bind(pinned)
                    .push(" AND updated_at = ")
                    .push_bind(&cursor.updated_at)
                    .push(" AND ('legacy-pm-' || id) < ")
                    .push_bind(&cursor.id)
                    .push("))");
            }
            query
                .push(" ORDER BY pinned DESC, updated_at DESC, id DESC LIMIT ")
                .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
            let rows = query.build().fetch_all(db).await?;
            items.extend(
                rows.iter()
                    .map(|row| legacy_pm_row_to_item(row, tenant_id, user_id)),
            );
        }
    }
    Ok(items)
}

async fn create_memory_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<MemoryUpsertRequest>,
) -> Result<Json<AgentMemoryItem>, AppError> {
    let item = create_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, req).await?;
    Ok(Json(item))
}

async fn update_memory_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(memory_id): Path<String>,
    Json(req): Json<MemoryPatchRequest>,
) -> Result<Json<AgentMemoryItem>, AppError> {
    let item =
        update_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id, req)
            .await?;
    Ok(Json(item))
}

/// Update a single Unified_Memory item by id. Shared write path so legacy
/// endpoints (`/chat/memories`, `pm-memories`) converge onto `/memory/items`
/// instead of re-implementing an `UPDATE` against a parallel table.
pub(crate) async fn update_memory_item_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    memory_id: &str,
    req: MemoryPatchRequest,
) -> Result<AgentMemoryItem, AppError> {
    if let Some(content) = req.content.as_deref() {
        memory_engine::MemoryEngine::admit_text(content).map_err(|error| {
            AppError::ValidationError(format!("memory content was rejected: {error}"))
        })?;
    }
    let existing = get_unified_memory_item(db, tenant_id, user_id, memory_id)
        .await?
        .ok_or_else(|| AppError::NotFound("memory not found".to_string()))?;
    let content_changed = req
        .content
        .as_ref()
        .is_some_and(|content| content.trim() != existing.content.trim());
    let existing_embedding_model = existing.embedding_model.clone();
    let existing_embedding_dimensions = existing.embedding_dimensions;
    let existing_embedding_json = existing
        .embedding_vector
        .as_deref()
        .and_then(serialize_memory_vector);
    let scope = req.scope.unwrap_or(existing.scope);
    let app = req.app.unwrap_or(existing.app);
    let session_id = req.session_id.or(existing.session_id);
    let session_key = session_id.clone().unwrap_or_default();
    let memory_type = req.memory_type.unwrap_or(existing.memory_type);
    let content = req.content.unwrap_or(existing.content);
    let content_hash = content_hash(&content);
    let embedding = embed_memory_text_best_effort(db, tenant_id, &app, &content).await;
    let embedding_model = embedding
        .as_ref()
        .map(|item| item.model.clone())
        .or_else(|| {
            (!content_changed)
                .then(|| existing_embedding_model)
                .flatten()
        });
    let embedding_dimensions = embedding
        .as_ref()
        .map(|item| i32::try_from(item.vector.len()).unwrap_or(i32::MAX))
        .or_else(|| {
            (!content_changed)
                .then_some(existing_embedding_dimensions)
                .flatten()
        });
    let embedding_json = embedding
        .as_ref()
        .and_then(|item| serialize_memory_vector(&item.vector))
        .or_else(|| {
            (!content_changed)
                .then(|| existing_embedding_json)
                .flatten()
        });
    let metadata_json = req
        .metadata
        .or(existing.metadata)
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let source_type = req.source_type.unwrap_or(existing.source_type);
    let confidence = req
        .confidence
        .unwrap_or(existing.confidence)
        .clamp(0.0, 1.0);
    let pinned = req.pinned.unwrap_or(existing.pinned);
    let enabled = req.enabled.unwrap_or(existing.enabled);
    let stale_at = req.stale_at.flatten().or(existing.stale_at);
    let verified_at = req.verified_at.flatten().or(existing.verified_at);
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    sqlx::query(
        r#"
        UPDATE agent_memory_items
        SET scope = ?, app = ?, session_id = ?, session_key = ?, memory_type = ?, content = ?, content_hash = ?,
            source_type = ?, confidence = ?, pinned = ?, enabled = ?, stale_at = ?,
            verified_at = ?, metadata_json = json(?),
            embedding_model = ?, embedding_dimensions = ?, embedding_json = ?
        WHERE tenant_id = ? AND user_id = ? AND id = ?
        "#,
    )
    .bind(&scope)
    .bind(&app)
    .bind(&session_id)
    .bind(&session_key)
    .bind(&memory_type)
    .bind(&content)
    .bind(content_hash)
    .bind(&source_type)
    .bind(confidence)
    .bind(if pinned { 1 } else { 0 })
    .bind(if enabled { 1 } else { 0 })
    .bind(stale_at)
    .bind(verified_at)
    .bind(metadata_json.as_deref())
    .bind(embedding_model)
    .bind(embedding_dimensions)
    .bind(embedding_json)
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .execute(&mut *tx)
    .await?;
    if enabled {
        persist_structured_memory_projection_in_transaction(
            &mut tx,
            tenant_id,
            user_id,
            memory_id,
            &scope,
            &app,
            session_id.as_deref(),
            &memory_type,
            &content,
            &source_type,
            confidence,
            pinned,
            metadata_json.as_deref(),
        )
        .await?;
    } else {
        sqlx::query(
            "UPDATE structured_memory_facts
             SET current = 0, valid_until = COALESCE(valid_until, CURRENT_TIMESTAMP),
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ? AND current = 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(memory_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let item = get_unified_memory_item(db, tenant_id, user_id, memory_id)
        .await?
        .ok_or_else(|| AppError::NotFound("memory not found".to_string()))?;
    Ok(item)
}

async fn delete_memory_item(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(memory_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let deleted =
        delete_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, &memory_id).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

/// Delete a single Unified_Memory item by id. Shared write path for the
/// converged legacy endpoints.
pub(crate) async fn delete_memory_item_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    memory_id: &str,
) -> Result<bool, AppError> {
    // Memory deletion is a graph operation, not just removal of the visible
    // row. Keep the operation tenant/user scoped and atomic so a deleted item
    // cannot remain retrievable through relations, citations, summaries, or a
    // later consolidation pass.
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let session_id = sqlx::query_scalar::<sqlx::Sqlite, Option<String>>(
        "SELECT session_id FROM agent_memory_items
         WHERE tenant_id = ? AND user_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let affected = sqlx::query(
        "DELETE FROM agent_memory_items WHERE tenant_id = ? AND user_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if affected == 0 {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query(
        "UPDATE structured_memory_facts
         SET current = 0, valid_until = COALESCE(valid_until, CURRENT_TIMESTAMP),
             candidate_json = json_set(candidate_json, '$.forgotten', json('true')),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ? AND current = 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM agent_memory_relations
         WHERE tenant_id = ? AND user_id = ?
           AND (from_memory_id = ? OR to_memory_id = ?)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .bind(memory_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM agent_memory_citations
         WHERE tenant_id = ? AND user_id = ? AND memory_id = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(memory_id)
    .execute(&mut *tx)
    .await?;
    // A summary can contain the deleted item verbatim. Remove the affected
    // session summary so the next consolidation pass rebuilds it from the
    // remaining evidence instead of serving stale text.
    if let Some(session_id) = session_id.as_deref() {
        sqlx::query(
            "DELETE FROM agent_memory_summaries
             WHERE tenant_id = ? AND user_id = ? AND session_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

async fn search_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<MemorySearchRequest>,
) -> Result<Json<MemorySearchResponse>, AppError> {
    let items = search_memory_internal(&state.db, &claims.tenant_id, &claims.sub, &req).await?;
    Ok(Json(MemorySearchResponse { items }))
}

async fn read_memory_path(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<MemoryReadRequest>,
) -> Result<Json<MemoryReadResponse>, AppError> {
    let max_chars = req
        .max_chars
        .unwrap_or(MEMORY_READ_MAX_CHARS)
        .clamp(500, 32_000);
    let content = resolve_virtual_memory_path(&state.db, &claims.tenant_id, &claims.sub, &req.path)
        .await?
        .ok_or_else(|| AppError::NotFound("memory path not found".to_string()))?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = req.line_start.unwrap_or(1).max(1);
    let end = req
        .line_end
        .unwrap_or(lines.len())
        .max(start)
        .min(lines.len());
    let selected = if lines.is_empty() {
        String::new()
    } else {
        lines[start - 1..end].join("\n")
    };
    let (content, truncated) = truncate_with_flag(&selected, max_chars);
    Ok(Json(MemoryReadResponse {
        path: req.path,
        content,
        line_start: start,
        line_end: end,
        truncated,
    }))
}

async fn note_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<MemoryNoteRequest>,
) -> Result<Json<AgentMemoryItem>, AppError> {
    let upsert = MemoryUpsertRequest {
        scope: Some(if req.session_id.is_some() {
            "session".to_string()
        } else {
            "global".to_string()
        }),
        app: req.app,
        session_id: req.session_id,
        memory_type: req.memory_type.or_else(|| Some("note".to_string())),
        content: req.content,
        source_type: Some("manual".to_string()),
        confidence: Some(1.0),
        pinned: req.pinned,
        enabled: Some(true),
        stale_at: None,
        verified_at: Some(chrono::Utc::now().naive_utc().to_string()),
        metadata: req.metadata,
    };
    let item =
        create_memory_item_internal(&state.db, &claims.tenant_id, &claims.sub, upsert).await?;
    Ok(Json(item))
}

async fn list_memory_summaries(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT id, scope, app, session_id, summary, source_type, turn_count,
               CAST(metadata_json AS TEXT) AS metadata_json, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_memory_summaries
        WHERE tenant_id = ? AND user_id = ?
        ORDER BY updated_at DESC
        LIMIT 100
        "#,
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await?;
    let items = rows
        .iter()
        .map(|row| MemorySummaryRecord {
            id: row.get("id"),
            scope: row.get("scope"),
            app: row.get("app"),
            session_id: row.get("session_id"),
            summary: row.get("summary"),
            source_type: row.get("source_type"),
            turn_count: row.get("turn_count"),
            metadata: parse_json(row.get("metadata_json")),
            updated_at: row.get("updated_at"),
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "items": items })))
}

async fn consolidate_memory(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<MemoryConsolidateRequest>,
) -> Result<Json<Value>, AppError> {
    let result =
        consolidate_memory_internal(&state.db, &claims.tenant_id, &claims.sub, req).await?;
    Ok(Json(result))
}

async fn consolidate_memory_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    req: MemoryConsolidateRequest,
) -> Result<Value, AppError> {
    let app = normalize_app(req.app.as_deref());
    let scope = if req.session_id.is_some() {
        "session".to_string()
    } else {
        "global".to_string()
    };
    let summary = req
        .summary
        .map(|value| compact_text(&value, 8_000))
        .unwrap_or_else(|| "Manual consolidation checkpoint.".to_string());
    let session_key = req.session_id.clone().unwrap_or_default();
    let cursor = req.cursor.clone().unwrap_or_else(|| {
        format!(
            "{}:{}",
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            summary.len()
        )
    });
    let id = format!(
        "summary:{}",
        content_hash(&format!(
            "{}|{}|{}|{}|{}",
            tenant_id, user_id, scope, app, session_key
        ))
    );
    let metadata_json = req
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let existing_cursor = sqlx::query_scalar::<_, String>(
        "SELECT cursor FROM agent_memory_consolidation_cursors
         WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ? AND session_key = ?",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&session_key)
    .fetch_optional(&mut *tx)
    .await?;
    if existing_cursor.as_deref() == Some(cursor.as_str()) {
        tx.commit().await?;
        return Ok(json!({
            "ok": true,
            "idempotent": true,
            "scope": scope,
            "app": app,
            "sessionId": req.session_id,
            "cursor": cursor,
            "relationCount": req.relations.len()
        }));
    }
    for relation in &req.relations {
        let temporal_relation = memory_engine::MemoryEngine::temporal_relation(&relation.relation)
            .map_err(|error| AppError::ValidationError(error.to_string()))?;
        let relation_kind = match temporal_relation {
            memory_engine::TemporalRelation::Supersedes => "supersedes",
            memory_engine::TemporalRelation::ConflictsWith => "conflicts_with",
        };
        let valid_ids: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_memory_items
             WHERE tenant_id = ? AND user_id = ? AND id IN (?, ?)",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(&relation.from_memory_id)
        .bind(&relation.to_memory_id)
        .fetch_one(&mut *tx)
        .await?;
        if valid_ids != 2 {
            return Err(AppError::ValidationError(
                "memory relation references an inaccessible item".to_string(),
            ));
        }
        sqlx::query(
            "INSERT INTO agent_memory_relations
                (id, tenant_id, user_id, from_memory_id, to_memory_id, relation, reason, source_cursor)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(tenant_id, user_id, from_memory_id, to_memory_id, relation) DO UPDATE SET
                reason = excluded.reason, source_cursor = excluded.source_cursor",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(user_id)
        .bind(&relation.from_memory_id)
        .bind(&relation.to_memory_id)
        .bind(relation_kind)
        .bind(compact_text(&relation.reason, 1_000))
        .bind(&cursor)
        .execute(&mut *tx)
        .await?;
        if matches!(
            temporal_relation,
            memory_engine::TemporalRelation::Supersedes
        ) {
            let successor_fact_id = sqlx::query_scalar::<_, String>(
                "SELECT id FROM structured_memory_facts
                 WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ? AND current = 1
                 ORDER BY updated_at DESC LIMIT 1",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(&relation.to_memory_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(successor_fact_id) = successor_fact_id else {
                return Err(AppError::ValidationError(
                    "superseding memory has no canonical structured fact".to_string(),
                ));
            };
            sqlx::query(
                "UPDATE structured_memory_facts
                 SET current = 0, superseded_by = ?,
                     valid_until = COALESCE(valid_until, CURRENT_TIMESTAMP),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND user_id = ? AND projection_memory_id = ? AND current = 1",
            )
            .bind(&successor_fact_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(&relation.from_memory_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_memory_items
                 SET enabled = 0, stale_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND user_id = ? AND id = ?",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(&relation.from_memory_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(
        r#"
        INSERT INTO agent_memory_summaries
          (id, tenant_id, user_id, scope, app, session_id, session_key, summary, source_type, turn_count, metadata_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'session_summary', ?, json(?))
        ON CONFLICT(id) DO UPDATE SET
          summary = excluded.summary,
          source_type = excluded.source_type,
          turn_count = excluded.turn_count,
          metadata_json = excluded.metadata_json,
          updated_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&req.session_id)
    .bind(&session_key)
    .bind(&summary)
    .bind(req.turn_count.unwrap_or(0))
    .bind(metadata_json)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO agent_memory_consolidation_cursors
            (tenant_id, user_id, scope, app, session_key, cursor, revision)
         VALUES (?, ?, ?, ?, ?, ?, 1)
         ON CONFLICT(tenant_id, user_id, scope, app, session_key) DO UPDATE SET
            cursor = excluded.cursor, revision = revision + 1, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&session_key)
    .bind(&cursor)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({
        "ok": true,
        "idempotent": false,
        "scope": scope,
        "app": app,
        "sessionId": req.session_id,
        "cursor": cursor,
        "relationCount": req.relations.len()
    }))
}

pub(crate) async fn create_memory_item_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    req: MemoryUpsertRequest,
) -> Result<AgentMemoryItem, AppError> {
    let content = compact_text(&req.content, 2_000);
    memory_engine::MemoryEngine::admit_text(&content).map_err(|error| {
        AppError::ValidationError(format!("memory content was rejected: {error}"))
    })?;
    let scope = normalize_scope(req.scope.as_deref());
    let app = normalize_app(req.app.as_deref());
    let memory_type = normalize_memory_type(req.memory_type.as_deref());
    let source_type = normalize_source_type(req.source_type.as_deref());
    let id = uuid::Uuid::new_v4().to_string();
    let metadata_json = req
        .metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let session_id = req.session_id.clone();
    let session_key = session_id.clone().unwrap_or_default();
    let content_hash = content_hash(&content);
    let confidence = req.confidence.unwrap_or(1.0).clamp(0.0, 1.0);
    let pinned = if req.pinned.unwrap_or(false) { 1 } else { 0 };
    let enabled = if req.enabled.unwrap_or(true) { 1 } else { 0 };
    // Embedding is an external/local-model read and must happen before the
    // short SQLite write transaction. All durable rows below then commit as a
    // single unit so a failed structured projection cannot leave a searchable
    // row without its authoritative fact.
    let embedding = embed_memory_text_best_effort(db, tenant_id, &app, &content).await;
    let mut tx = db.begin().await?;
    crate::acquire_sqlite_write_lock(&mut tx).await?;
    let existing_id = sqlx::query_scalar::<_, String>(
        r#"
        SELECT id
        FROM agent_memory_items
        WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ?
          AND session_key = ? AND memory_type = ? AND content_hash = ?
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&session_key)
    .bind(&memory_type)
    .bind(&content_hash)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(existing_id) = existing_id {
        sqlx::query(
            r#"
            UPDATE agent_memory_items
            SET updated_at = CURRENT_TIMESTAMP,
                confidence = MAX(confidence, ?),
                pinned = MAX(pinned, ?),
                enabled = ?,
                source_type = CASE
                    WHEN source_type = 'manual' OR ? = 'manual' THEN 'manual'
                    ELSE source_type
                END,
                stale_at = COALESCE(?, stale_at),
                verified_at = COALESCE(?, verified_at),
                metadata_json = COALESCE(json(?), metadata_json)
            WHERE tenant_id = ? AND user_id = ? AND id = ?
            "#,
        )
        .bind(confidence)
        .bind(pinned)
        .bind(enabled)
        .bind(&source_type)
        .bind(&req.stale_at)
        .bind(&req.verified_at)
        .bind(&metadata_json)
        .bind(tenant_id)
        .bind(user_id)
        .bind(&existing_id)
        .execute(&mut *tx)
        .await?;
        persist_structured_memory_projection_in_transaction(
            &mut tx,
            tenant_id,
            user_id,
            &existing_id,
            &scope,
            &app,
            session_id.as_deref(),
            &memory_type,
            &content,
            &source_type,
            confidence,
            pinned != 0,
            metadata_json.as_deref(),
        )
        .await?;
        tx.commit().await?;
        return get_unified_memory_item(db, tenant_id, user_id, &existing_id)
            .await?
            .ok_or_else(|| {
                AppError::Internal("updated memory item could not be reloaded".to_string())
            });
    }
    sqlx::query(
        r#"
        INSERT INTO agent_memory_items
          (id, tenant_id, user_id, scope, app, session_id, session_key, memory_type, content, content_hash,
           source_type, confidence, pinned, enabled, stale_at, verified_at, metadata_json,
           embedding_model, embedding_dimensions, embedding_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?), ?, ?, ?)
        ON CONFLICT DO UPDATE SET
          updated_at = CURRENT_TIMESTAMP,
          confidence = MAX(confidence, excluded.confidence),
          pinned = MAX(pinned, excluded.pinned),
          enabled = excluded.enabled,
          source_type = CASE
              WHEN source_type = 'manual' OR excluded.source_type = 'manual' THEN 'manual'
              ELSE source_type
          END,
          metadata_json = COALESCE(excluded.metadata_json, metadata_json),
          embedding_model = COALESCE(excluded.embedding_model, embedding_model),
          embedding_dimensions = COALESCE(excluded.embedding_dimensions, embedding_dimensions),
          embedding_json = COALESCE(excluded.embedding_json, embedding_json)
        "#,
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&session_id)
    .bind(&session_key)
    .bind(&memory_type)
    .bind(&content)
    .bind(&content_hash)
    .bind(&source_type)
    .bind(confidence)
    .bind(pinned)
    .bind(enabled)
    .bind(req.stale_at)
    .bind(req.verified_at)
    .bind(metadata_json.as_deref())
    .bind(embedding.as_ref().map(|item| item.model.as_str()))
    .bind(
        embedding
            .as_ref()
            .map(|item| i32::try_from(item.vector.len()).unwrap_or(i32::MAX)),
    )
    .bind(embedding.as_ref().and_then(|item| serialize_memory_vector(&item.vector)))
    .execute(&mut *tx)
    .await?;
    persist_structured_memory_projection_in_transaction(
        &mut tx,
        tenant_id,
        user_id,
        &id,
        &scope,
        &app,
        session_id.as_deref(),
        &memory_type,
        &content,
        &source_type,
        confidence,
        pinned != 0,
        metadata_json.as_deref(),
    )
    .await?;
    tx.commit().await?;
    let item = sqlx::query(
        r#"
        SELECT id, tenant_id, user_id, scope, app, session_id, memory_type, content,
               source_type, CAST(confidence AS DOUBLE) AS confidence, pinned, enabled,
               CAST(stale_at AS TEXT) AS stale_at, CAST(verified_at AS TEXT) AS verified_at,
               CAST(metadata_json AS TEXT) AS metadata_json,
               embedding_model, embedding_dimensions, embedding_json,
               CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_memory_items
        WHERE tenant_id = ? AND user_id = ? AND scope = ? AND app = ?
          AND session_key = ? AND memory_type = ? AND content_hash = ?
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&scope)
    .bind(&app)
    .bind(&session_key)
    .bind(&memory_type)
    .bind(&content_hash)
    .fetch_one(db)
    .await?;
    Ok(row_to_memory_item(&item, None))
}

pub(crate) async fn get_unified_memory_item(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    id: &str,
) -> Result<Option<AgentMemoryItem>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT id, tenant_id, user_id, scope, app, session_id, memory_type, content,
               source_type, CAST(confidence AS DOUBLE) AS confidence, pinned, enabled,
               CAST(stale_at AS TEXT) AS stale_at, CAST(verified_at AS TEXT) AS verified_at,
               CAST(metadata_json AS TEXT) AS metadata_json,
               embedding_model, embedding_dimensions, embedding_json,
               CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_memory_items
        WHERE tenant_id = ? AND user_id = ? AND id = ?
          AND EXISTS (
              SELECT 1 FROM structured_memory_facts fact
              WHERE fact.projection_memory_id = agent_memory_items.id
                AND fact.tenant_id = agent_memory_items.tenant_id
                AND fact.user_id = agent_memory_items.user_id
                AND fact.current = 1
          )
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(|row| row_to_memory_item(row, None)))
}

pub(crate) async fn list_unified_memory_items(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    scope: Option<&str>,
    app: Option<&str>,
    session_id: Option<&str>,
    include_disabled: bool,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    // This is the dominant Super Assistant recall shape. A single query with
    // `session_id = ? OR session_id IS NULL` makes the planner prefer the recent-user
    // index, then fetch large embedding rows just to reject other sessions.
    // Rank the two disjoint, index-ordered id sets first and fetch payloads only
    // for the final bounded set. The union is equivalent to the original OR.
    if scope.is_none() && session_id.is_some() && !include_disabled {
        let session_id = session_id.expect("checked above");
        let db_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            r#"
            WITH session_candidates AS (
                SELECT id, pinned, updated_at
                FROM agent_memory_items
                WHERE tenant_id = ? AND user_id = ? AND enabled = 1 AND session_id = ?
                  AND (? IS NULL OR app = ? OR app = 'shared')
                  AND EXISTS (
                      SELECT 1 FROM structured_memory_facts fact
                      WHERE fact.projection_memory_id = agent_memory_items.id
                        AND fact.tenant_id = agent_memory_items.tenant_id
                        AND fact.user_id = agent_memory_items.user_id
                        AND fact.current = 1
                  )
                ORDER BY pinned DESC, updated_at DESC, id DESC
                LIMIT ?
            ),
            global_candidates AS (
                SELECT id, pinned, updated_at
                FROM agent_memory_items
                WHERE tenant_id = ? AND user_id = ? AND enabled = 1 AND session_id IS NULL
                  AND (? IS NULL OR app = ? OR app = 'shared')
                  AND EXISTS (
                      SELECT 1 FROM structured_memory_facts fact
                      WHERE fact.projection_memory_id = agent_memory_items.id
                        AND fact.tenant_id = agent_memory_items.tenant_id
                        AND fact.user_id = agent_memory_items.user_id
                        AND fact.current = 1
                  )
                ORDER BY pinned DESC, updated_at DESC, id DESC
                LIMIT ?
            ),
            ranked AS (
                SELECT id, pinned, updated_at FROM session_candidates
                UNION ALL
                SELECT id, pinned, updated_at FROM global_candidates
                ORDER BY pinned DESC, updated_at DESC, id DESC
                LIMIT ?
            )
            SELECT memory.id, memory.tenant_id, memory.user_id, memory.scope, memory.app,
                   memory.session_id, memory.memory_type, memory.content, memory.source_type,
                   CAST(memory.confidence AS DOUBLE) AS confidence, memory.pinned, memory.enabled,
                   CAST(memory.stale_at AS TEXT) AS stale_at,
                   CAST(memory.verified_at AS TEXT) AS verified_at,
                   CAST(memory.metadata_json AS TEXT) AS metadata_json,
                   memory.embedding_model, memory.embedding_dimensions, memory.embedding_json,
                   CAST(memory.created_at AS TEXT) AS created_at,
                   CAST(memory.updated_at AS TEXT) AS updated_at
            FROM ranked
            INNER JOIN agent_memory_items AS memory ON memory.id = ranked.id
            ORDER BY ranked.pinned DESC, ranked.updated_at DESC, ranked.id DESC
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(app)
        .bind(app)
        .bind(db_limit)
        .bind(tenant_id)
        .bind(user_id)
        .bind(app)
        .bind(app)
        .bind(db_limit)
        .bind(db_limit)
        .fetch_all(db)
        .await?;
        return Ok(rows
            .iter()
            .map(|row| row_to_memory_item(row, None))
            .collect());
    }

    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        r#"
        SELECT id, tenant_id, user_id, scope, app, session_id, memory_type, content,
               source_type, CAST(confidence AS DOUBLE) AS confidence, pinned, enabled,
               CAST(stale_at AS TEXT) AS stale_at, CAST(verified_at AS TEXT) AS verified_at,
               CAST(metadata_json AS TEXT) AS metadata_json,
               embedding_model, embedding_dimensions, embedding_json,
               CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
        FROM agent_memory_items
        WHERE tenant_id = "#,
    );
    query
        .push_bind(tenant_id)
        .push(" AND user_id = ")
        .push_bind(user_id);
    if !include_disabled {
        query.push(
            " AND EXISTS (SELECT 1 FROM structured_memory_facts fact WHERE fact.projection_memory_id = agent_memory_items.id AND fact.tenant_id = agent_memory_items.tenant_id AND fact.user_id = agent_memory_items.user_id AND fact.current = 1)",
        );
    }
    if let Some(scope) = scope {
        query.push(" AND scope = ").push_bind(scope);
    }
    if let Some(app) = app {
        query
            .push(" AND (app = ")
            .push_bind(app)
            .push(" OR app = 'shared')");
    }
    if let Some(session_id) = session_id {
        query
            .push(" AND (session_id = ")
            .push_bind(session_id)
            .push(" OR session_id IS NULL)");
    }
    if !include_disabled {
        query.push(" AND enabled = 1");
    }
    query
        .push(" ORDER BY pinned DESC, updated_at DESC LIMIT ")
        .push_bind(i64::try_from(limit).unwrap_or(i64::MAX));
    let rows = query.build().fetch_all(db).await?;
    Ok(rows
        .iter()
        .map(|row| row_to_memory_item(row, None))
        .collect())
}

pub(crate) async fn list_legacy_memory_items(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    app: Option<&str>,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let mut items = Vec::new();
    // Legacy chat memories are user-global (the table has no session column),
    // matching unified rows whose `session_id IS NULL`. They must remain
    // available when a current session filter is supplied; otherwise every
    // real Super_Assistant turn silently loses pre-migration preferences.
    if app.is_none_or(|value| value.eq_ignore_ascii_case("chat") || value == "shared") {
        let rows = sqlx::query(
            r#"
            SELECT id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
                   pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json,
                   CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
            FROM chat_memories
            WHERE tenant_id = ? AND user_id = ? AND enabled = 1
            ORDER BY pinned DESC, updated_at DESC
            LIMIT ?
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(limit as i64)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        items.extend(
            rows.iter()
                .map(|row| legacy_chat_row_to_item(row, tenant_id, user_id)),
        );
    }
    if app.is_none_or(|value| value.eq_ignore_ascii_case("pm") || value == "shared") {
        if let Some(session_id) = session_id {
            let rows = sqlx::query(
                r#"
                SELECT id, session_id, memory_type, content, source, CAST(confidence AS DOUBLE) AS confidence,
                       pinned, enabled, CAST(metadata_json AS TEXT) AS metadata_json,
                       CAST(created_at AS TEXT) AS created_at, CAST(updated_at AS TEXT) AS updated_at
                FROM pm_session_memories
                WHERE tenant_id = ? AND user_id = ? AND session_id = ? AND enabled = 1
                ORDER BY pinned DESC, updated_at DESC
                LIMIT ?
                "#,
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(session_id)
            .bind(limit as i64)
            .fetch_all(db)
            .await
            .unwrap_or_default();
            items.extend(
                rows.iter()
                    .map(|row| legacy_pm_row_to_item(row, tenant_id, user_id)),
            );
        }
    }
    Ok(items)
}

async fn list_context_archive_items(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, user_id, session_id, source, role, ordinal,
               content, content_kind, CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_context_archives
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
          AND content_kind NOT IN (
              'user_context',
              'route_decision',
              'zero_loss_measurement'
          )
          AND (
              source <> 'chat_adversarial'
              OR COALESCE(JSON_EXTRACT(metadata_json, '$.label'), '')
                 NOT IN ('trace', 'runtime_context')
          )
        ORDER BY created_at DESC, ordinal ASC
        LIMIT ?
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(limit as i64)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(context_archive_row_to_item)
        .filter(|item| !memory_is_sensitive(&item.content))
        .collect())
}

fn archive_search_terms(tokens: &[String]) -> Vec<String> {
    const GENERIC_HISTORY_CUES: &[&str] = &[
        "previous",
        "earlier",
        "before",
        "history",
        "recall",
        "remember",
        "first",
        "之前",
        "刚才",
        "前面",
        "上一",
        "第一次",
        "最开始",
        "历史",
        "记得",
        "问题",
        "消息",
        "对话",
        "还记",
        "第一",
        "一次",
    ];
    let mut terms = tokens
        .iter()
        .filter(|token| {
            !GENERIC_HISTORY_CUES
                .iter()
                .any(|cue| token == cue || token.contains(cue))
        })
        .cloned()
        .collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });
    terms.dedup();
    terms.truncate(12);
    terms
}

pub(crate) fn query_requests_earliest_history(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "first message",
        "first conversation",
        "earliest",
        "最早",
        "最开始",
        "第一次",
        "第一个问题",
    ]
    .iter()
    .any(|cue| lower.contains(cue) || query.contains(cue))
}

fn escape_like_pattern(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

async fn search_context_archive_items(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    tokens: &[String],
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let terms = archive_search_terms(tokens);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        r#"
        SELECT id, tenant_id, user_id, session_id, source, role, ordinal,
               content, content_kind, CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_context_archives
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
          AND content_kind NOT IN ('user_context', 'route_decision', 'zero_loss_measurement')
          AND (
              source <> 'chat_adversarial'
              OR COALESCE(JSON_EXTRACT(metadata_json, '$.label'), '')
                 NOT IN ('trace', 'runtime_context')
          )
          AND (
        "#,
    );
    sql.push_str(&vec!["LOWER(content) LIKE ? ESCAPE '\\\\'"; terms.len()].join(" OR "));
    sql.push_str(") ORDER BY created_at DESC, ordinal ASC LIMIT ?");
    let mut query = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id);
    for term in &terms {
        query = query.bind(format!("%{}%", escape_like_pattern(term)));
    }
    let rows = query
        .bind(i64::try_from(limit).unwrap_or(120))
        .fetch_all(db)
        .await?;
    Ok(rows
        .iter()
        .map(context_archive_row_to_item)
        .filter(|item| !memory_is_sensitive(&item.content))
        .collect())
}

async fn list_earliest_context_archive_items(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, user_id, session_id, source, role, ordinal,
               content, content_kind, CAST(metadata_json AS TEXT) AS metadata_json,
               CAST(created_at AS TEXT) AS created_at
        FROM agent_context_archives
        WHERE tenant_id = ? AND user_id = ? AND session_id = ?
          AND content_kind NOT IN ('user_context', 'route_decision', 'zero_loss_measurement')
          AND (
              source <> 'chat_adversarial'
              OR COALESCE(JSON_EXTRACT(metadata_json, '$.label'), '')
                 NOT IN ('trace', 'runtime_context')
          )
        ORDER BY created_at ASC, ordinal ASC
        LIMIT ?
        "#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(session_id)
    .bind(i64::try_from(limit).unwrap_or(80))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(context_archive_row_to_item)
        .filter(|item| !memory_is_sensitive(&item.content))
        .collect())
}

/// Gather the candidate memory items that [`search_memory_internal`] scores:
/// unified memory rows, plus (when `include_legacy`) legacy memory rows and
/// durable context-archive rows. Internal `user_context` snapshots and
/// observability-only route/recall measurements are kept in the exact archive
/// for audit/replay, but deliberately excluded from retrieval: composed prompts
/// would recursively re-inject stale context, while trace rows would consume a
/// ranked-recall slot without contributing user facts.
///
/// Extracted so callers that need the full [`AgentMemoryItem`] bodies behind a
/// ranked search result (e.g. the Super_Assistant post-compaction injection
/// path) can resolve them by id from the *same* candidate set the search scores,
/// without re-implementing the gathering logic. `search_memory_internal` itself
/// delegates here so the two paths never drift.
pub(crate) async fn gather_memory_candidates(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    scope: Option<&str>,
    app: Option<&str>,
    session_id: Option<&str>,
    include_legacy: bool,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let mut items =
        list_unified_memory_items(db, tenant_id, user_id, scope, app, session_id, false, 300)
            .await?;
    if include_legacy {
        append_nonduplicate_legacy_memory_candidates(
            &mut items,
            list_legacy_memory_items(db, tenant_id, user_id, app, session_id, 200)
                .await
                .unwrap_or_default(),
        );
        items.extend(
            list_context_archive_items(db, tenant_id, user_id, session_id, 300)
                .await
                .unwrap_or_default(),
        );
    }
    Ok(items)
}

/// Expand the bounded recent-memory candidate set only for an explicit exact
/// history request. This keeps normal turns off the archive LONGTEXT scan while
/// allowing long sessions to recover old SQL, messages, and the first turn.
pub(crate) async fn gather_memory_candidates_for_query(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    scope: Option<&str>,
    app: Option<&str>,
    session_id: Option<&str>,
    include_legacy: bool,
    query: &str,
) -> Result<Vec<AgentMemoryItem>, AppError> {
    let mut items = gather_memory_candidates(
        db,
        tenant_id,
        user_id,
        scope,
        app,
        session_id,
        include_legacy,
    )
    .await?;
    if include_legacy
        && scope.is_none_or(|value| value.eq_ignore_ascii_case("session"))
        && query_requests_exact_history(query)
    {
        let tokens = tokenize(query);
        items.extend(
            search_context_archive_items(db, tenant_id, user_id, session_id, &tokens, 120)
                .await
                .unwrap_or_default(),
        );
        if query_requests_earliest_history(query) {
            items.extend(
                list_earliest_context_archive_items(db, tenant_id, user_id, session_id, 80)
                    .await
                    .unwrap_or_default(),
            );
        }
    }
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.id.clone()));
    Ok(items)
}

fn memory_candidate_dedupe_key(item: &AgentMemoryItem) -> (Option<String>, String) {
    let normalized = item
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (item.session_id.clone(), normalized)
}

fn append_nonduplicate_legacy_memory_candidates(
    durable: &mut Vec<AgentMemoryItem>,
    legacy: Vec<AgentMemoryItem>,
) {
    let mut durable_keys = durable
        .iter()
        .map(memory_candidate_dedupe_key)
        .collect::<HashSet<_>>();
    for item in legacy {
        if durable_keys.insert(memory_candidate_dedupe_key(&item)) {
            durable.push(item);
        }
    }
}

pub(crate) async fn search_memory_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    req: &MemorySearchRequest,
) -> Result<Vec<MemorySearchResult>, AppError> {
    let items = gather_memory_candidates_for_query(
        db,
        tenant_id,
        user_id,
        req.scope.as_deref(),
        req.app.as_deref(),
        req.session_id.as_deref(),
        req.include_legacy.unwrap_or(true),
        &req.query,
    )
    .await?;
    Ok(search_memory_candidates_internal(db, tenant_id, req, items).await)
}

fn memory_recall_embedding_timeout() -> Duration {
    let millis = std::env::var("AOS_MEMORY_RECALL_EMBEDDING_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(600);
    Duration::from_millis(millis)
}

/// Rank an already-authorized candidate set without querying it a second time.
/// Missing item embeddings deliberately fall back to lexical scoring. Doing
/// synchronous embedding backfills here previously added up to 16 sequential
/// model calls to an interactive request and could discard the entire recall
/// bundle when the outer latency budget expired.
pub(crate) async fn search_memory_candidates_internal(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    req: &MemorySearchRequest,
    items: Vec<AgentMemoryItem>,
) -> Vec<MemorySearchResult> {
    let limit = req
        .limit
        .unwrap_or(MEMORY_SEARCH_LIMIT_DEFAULT)
        .clamp(1, 50);
    if items.is_empty() {
        return Vec::new();
    }
    let tokens = tokenize(&req.query);
    let exact_history_query = query_requests_exact_history(&req.query);
    let query_embedding = embed_memory_text_for_recall_best_effort(
        db,
        tenant_id,
        req.app.as_deref().unwrap_or("shared"),
        &req.query,
    )
    .await;
    let pinned_only = req.pinned_only.unwrap_or(false);
    let mut scored = items
        .into_iter()
        .filter(|item| item.enabled)
        .filter(|item| !pinned_only || item.pinned)
        .filter(|item| !memory_is_sensitive(&item.content))
        .filter(|item| memory_condition_matches_message(&item.content, &req.query))
        .map(|item| {
            // Production retrieval shares the MemoryEngine lexical policy;
            // embeddings remain an optional second signal.
            let mut lexical_score =
                memory_engine::MemoryEngine::lexical_relevance_terms(&tokens, &item.content);
            if exact_history_query
                && lexical_score <= 0.0
                && item.legacy_source.as_deref() == Some("agent_context_archives")
            {
                lexical_score = 0.05;
            }
            let semantic_score = query_embedding
                .as_ref()
                .and_then(|query| semantic_memory_score(query, &item));
            let score = memory_hybrid_score(&item, lexical_score, semantic_score);
            (score, lexical_score, semantic_score, item)
        })
        .filter(|(_, lexical_score, semantic_score, item)| {
            item.pinned
                || *lexical_score > 0.0
                || semantic_score.is_some_and(|value| value >= MEMORY_SEMANTIC_MIN_RELEVANCE)
                || (exact_history_query
                    && item.legacy_source.as_deref() == Some("agent_context_archives"))
                || tokens.is_empty()
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(limit);
    scored
        .into_iter()
        .map(|(score, lexical_score, semantic_score, item)| {
            let (excerpt, line_start, line_end) = excerpt_for_tokens(&item.content, &tokens);
            MemorySearchResult {
                id: item.id,
                path: item.virtual_path,
                memory_type: item.memory_type,
                app: item.app,
                scope: item.scope,
                session_id: item.session_id,
                excerpt,
                line_start,
                line_end,
                score,
                retrieval_mode: match semantic_score {
                    Some(_) if lexical_score > 0.0 => "hybrid_semantic_keyword".to_string(),
                    Some(_) => "semantic".to_string(),
                    None => "keyword".to_string(),
                },
                lexical_score,
                semantic_score,
                pinned: item.pinned,
                source_type: item.source_type,
                legacy_source: item.legacy_source,
            }
        })
        .collect()
}

#[cfg(any(feature = "agent", test))]
#[cfg_attr(not(feature = "agent"), allow(dead_code))]
async fn baseline_memory_results(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    app: &str,
    session_id: Option<&str>,
    message: &str,
    pinned_only: bool,
) -> Result<Vec<MemorySearchResult>, AppError> {
    let mut items = list_unified_memory_items(
        db,
        tenant_id,
        user_id,
        None,
        Some(app),
        session_id,
        false,
        120,
    )
    .await?;
    items.extend(
        list_legacy_memory_items(db, tenant_id, user_id, Some(app), session_id, 80)
            .await
            .unwrap_or_default(),
    );
    let mut scored = items
        .into_iter()
        .filter(|item| item.enabled)
        .filter(|item| !memory_is_sensitive(&item.content))
        .filter(|item| memory_condition_matches_message(&item.content, message))
        .filter(|item| {
            if pinned_only {
                item.pinned
            } else {
                is_baseline_memory_candidate(item)
            }
        })
        .map(|item| {
            let score = baseline_memory_score(&item);
            (score, item)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(MEMORY_BASELINE_PROMPT_LIMIT);
    Ok(scored
        .into_iter()
        .map(|(score, item)| {
            let (excerpt, line_start, line_end) = excerpt_for_tokens(&item.content, &[]);
            MemorySearchResult {
                id: item.id,
                path: item.virtual_path,
                memory_type: item.memory_type,
                app: item.app,
                scope: item.scope,
                session_id: item.session_id,
                excerpt,
                line_start,
                line_end,
                score,
                retrieval_mode: if item.pinned {
                    "pinned_baseline".to_string()
                } else {
                    "baseline_preference".to_string()
                },
                lexical_score: 0.0,
                semantic_score: None,
                pinned: item.pinned,
                source_type: item.source_type,
                legacy_source: item.legacy_source,
            }
        })
        .collect())
}

#[cfg(any(feature = "agent", test))]
fn is_baseline_memory_candidate(item: &AgentMemoryItem) -> bool {
    item.pinned
        || item.memory_type.eq_ignore_ascii_case("preference")
        || looks_like_preference_memory(&item.content)
}

fn memory_condition_matches_message(content: &str, message: &str) -> bool {
    let Some(condition) = extract_memory_condition_phrase(content) else {
        return true;
    };
    if message.trim().is_empty() {
        return false;
    }
    message.contains(&condition)
        || message
            .to_ascii_lowercase()
            .contains(&condition.to_ascii_lowercase())
}

#[cfg(any(feature = "agent", test))]
fn memory_answer_language_contract(items: &[MemorySearchResult]) -> String {
    let Some(language) = items
        .iter()
        .filter(|item| {
            item.memory_type.eq_ignore_ascii_case("preference")
                || looks_like_preference_memory(&item.excerpt)
        })
        .find_map(|item| detect_memory_answer_language(&item.excerpt))
    else {
        return String::new();
    };
    format!(
        "\nAnswer language contract from memory: All user-visible narrative, greetings, conclusions, caveats, and next actions in this turn MUST be written entirely in {language}. Do not mix languages unless the user explicitly asks for translation, comparison, or quoted text in another language."
    )
}

#[cfg(any(feature = "agent", test))]
fn detect_memory_answer_language(content: &str) -> Option<&'static str> {
    let lower = content.to_ascii_lowercase();
    if content.contains("日文")
        || content.contains("日语")
        || content.contains("日語")
        || content.contains("日本語")
        || lower.contains("japanese")
        || lower.contains("nihongo")
    {
        return Some("Japanese");
    }
    if content.contains("中文") || content.contains("汉语") || content.contains("漢語") {
        return Some("Chinese");
    }
    if content.contains("英文") || content.contains("英语") || lower.contains("english") {
        return Some("English");
    }
    if content.contains("俄文")
        || content.contains("俄语")
        || content.contains("俄語")
        || content.contains("俄罗斯语")
        || content.contains("俄羅斯語")
        || lower.contains("russian")
        || lower.contains("русский")
    {
        return Some("Russian");
    }
    None
}

fn extract_memory_condition_phrase(content: &str) -> Option<String> {
    let text = content.trim();
    for (start, end) in [
        ("每当有", "时"),
        ("每当", "时"),
        ("当有", "时"),
        ("当", "时"),
        ("如果有", "时"),
        ("如果", "时"),
        ("包含", "时"),
        ("只要有", "就"),
        ("只要", "就"),
        ("包含", "就"),
        ("when there is", "then"),
        ("when", "then"),
        ("if", "then"),
        ("contains", "then"),
    ] {
        if let Some(value) = extract_between_markers(text, start, end) {
            return Some(value);
        }
    }
    None
}

fn extract_between_markers(text: &str, start: &str, end: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start_lower = start.to_ascii_lowercase();
    let end_lower = end.to_ascii_lowercase();
    let start_pos = lower.find(&start_lower)?;
    let body_start = start_pos + start.len();
    let tail = &text[body_start..];
    let tail_lower = &lower[body_start..];
    let end_pos = tail_lower.find(&end_lower)?;
    let candidate = tail[..end_pos]
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，' | '"' | '\'' | '“' | '”')
        })
        .trim();
    let candidate = normalize_memory_condition_candidate(candidate);
    if candidate.chars().count() >= 1 && candidate.chars().count() <= 40 {
        Some(candidate)
    } else {
        None
    }
}

fn normalize_memory_condition_candidate(candidate: &str) -> String {
    let mut value = candidate
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ':' | '：' | ',' | '，' | '"' | '\'' | '“' | '”' | '「' | '」' | '『' | '』'
                )
        })
        .trim()
        .to_string();
    for suffix in [
        "这两个字",
        "這兩個字",
        "这几个字",
        "這幾個字",
        "两个字",
        "兩個字",
        "几个字",
        "幾個字",
        "这句话",
        "這句話",
        "这句",
        "這句",
        "这个词",
        "這個詞",
        "这个词语",
        "這個詞語",
    ] {
        if value.ends_with(suffix) {
            value.truncate(value.len().saturating_sub(suffix.len()));
            value = value.trim().to_string();
            break;
        }
    }
    value
}

#[cfg(any(feature = "agent", test))]
fn looks_like_preference_memory(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "以后",
        "今后",
        "默认",
        "偏好",
        "口吻",
        "格式",
        "中文",
        "英文",
        "日文",
        "日语",
        "日語",
        "日本語",
        "俄文",
        "俄语",
        "俄語",
        "俄罗斯语",
        "俄羅斯語",
        "回复",
        "回答",
        "简洁",
        "详细",
        "from now on",
        "always",
        "by default",
        "prefer",
        "preference",
        "concise",
        "japanese",
        "russian",
        "language",
        "reply",
        "respond",
        "answer",
        "tone",
        "format",
    ]
    .iter()
    .any(|needle| lower.contains(needle) || content.contains(needle))
}

#[cfg(any(feature = "agent", test))]
fn baseline_memory_score(item: &AgentMemoryItem) -> f64 {
    if item.pinned {
        return 1.0;
    }
    let type_score = if item.memory_type.eq_ignore_ascii_case("preference") {
        0.78
    } else if looks_like_preference_memory(&item.content) {
        0.68
    } else {
        0.0
    };
    let confidence = item.confidence.clamp(0.0, 1.0) * 0.12;
    let recency = recency_score(&item.updated_at) * 0.10;
    (type_score + confidence + recency).clamp(0.0, 1.0)
}

async fn resolve_virtual_memory_path(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    path: &str,
) -> Result<Option<String>, AppError> {
    if path == "MEMORY.md" {
        let items =
            list_unified_memory_items(db, tenant_id, user_id, None, None, None, false, 200).await?;
        return Ok(Some(render_items_markdown("MEMORY.md", &items)));
    }
    if path == "memory_summary.md" {
        let rows = sqlx::query(
            r#"
            SELECT app, scope, session_id, summary, CAST(updated_at AS TEXT) AS updated_at
            FROM agent_memory_summaries
            WHERE tenant_id = ? AND user_id = ?
            ORDER BY updated_at DESC
            LIMIT 80
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_all(db)
        .await?;
        let mut lines = vec!["# Memory Summary".to_string()];
        for row in rows {
            lines.push(format!(
                "\n## {} / {} / {}\n{}\n_updated: {}_",
                row.get::<String, _>("app"),
                row.get::<String, _>("scope"),
                row.get::<Option<String>, _>("session_id")
                    .unwrap_or_else(|| "global".to_string()),
                row.get::<String, _>("summary"),
                row.get::<String, _>("updated_at")
            ));
        }
        return Ok(Some(lines.join("\n")));
    }
    if let Some(session_id) = path
        .strip_prefix("sessions/")
        .and_then(|value| value.strip_suffix(".md"))
    {
        let items = list_unified_memory_items(
            db,
            tenant_id,
            user_id,
            Some("session"),
            None,
            Some(session_id),
            false,
            200,
        )
        .await?;
        return Ok(Some(render_items_markdown(path, &items)));
    }
    if let Some(session_id) = path
        .strip_prefix("pm/")
        .and_then(|value| value.strip_suffix(".md"))
    {
        let mut items = list_unified_memory_items(
            db,
            tenant_id,
            user_id,
            None,
            Some("pm"),
            Some(session_id),
            false,
            200,
        )
        .await?;
        items.extend(
            list_legacy_memory_items(db, tenant_id, user_id, Some("pm"), Some(session_id), 200)
                .await
                .unwrap_or_default(),
        );
        return Ok(Some(render_items_markdown(path, &items)));
    }
    if let Some(archive_id) = path
        .strip_prefix("context_archives/")
        .and_then(|value| value.strip_suffix(".md"))
    {
        let row = sqlx::query(
            r#"
            SELECT role, content, content_kind, source, window_id, ordinal,
                   CAST(created_at AS TEXT) AS created_at
            FROM agent_context_archives
            WHERE tenant_id = ? AND user_id = ? AND id = ?
            LIMIT 1
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(archive_id)
        .fetch_optional(db)
        .await?;
        return Ok(row.and_then(|row| {
            let content: String = row.get("content");
            if memory_is_sensitive(&content) {
                return None;
            }
            Some(format!(
                "# Exact Context Archive\n\n- role: {}\n- kind: {}\n- source: {}\n- window: {}\n- ordinal: {}\n- created: {}\n\n{}",
                row.get::<String, _>("role"),
                row.get::<String, _>("content_kind"),
                row.get::<String, _>("source"),
                row.get::<String, _>("window_id"),
                row.get::<i32, _>("ordinal"),
                row.get::<String, _>("created_at"),
                content
            ))
        }));
    }
    if let Some(memory_id) = path.strip_prefix("ad_hoc/notes/") {
        let memory_id = memory_id.trim_end_matches(".md");
        let item = get_unified_memory_item(db, tenant_id, user_id, memory_id).await?;
        return Ok(item.map(|item| item.content));
    }
    Ok(None)
}

fn context_archive_row_to_item(row: &sqlx::sqlite::SqliteRow) -> AgentMemoryItem {
    let id: String = row.get("id");
    let session_id: String = row.get("session_id");
    let source: String = row.get("source");
    let content_kind: String = row.get("content_kind");
    let created_at: String = row.get("created_at");
    AgentMemoryItem {
        id: format!("context-archive-{id}"),
        tenant_id: row.get("tenant_id"),
        user_id: row.get("user_id"),
        scope: "session".to_string(),
        app: "shared".to_string(),
        session_id: Some(session_id),
        memory_type: content_kind,
        content: row.get("content"),
        source_type: source,
        confidence: 1.0,
        pinned: false,
        enabled: true,
        stale_at: None,
        verified_at: None,
        metadata: parse_json(row.get("metadata_json")),
        embedding_model: None,
        embedding_dimensions: None,
        embedding_vector: None,
        created_at: created_at.clone(),
        updated_at: created_at,
        virtual_path: format!("context_archives/{id}.md"),
        legacy_source: Some("agent_context_archives".to_string()),
    }
}

fn row_to_memory_item(
    row: &sqlx::sqlite::SqliteRow,
    legacy_source: Option<&str>,
) -> AgentMemoryItem {
    let id: String = row.get("id");
    let app: String = row.get("app");
    let scope: String = row.get("scope");
    let session_id: Option<String> = row.get("session_id");
    let memory_type: String = row.get("memory_type");
    let virtual_path = virtual_path_for(&id, &app, &scope, session_id.as_deref(), legacy_source);
    AgentMemoryItem {
        id,
        tenant_id: row.get("tenant_id"),
        user_id: row.get("user_id"),
        scope,
        app,
        session_id,
        memory_type,
        content: row.get("content"),
        source_type: row.get("source_type"),
        confidence: row.try_get("confidence").unwrap_or(1.0),
        pinned: row.get::<i8, _>("pinned") != 0,
        enabled: row.get::<i8, _>("enabled") != 0,
        stale_at: row.get("stale_at"),
        verified_at: row.get("verified_at"),
        metadata: parse_json(row.get("metadata_json")),
        embedding_model: row.try_get("embedding_model").ok(),
        embedding_dimensions: row.try_get("embedding_dimensions").ok(),
        embedding_vector: deserialize_memory_vector(row.try_get("embedding_json").ok()),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        virtual_path,
        legacy_source: legacy_source.map(ToOwned::to_owned),
    }
}

fn legacy_chat_row_to_item(
    row: &sqlx::sqlite::SqliteRow,
    tenant_id: &str,
    user_id: &str,
) -> AgentMemoryItem {
    let id: String = row.get("id");
    AgentMemoryItem {
        id: format!("legacy-chat-{id}"),
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        scope: "global".to_string(),
        app: "chat".to_string(),
        session_id: None,
        memory_type: row.get("memory_type"),
        content: row.get("content"),
        source_type: row.get("source"),
        confidence: row.try_get("confidence").unwrap_or(1.0),
        pinned: row.get::<i8, _>("pinned") != 0,
        enabled: row.get::<i8, _>("enabled") != 0,
        stale_at: None,
        verified_at: None,
        metadata: parse_json(row.get("metadata_json")),
        embedding_model: None,
        embedding_dimensions: None,
        embedding_vector: None,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        virtual_path: "MEMORY.md".to_string(),
        legacy_source: Some("chat_memories".to_string()),
    }
}

fn legacy_pm_row_to_item(
    row: &sqlx::sqlite::SqliteRow,
    tenant_id: &str,
    user_id: &str,
) -> AgentMemoryItem {
    let id: String = row.get("id");
    let session_id: String = row.get("session_id");
    AgentMemoryItem {
        id: format!("legacy-pm-{id}"),
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        scope: "session".to_string(),
        app: "pm".to_string(),
        session_id: Some(session_id.clone()),
        memory_type: row.get("memory_type"),
        content: row.get("content"),
        source_type: row.get("source"),
        confidence: row.try_get("confidence").unwrap_or(1.0),
        pinned: row.get::<i8, _>("pinned") != 0,
        enabled: row.get::<i8, _>("enabled") != 0,
        stale_at: None,
        verified_at: None,
        metadata: parse_json(row.get("metadata_json")),
        embedding_model: None,
        embedding_dimensions: None,
        embedding_vector: None,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        virtual_path: format!("pm/{session_id}.md"),
        legacy_source: Some("pm_session_memories".to_string()),
    }
}

fn normalize_scope(value: Option<&str>) -> String {
    match value.unwrap_or("global").trim() {
        "global" | "app" | "session" | "project" => value.unwrap_or("global").trim().to_string(),
        _ => "global".to_string(),
    }
}

fn normalize_app(value: Option<&str>) -> String {
    match value.unwrap_or("shared").trim() {
        // `nl2sql` is a first-class app bucket so the SQL capability can persist
        // and retrieve its session-scoped 取数口径/约束 (query semantics /
        // constraints) through the unified store without a parallel table
        // (task 20.2).
        "chat" | "pm" | "rd" | "nl2sql" | "shared" => value.unwrap_or("shared").trim().to_string(),
        _ => "shared".to_string(),
    }
}

fn normalize_memory_type(value: Option<&str>) -> String {
    match value.unwrap_or("note").trim() {
        "preference" | "project_fact" | "business_context" | "workflow" | "pitfall"
        | "decision" | "note" => value.unwrap_or("note").trim().to_string(),
        _ => "note".to_string(),
    }
}

fn normalize_source_type(value: Option<&str>) -> String {
    match value.unwrap_or("manual").trim() {
        "explicit_user" | "session_summary" | "compaction" | "manual" | "import" | "extracted"
        | "migration" => value.unwrap_or("manual").trim().to_string(),
        _ => "manual".to_string(),
    }
}

fn virtual_path_for(
    id: &str,
    app: &str,
    scope: &str,
    session_id: Option<&str>,
    legacy_source: Option<&str>,
) -> String {
    if legacy_source == Some("pm_session_memories") {
        return format!("pm/{}.md", session_id.unwrap_or("unknown"));
    }
    if scope == "session" {
        if app == "pm" {
            return format!("pm/{}.md", session_id.unwrap_or("unknown"));
        }
        return format!("sessions/{}.md", session_id.unwrap_or("unknown"));
    }
    if app == "shared" || app == "chat" {
        "MEMORY.md".to_string()
    } else {
        format!("ad_hoc/notes/{id}.md")
    }
}

fn parse_json(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn exact_context_archive_hash(role: &str, ordinal: usize, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(role.as_bytes());
    hasher.update(b":");
    hasher.update(ordinal.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(content.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn exact_archive_window_id(app: &str, turn_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(app.as_bytes());
    hasher.update(b":");
    hasher.update(turn_id.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    normalize_exact_archive_window_id(&format!("exact-{app}-{}", &hash[..16]))
}

fn normalize_exact_archive_app(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        "shared".to_string()
    } else {
        let cleaned = normalized
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .take(16)
            .collect::<String>();
        if cleaned.is_empty() {
            "shared".to_string()
        } else {
            cleaned
        }
    }
}

fn normalize_exact_archive_source(value: &str) -> String {
    let normalized = value.trim();
    let normalized = if normalized.is_empty() {
        "exact_turn"
    } else {
        normalized
    };
    normalized.chars().take(32).collect()
}

fn normalize_exact_archive_role(value: &str) -> String {
    match value.trim() {
        "user" | "assistant" | "tool" | "system" => value.trim().to_string(),
        _ => "system".to_string(),
    }
}

fn normalize_exact_archive_kind(value: &str) -> String {
    let normalized = value.trim();
    let normalized = if normalized.is_empty() {
        "exact_context"
    } else {
        normalized
    };
    normalized.chars().take(32).collect()
}

fn normalize_exact_archive_window_id(value: &str) -> String {
    let normalized = value.trim();
    if normalized.chars().count() <= 64 {
        return normalized.to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("exact-{}", &hash[..24])
}

fn render_exact_archive_content(title: Option<&str>, content: &str) -> String {
    let content = content.trim();
    match title.map(str::trim).filter(|value| !value.is_empty()) {
        Some(title) => format!("# {title}\n\n{content}"),
        None => content.to_string(),
    }
}

fn truncate_exact_archive_content(content: &str) -> String {
    if content.chars().count() <= EXACT_CONTEXT_ARCHIVE_MAX_CHARS {
        content.to_string()
    } else {
        content
            .chars()
            .take(EXACT_CONTEXT_ARCHIVE_MAX_CHARS)
            .collect::<String>()
    }
}

fn merge_exact_archive_metadata(base: &Value, extra: Value) -> Value {
    let mut object = base.as_object().cloned().unwrap_or_default();
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    Value::Object(object)
}

pub(crate) fn query_requests_exact_history(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "recall",
        "remember",
        "previous",
        "earlier",
        "last time",
        "first message",
        "first conversation",
        "之前",
        "前面",
        "刚才",
        "上次",
        "历史",
        "原文",
        "还记得",
        "发给你",
        "传过",
        "上传过",
        "第一次",
        "最开始",
        "第一个问题",
    ]
    .iter()
    .any(|needle| lower.contains(needle) || query.contains(needle))
}

struct MemoryEmbedding {
    model: String,
    vector: Vec<f32>,
}

#[cfg(feature = "nl2sql")]
async fn embed_memory_text_best_effort(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    app: &str,
    text: &str,
) -> Option<MemoryEmbedding> {
    embed_memory_text_with_timeout(db, tenant_id, app, text, None).await
}

#[cfg(feature = "nl2sql")]
async fn embed_memory_text_for_recall_best_effort(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    app: &str,
    text: &str,
) -> Option<MemoryEmbedding> {
    embed_memory_text_with_timeout(
        db,
        tenant_id,
        app,
        text,
        Some(memory_recall_embedding_timeout()),
    )
    .await
}

#[cfg(feature = "nl2sql")]
async fn embed_memory_text_with_timeout(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    app: &str,
    text: &str,
    request_timeout: Option<Duration>,
) -> Option<MemoryEmbedding> {
    if text.trim().is_empty() {
        return None;
    }
    let cfg = if let Some(cfg) = resolve_embedding_config(db, tenant_id, Some(app)).await {
        cfg
    } else if let Some(cfg) = resolve_embedding_config(db, tenant_id, Some("chat")).await {
        cfg
    } else {
        resolve_embedding_config(db, tenant_id, None).await?
    };
    let model_name = cfg.model.clone();
    let model = EmbeddingModel::new_with_dimensions(
        &model_name,
        cfg.base_url.clone(),
        Some(cfg.api_key),
        cfg.dimensions,
    );
    let inputs = [compact_text(text, 2_000)];
    let result = if let Some(timeout) = request_timeout {
        match tokio::time::timeout(timeout, model.embed_batch(&inputs)).await {
            Ok(result) => result,
            Err(_) => {
                tracing::debug!(
                    tenant_id,
                    app,
                    timeout_ms = timeout.as_millis(),
                    "memory recall embedding timed out; using keyword recall"
                );
                return None;
            }
        }
    } else {
        model.embed_batch(&inputs).await
    };
    match result {
        Ok(mut vectors) => vectors.pop().map(|vector| MemoryEmbedding {
            model: model_name,
            vector,
        }),
        Err(error) => {
            tracing::debug!(
                tenant_id,
                app,
                error = %error,
                "memory semantic embedding skipped; keyword memory retrieval remains available"
            );
            None
        }
    }
}

#[cfg(not(feature = "nl2sql"))]
async fn embed_memory_text_best_effort(
    _db: &sqlx::SqlitePool,
    _tenant_id: &str,
    _app: &str,
    _text: &str,
) -> Option<MemoryEmbedding> {
    None
}

#[cfg(not(feature = "nl2sql"))]
async fn embed_memory_text_for_recall_best_effort(
    _db: &sqlx::SqlitePool,
    _tenant_id: &str,
    _app: &str,
    _text: &str,
) -> Option<MemoryEmbedding> {
    None
}

fn serialize_memory_vector(vector: &[f32]) -> Option<String> {
    serde_json::to_string(vector).ok()
}

fn deserialize_memory_vector(raw: Option<String>) -> Option<Vec<f32>> {
    raw.and_then(|value| serde_json::from_str::<Vec<f32>>(&value).ok())
        .filter(|vector| !vector.is_empty())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (left, right) in a.iter().zip(b.iter()) {
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }
    if norm_a <= f64::EPSILON || norm_b <= f64::EPSILON {
        return None;
    }
    Some((dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0))
}

fn semantic_memory_score(query: &MemoryEmbedding, item: &AgentMemoryItem) -> Option<f64> {
    if item.embedding_model.as_deref() != Some(query.model.as_str()) {
        return None;
    }
    let vector = item.embedding_vector.as_deref()?;
    cosine_similarity(&query.vector, vector).map(|score| ((score + 1.0) / 2.0).clamp(0.0, 1.0))
}

fn memory_hybrid_score(
    item: &AgentMemoryItem,
    lexical_score: f64,
    semantic_score: Option<f64>,
) -> f64 {
    memory_engine::MemoryEngine::retrieval_score(memory_engine::RetrievalSignals {
        lexical: lexical_score,
        semantic: semantic_score,
        semantic_min_relevance: MEMORY_SEMANTIC_MIN_RELEVANCE,
        semantic_weight: MEMORY_SEMANTIC_WEIGHT,
        lexical_weight: MEMORY_LEXICAL_WEIGHT,
        confidence: item.confidence,
        confidence_weight: MEMORY_CONFIDENCE_WEIGHT,
        recency: recency_score(&item.updated_at),
        recency_weight: MEMORY_RECENCY_WEIGHT,
        pinned: item.pinned,
    })
}

fn recency_score(updated_at: &str) -> f64 {
    let Ok(updated_at) = chrono::NaiveDateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S")
    else {
        return 0.3;
    };
    let age_days = (chrono::Utc::now().naive_utc() - updated_at)
        .num_days()
        .max(0) as f64;
    if age_days <= 1.0 {
        1.0
    } else if age_days <= 30.0 {
        0.75
    } else if age_days <= 180.0 {
        0.45
    } else {
        0.2
    }
}

fn tokenize(query: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = None;
    let flush =
        |runs: &mut Vec<(String, bool)>, current: &mut String, is_cjk: &mut Option<bool>| {
            if !current.is_empty() {
                runs.push((std::mem::take(current), is_cjk.unwrap_or(false)));
            }
            *is_cjk = None;
        };
    for ch in query.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch);
        let is_word = is_cjk || ch.is_alphanumeric() || ch == '_';
        if !is_word {
            flush(&mut runs, &mut current, &mut current_is_cjk);
            continue;
        }
        if current_is_cjk.is_some_and(|kind| kind != is_cjk) {
            flush(&mut runs, &mut current, &mut current_is_cjk);
        }
        current_is_cjk = Some(is_cjk);
        current.push(ch);
    }
    flush(&mut runs, &mut current, &mut current_is_cjk);

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let push = |token: String, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        let token = token.trim().to_ascii_lowercase();
        if token.chars().count() >= 2 && seen.insert(token.clone()) && out.len() < 80 {
            out.push(token);
        }
    };
    for (run, _) in &runs {
        push(run.clone(), &mut out, &mut seen);
    }
    let mut ngrams = Vec::new();
    let mut ngram_seen = HashSet::new();
    for (run, is_cjk) in &runs {
        if !*is_cjk {
            continue;
        }
        let chars = run.chars().collect::<Vec<_>>();
        for width in [2_usize, 3] {
            for window in chars.windows(width) {
                let token = window.iter().collect::<String>().to_ascii_lowercase();
                if !seen.contains(&token) && ngram_seen.insert(token.clone()) {
                    ngrams.push(token);
                }
            }
        }
    }
    let remaining = 80usize.saturating_sub(out.len());
    if ngrams.len() <= remaining {
        for token in ngrams {
            push(token, &mut out, &mut seen);
        }
    } else if remaining == 1 {
        push(ngrams[0].clone(), &mut out, &mut seen);
    } else if remaining > 1 {
        for index in 0..remaining {
            let sampled = index.saturating_mul(ngrams.len() - 1) / (remaining - 1);
            push(ngrams[sampled].clone(), &mut out, &mut seen);
        }
    }
    out
}

fn excerpt_for_tokens(content: &str, tokens: &[String]) -> (String, usize, usize) {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return (String::new(), 1, 1);
    }
    let mut best_idx = 0usize;
    let mut best_score = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        let score = tokens
            .iter()
            .filter(|token| lower.contains(token.as_str()) || line.contains(token.as_str()))
            .count();
        if score > best_score {
            best_score = score;
            best_idx = idx;
        }
    }
    let start = best_idx.saturating_sub(1);
    let end = (best_idx + 2).min(lines.len());
    let excerpt = compact_text(&lines[start..end].join("\n"), 500);
    (excerpt, start + 1, end)
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        normalized.chars().take(max_chars).collect::<String>()
    }
}

fn truncate_with_flag(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        (text.to_string(), false)
    } else {
        (text.chars().take(max_chars).collect::<String>(), true)
    }
}

fn render_items_markdown(title: &str, items: &[AgentMemoryItem]) -> String {
    let mut lines = vec![format!("# {title}")];
    for item in items {
        lines.push(format!(
            "\n## {} / {} / {}\n{}\n",
            item.app, item.scope, item.memory_type, item.content
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn memory_test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect memory database");
        sqlx::query(
            r#"
            CREATE TABLE agent_memory_items (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                app TEXT NOT NULL,
                session_id TEXT,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                source_type TEXT NOT NULL,
                confidence REAL NOT NULL,
                pinned INTEGER NOT NULL,
                enabled INTEGER NOT NULL,
                stale_at TEXT,
                verified_at TEXT,
                metadata_json TEXT,
                embedding_model TEXT,
                embedding_dimensions INTEGER,
                embedding_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("create memory table");
        sqlx::query(
            "CREATE TABLE structured_memory_facts (
                id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, user_id TEXT NOT NULL,
                scope TEXT NOT NULL, app TEXT NOT NULL, session_id TEXT,
                channel TEXT NOT NULL, kind TEXT NOT NULL, subject_json TEXT NOT NULL,
                predicate TEXT NOT NULL, value_json TEXT NOT NULL, text TEXT NOT NULL,
                evidence_id TEXT NOT NULL, evidence_hash TEXT NOT NULL,
                observed_at TEXT NOT NULL, valid_until TEXT, confidence REAL NOT NULL,
                sensitivity TEXT NOT NULL, current INTEGER NOT NULL DEFAULT 1,
                superseded_by TEXT, conflict_group TEXT, projection_memory_id TEXT,
                candidate_json TEXT NOT NULL, created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create structured memory table");
        pool
    }

    async fn insert_memory_fixture(
        pool: &sqlx::SqlitePool,
        id: &str,
        session_id: Option<&str>,
        pinned: bool,
        updated_at: &str,
    ) {
        sqlx::query(
            r#"
            INSERT INTO agent_memory_items (
                id, tenant_id, user_id, scope, app, session_id, memory_type, content,
                source_type, confidence, pinned, enabled, created_at, updated_at
            ) VALUES (?, 'tenant', 'user', ?, 'chat', ?, 'note', ?, 'manual', 1.0, ?, 1, ?, ?)
            "#,
        )
        .bind(id)
        .bind(if session_id.is_some() {
            "session"
        } else {
            "global"
        })
        .bind(session_id)
        .bind(format!("memory {id}"))
        .bind(i64::from(pinned))
        .bind(updated_at)
        .bind(updated_at)
        .execute(pool)
        .await
        .expect("insert memory fixture");
        sqlx::query(
            r#"
            INSERT INTO structured_memory_facts (
                id, tenant_id, user_id, scope, app, session_id, channel, kind,
                subject_json, predicate, value_json, text, evidence_id, evidence_hash,
                observed_at, confidence, sensitivity, current, projection_memory_id,
                candidate_json, created_at, updated_at
            ) VALUES (?, 'tenant', 'user', ?, 'chat', ?, 'long_term_memory', 'note',
                      '{"memoryType":"note"}', 'note', ?, ?, ?, ?, ?, 1.0, 'internal',
                      1, ?, '{}', ?, ?)
            "#,
        )
        .bind(format!("fact-{id}"))
        .bind(if session_id.is_some() {
            "session"
        } else {
            "global"
        })
        .bind(session_id)
        .bind(serde_json::Value::String(format!("memory {id}")).to_string())
        .bind(format!("memory {id}"))
        .bind(format!("memory:{id}"))
        .bind(format!("hash:{id}"))
        .bind(updated_at)
        .bind(id)
        .bind(updated_at)
        .bind(updated_at)
        .execute(pool)
        .await
        .expect("insert structured memory fixture");
    }

    fn test_memory_item(content: &str) -> AgentMemoryItem {
        AgentMemoryItem {
            id: "mem-test".to_string(),
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            scope: "global".to_string(),
            app: "chat".to_string(),
            session_id: None,
            memory_type: "preference".to_string(),
            content: content.to_string(),
            source_type: "manual".to_string(),
            confidence: 0.9,
            pinned: false,
            enabled: true,
            stale_at: None,
            verified_at: None,
            metadata: None,
            embedding_model: Some("test-embedding".to_string()),
            embedding_dimensions: Some(3),
            embedding_vector: Some(vec![1.0, 0.0, 0.0]),
            created_at: "2026-06-25 00:00:00".to_string(),
            updated_at: chrono::Utc::now()
                .naive_utc()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            virtual_path: "MEMORY.md".to_string(),
            legacy_source: None,
        }
    }

    async fn insert_consolidation_fixture(
        pool: &sqlx::SqlitePool,
        id: &str,
        tenant_id: &str,
        user_id: &str,
    ) {
        sqlx::query(
            "INSERT INTO agent_memory_items
                (id, tenant_id, user_id, scope, app, session_id, session_key,
                 memory_type, content, content_hash, source_type, confidence,
                 pinned, enabled)
             VALUES (?, ?, ?, 'global', 'chat', NULL, '', 'note', ?, ?,
                     'manual', 1.0, 0, 1)",
        )
        .bind(id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(format!("memory {id}"))
        .bind(content_hash(id))
        .execute(pool)
        .await
        .expect("insert consolidation fixture");
        sqlx::query(
            r#"
            INSERT INTO structured_memory_facts (
                id, tenant_id, user_id, scope, app, session_id, channel, kind,
                subject_json, predicate, value_json, text, evidence_id, evidence_hash,
                observed_at, confidence, sensitivity, current, projection_memory_id,
                candidate_json, created_at, updated_at
            ) VALUES (?, ?, ?, 'global', 'chat', NULL, 'long_term_memory', 'note',
                      '{"memoryType":"note"}', 'note', ?, ?, ?, ?, CURRENT_TIMESTAMP,
                      1.0, 'internal', 1, ?, '{}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            "#,
        )
        .bind(format!("fact-{tenant_id}-{user_id}-{id}"))
        .bind(tenant_id)
        .bind(user_id)
        .bind(serde_json::Value::String(format!("memory {id}")).to_string())
        .bind(format!("memory {id}"))
        .bind(format!("memory:{id}"))
        .bind(format!("hash:{id}"))
        .bind(id)
        .execute(pool)
        .await
        .expect("insert consolidation structured fact");
    }

    fn consolidation_request(
        cursor: &str,
        relation: &str,
        from_memory_id: &str,
        to_memory_id: &str,
    ) -> MemoryConsolidateRequest {
        MemoryConsolidateRequest {
            app: Some("chat".into()),
            session_id: None,
            summary: Some("current consolidated facts".into()),
            turn_count: Some(2),
            metadata: None,
            cursor: Some(cursor.into()),
            relations: vec![MemoryConsolidationRelation {
                from_memory_id: from_memory_id.into(),
                to_memory_id: to_memory_id.into(),
                relation: relation.into(),
                reason: "new evidence".into(),
            }],
        }
    }

    #[tokio::test]
    async fn consolidation_is_transactional_idempotent_and_tenant_scoped() {
        let db = crate::test_sqlite_pool().await;
        insert_consolidation_fixture(&db, "old", "tenant", "user").await;
        insert_consolidation_fixture(&db, "new", "tenant", "user").await;
        insert_consolidation_fixture(&db, "foreign", "other", "user").await;

        let first = consolidate_memory_internal(
            &db,
            "tenant",
            "user",
            consolidation_request("cursor-1", "supersedes", "old", "new"),
        )
        .await
        .expect("first consolidation");
        assert_eq!(first["idempotent"], false);
        let old_state: (i64, Option<String>) =
            sqlx::query_as("SELECT enabled, stale_at FROM agent_memory_items WHERE id = 'old'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(old_state.0, 0);
        assert!(old_state.1.is_some());

        let retry = consolidate_memory_internal(
            &db,
            "tenant",
            "user",
            consolidation_request("cursor-1", "supersedes", "old", "new"),
        )
        .await
        .expect("idempotent retry");
        assert_eq!(retry["idempotent"], true);
        let (revision, relation_count): (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT revision FROM agent_memory_consolidation_cursors
                 WHERE tenant_id = 'tenant' AND user_id = 'user' AND app = 'chat'),
                (SELECT COUNT(*) FROM agent_memory_relations
                 WHERE tenant_id = 'tenant' AND user_id = 'user')",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!((revision, relation_count), (1, 1));

        let rejected = consolidate_memory_internal(
            &db,
            "tenant",
            "user",
            consolidation_request("cursor-2", "conflicts_with", "new", "foreign"),
        )
        .await;
        assert!(rejected.is_err());
        let cursor: String = sqlx::query_scalar(
            "SELECT cursor FROM agent_memory_consolidation_cursors
             WHERE tenant_id = 'tenant' AND user_id = 'user' AND app = 'chat'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(cursor, "cursor-1");
    }

    #[tokio::test]
    async fn deleting_memory_removes_graph_citations_and_session_summary() {
        let db = crate::test_sqlite_pool().await;
        insert_consolidation_fixture(&db, "memory-delete", "tenant", "user").await;
        insert_consolidation_fixture(&db, "memory-keep", "tenant", "user").await;
        sqlx::query(
            "UPDATE agent_memory_items SET session_id = 'session-delete', scope = 'session'
             WHERE tenant_id = 'tenant' AND user_id = 'user' AND id IN ('memory-delete', 'memory-keep')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_memory_relations
               (id, tenant_id, user_id, from_memory_id, to_memory_id, relation, reason, source_cursor)
             VALUES ('relation-delete', 'tenant', 'user', 'memory-delete', 'memory-keep',
                     'conflicts_with', 'test', 'cursor')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_memory_citations
               (id, tenant_id, user_id, session_id, memory_id, path, line_start, line_end, note)
             VALUES ('citation-delete', 'tenant', 'user', 'session-delete', 'memory-delete',
                     'MEMORY.md', 1, 1, 'test')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_memory_summaries
               (id, tenant_id, user_id, scope, app, session_id, session_key,
                summary, source_type, turn_count)
             VALUES ('summary-delete', 'tenant', 'user', 'session', 'chat',
                     'session-delete', 'session-delete', 'contains memory-delete', 'manual', 1)",
        )
        .execute(&db)
        .await
        .unwrap();
        assert!(
            delete_memory_item_internal(&db, "tenant", "user", "memory-delete")
                .await
                .unwrap()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_memory_relations
                 WHERE tenant_id = 'tenant' AND user_id = 'user'",
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_memory_citations
                 WHERE tenant_id = 'tenant' AND user_id = 'user'",
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_memory_summaries
                 WHERE tenant_id = 'tenant' AND user_id = 'user' AND session_id = 'session-delete'",
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM agent_memory_items
                 WHERE tenant_id = 'tenant' AND user_id = 'user' AND id = 'memory-keep'",
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn conflict_and_supersession_control_current_memory_retrieval() {
        let db = crate::test_sqlite_pool().await;
        insert_consolidation_fixture(&db, "left", "tenant", "user").await;
        insert_consolidation_fixture(&db, "right", "tenant", "user").await;
        consolidate_memory_internal(
            &db,
            "tenant",
            "user",
            consolidation_request("cursor-conflict", "conflicts_with", "left", "right"),
        )
        .await
        .expect("record conflict");
        let enabled: Vec<i64> = sqlx::query_scalar(
            "SELECT enabled FROM agent_memory_items
             WHERE id IN ('left', 'right') ORDER BY id",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(enabled, vec![1, 1]);

        insert_consolidation_fixture(&db, "old", "tenant", "user").await;
        insert_consolidation_fixture(&db, "new", "tenant", "user").await;
        consolidate_memory_internal(
            &db,
            "tenant",
            "user",
            consolidation_request("cursor-supersedes", "supersedes", "old", "new"),
        )
        .await
        .expect("record supersession");
        let results = search_memory_internal(
            &db,
            "tenant",
            "user",
            &MemorySearchRequest {
                query: "memory".into(),
                scope: Some("global".into()),
                app: Some("chat".into()),
                session_id: None,
                include_legacy: Some(false),
                pinned_only: None,
                limit: Some(20),
            },
        )
        .await
        .expect("search current memories");
        assert!(results.iter().any(|item| item.id == "new"));
        assert!(!results.iter().any(|item| item.id == "old"));
        let relation: String = sqlx::query_scalar(
            "SELECT relation FROM agent_memory_relations
             WHERE tenant_id = 'tenant' AND from_memory_id = 'old' AND to_memory_id = 'new'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            memory_engine::MemoryEngine::temporal_relation(&relation).unwrap(),
            memory_engine::TemporalRelation::Supersedes
        );
    }

    #[tokio::test]
    async fn unified_memory_list_combines_current_session_and_global_rows() {
        let pool = memory_test_pool().await;
        insert_memory_fixture(
            &pool,
            "session",
            Some("session-1"),
            false,
            "2026-08-10 02:00:00",
        )
        .await;
        insert_memory_fixture(&pool, "global", None, true, "2026-08-10 01:00:00").await;
        insert_memory_fixture(
            &pool,
            "other",
            Some("session-2"),
            true,
            "2026-08-10 03:00:00",
        )
        .await;

        let items = list_unified_memory_items(
            &pool,
            "tenant",
            "user",
            None,
            Some("chat"),
            Some("session-1"),
            false,
            10,
        )
        .await
        .expect("list unified memory");

        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["global", "session"]
        );
    }

    #[test]
    fn memory_list_cursor_round_trips_stable_sort_key() {
        let mut item = test_memory_item("manual memory");
        item.id = "memory-123".to_string();
        item.pinned = true;
        item.updated_at = "2026-07-21 08:09:10.123456".to_string();

        let encoded = encode_memory_list_cursor(&item);
        let decoded = decode_memory_list_cursor(&encoded).expect("valid cursor");

        assert_eq!(
            decoded,
            MemoryListCursor {
                pinned: true,
                updated_at: item.updated_at,
                id: item.id,
            }
        );
        assert!(decode_memory_list_cursor("not-a-cursor").is_err());
    }

    #[test]
    fn memory_list_source_groups_keep_manual_and_automatic_separate() {
        assert_eq!(
            MemorySourceGroup::parse(Some("manual")).unwrap(),
            Some(MemorySourceGroup::Manual)
        );
        assert_eq!(
            MemorySourceGroup::parse(Some("automatic")).unwrap(),
            Some(MemorySourceGroup::Automatic)
        );
        assert_eq!(MemorySourceGroup::parse(None).unwrap(), None);
        assert!(MemorySourceGroup::parse(Some("system")).is_err());
    }

    #[test]
    fn memory_list_order_is_pinned_then_updated_then_id_descending() {
        let mut older = test_memory_item("older");
        older.id = "a".to_string();
        older.updated_at = "2026-07-20 00:00:00".to_string();
        let mut newer = test_memory_item("newer");
        newer.id = "b".to_string();
        newer.updated_at = "2026-07-21 00:00:00".to_string();
        let mut pinned = older.clone();
        pinned.id = "c".to_string();
        pinned.pinned = true;

        let mut items = vec![older, newer, pinned];
        items.sort_by(memory_list_order);

        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
    }

    #[test]
    fn sensitive_memory_detection_blocks_common_secret_shapes() {
        assert!(memory_is_sensitive("api_key=sk-abc123"));
        assert!(memory_is_sensitive("Authorization: Bearer abc.def.ghi"));
        assert!(memory_is_sensitive("13800138000123"));
        assert!(!memory_is_sensitive("以后默认用中文简洁回答"));
    }

    #[test]
    fn exact_archive_redacts_credentials_without_destroying_business_sql() {
        let input = "apikey: sk-1234567890abcdef\nPASSWD = \"top-secret\"\nBearer opaque-live-token-123456\nDATABASE_URL=mysql://analyst:db-secret@example.internal/aos\nSELECT * FROM events WHERE app_token = 'LUCKY_GAME'";
        let (redacted, changed) = redact_exact_archive_secrets(input);
        assert!(changed);
        assert!(!redacted.contains("sk-1234567890abcdef"));
        assert!(!redacted.contains("top-secret"));
        assert!(!redacted.contains("opaque-live-token-123456"));
        assert!(!redacted.contains("db-secret"));
        assert!(redacted.contains("mysql://analyst:[REDACTED]@example.internal/aos"));
        assert!(redacted.contains("app_token = 'LUCKY_GAME'"));
        assert!(redacted.contains("[REDACTED"));
    }

    #[test]
    fn tokenizer_splits_mixed_language_and_long_chinese_queries() {
        let tokens = tokenize("还记得第一次ROI分析里的SQL吗");
        assert!(tokens.iter().any(|token| token == "roi"));
        assert!(tokens.iter().any(|token| token == "sql"));
        assert!(tokens.iter().any(|token| token == "第一"));
        assert!(tokens.iter().any(|token| token == "分析"));
    }

    #[test]
    fn exact_history_detection_does_not_treat_current_sql_as_old_context() {
        assert!(query_requests_exact_history("还记得我第一次发的 SQL 吗"));
        assert!(query_requests_earliest_history("第一个问题是什么"));
        assert!(!query_requests_exact_history(
            "请解释当前这段 SQL 是什么意思"
        ));
        assert!(!query_requests_exact_history("给我一个完整的数据分析"));
    }

    #[test]
    fn agent_memory_item_serializes_to_camel_case() {
        let item = test_memory_item("remember to use dark mode");
        let json = serde_json::to_string(&item).unwrap();
        // camelCase field names match the frontend `AgentMemoryItem` interface.
        assert!(json.contains("\"sessionId\":null"));
        assert!(json.contains("\"memoryType\":\"preference\""));
        assert!(json.contains("\"sourceType\":\"manual\""));
        assert!(json.contains("\"virtualPath\":\"MEMORY.md\""));
        assert!(json.contains("\"embeddingModel\":\"test-embedding\""));
        // `embedding_vector` is never serialized (server-internal only).
        assert!(!json.contains("embeddingVector"));
    }

    #[test]
    fn agent_memory_item_round_trips() {
        // `embedding_vector` is intentionally not serialized, so identity is
        // asserted on an item without a vector (Req 7.2 round-trip guarantee).
        let mut item = test_memory_item("prefers concise Chinese answers");
        item.embedding_vector = None;
        let json = serde_json::to_string(&item).unwrap();
        let parsed: AgentMemoryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(item, parsed);
        // serialize → parse → serialize is identity-preserving.
        assert_eq!(json, serde_json::to_string(&parsed).unwrap());
    }

    #[test]
    fn legacy_memory_does_not_duplicate_migrated_unified_candidate() {
        let mut unified = test_memory_item("ROI = revenue / cost");
        unified.id = "unified".to_string();
        unified.session_id = Some("session-1".to_string());

        let mut duplicate_legacy = test_memory_item("  roi   =  REVENUE / COST  ");
        duplicate_legacy.id = "legacy-duplicate".to_string();
        duplicate_legacy.session_id = Some("session-1".to_string());
        duplicate_legacy.legacy_source = Some("pm_session_memories".to_string());

        let mut other_session = duplicate_legacy.clone();
        other_session.id = "legacy-other-session".to_string();
        other_session.session_id = Some("session-2".to_string());

        let mut candidates = vec![unified];
        append_nonduplicate_legacy_memory_candidates(
            &mut candidates,
            vec![duplicate_legacy, other_session],
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, "unified");
        assert_eq!(candidates[1].id, "legacy-other-session");
    }

    #[test]
    fn memory_planner_skips_simple_one_off_requests() {
        assert!(!should_consult_memory("翻译成英文"));
        assert!(!should_consult_memory("what time is it?"));
        assert!(should_skip_memory_for_message("翻译成英文"));
        assert!(!should_skip_memory_for_message(
            "解释一下 LTV 和 ROI 的关系。"
        ));
        assert!(should_consult_memory(
            "以后默认用中文简洁回答，并记住这是我的偏好"
        ));
        assert!(should_consult_memory(
            "我们项目之前的业务口径是什么？请结合历史决策回答"
        ));
    }

    #[test]
    fn baseline_memory_candidates_keep_preferences_available_for_general_questions() {
        let preference = test_memory_item("以后默认用中文简洁回答");
        assert!(is_baseline_memory_candidate(&preference));
        assert!(baseline_memory_score(&preference) > 0.7);

        let mut manually_added_as_project_fact =
            test_memory_item("默认回答格式：先结论，再风险，再方案");
        manually_added_as_project_fact.memory_type = "project_fact".to_string();
        assert!(is_baseline_memory_candidate(
            &manually_added_as_project_fact
        ));

        let mut business_context = test_memory_item("我们的产品是印尼休闲游戏矩阵");
        business_context.memory_type = "business_context".to_string();
        assert!(!is_baseline_memory_candidate(&business_context));
    }

    #[test]
    fn conditional_memory_only_matches_when_trigger_phrase_is_present() {
        let content = "每当有你好时就用日文回答我";
        assert_eq!(
            extract_memory_condition_phrase(content),
            Some("你好".to_string())
        );
        assert!(memory_condition_matches_message(
            content,
            "你好，介绍一下你自己"
        ));
        assert!(!memory_condition_matches_message(content, "hello"));
        assert!(!memory_condition_matches_message(content, ""));
    }

    #[test]
    fn conditional_memory_supports_as_long_as_then_language_rule() {
        let content = "只要有你好两个字就说俄文";
        assert_eq!(
            extract_memory_condition_phrase(content),
            Some("你好".to_string())
        );
        assert!(memory_condition_matches_message(content, "你好啊,,,"));
        assert!(!memory_condition_matches_message(content, "hello"));

        let item = MemorySearchResult {
            id: "mem-russian".to_string(),
            path: "sessions/test.md".to_string(),
            memory_type: "preference".to_string(),
            app: "chat".to_string(),
            scope: "session".to_string(),
            session_id: Some("session".to_string()),
            excerpt: content.to_string(),
            line_start: 1,
            line_end: 1,
            score: 1.0,
            retrieval_mode: "keyword".to_string(),
            lexical_score: 1.0,
            semantic_score: None,
            pinned: false,
            source_type: "manual".to_string(),
            legacy_source: None,
        };
        let contract = memory_answer_language_contract(&[item]);
        assert!(contract.contains("entirely in Russian"));
        assert!(contract.contains("Do not mix languages"));
    }

    #[test]
    fn language_memory_generates_strict_single_language_contract() {
        let item = MemorySearchResult {
            id: "mem-lang".to_string(),
            path: "sessions/test.md".to_string(),
            memory_type: "preference".to_string(),
            app: "chat".to_string(),
            scope: "session".to_string(),
            session_id: Some("session".to_string()),
            excerpt: "每当有你好时就用日文回答我".to_string(),
            line_start: 1,
            line_end: 1,
            score: 1.0,
            retrieval_mode: "keyword".to_string(),
            lexical_score: 1.0,
            semantic_score: None,
            pinned: false,
            source_type: "manual".to_string(),
            legacy_source: None,
        };
        let contract = memory_answer_language_contract(&[item]);
        assert!(contract.contains("entirely in Japanese"));
        assert!(contract.contains("Do not mix languages"));
    }

    #[test]
    fn explicit_memory_candidate_classifies_preferences_and_business_context() {
        let preference = extract_explicit_memory_candidate("以后默认用中文简洁回答", "chat")
            .expect("preference memory");
        assert_eq!(preference.0, "preference");
        assert!(preference.1.contains("中文"));

        let conditional_language =
            extract_explicit_memory_candidate("每当有你好时就用日文回答我", "chat")
                .expect("conditional language preference memory");
        assert_eq!(conditional_language.0, "preference");
        assert!(conditional_language.1.contains("日文"));

        let business = extract_explicit_memory_candidate(
            "记住我们的产品是印尼休闲游戏矩阵，ROI 口径只算 UA+UG",
            "pm",
        )
        .expect("business memory");
        assert_eq!(business.0, "business_context");
        assert!(business.1.contains("ROI"));
    }

    #[test]
    fn virtual_paths_keep_session_and_global_memory_separate() {
        assert_eq!(
            virtual_path_for("m1", "chat", "global", None, None),
            "MEMORY.md"
        );
        assert_eq!(
            virtual_path_for("m2", "chat", "session", Some("s1"), None),
            "sessions/s1.md"
        );
        assert_eq!(
            virtual_path_for("m3", "pm", "session", Some("p1"), None),
            "pm/p1.md"
        );
    }

    #[test]
    fn memory_semantic_score_requires_matching_embedding_model() {
        let query = MemoryEmbedding {
            model: "test-embedding".to_string(),
            vector: vec![1.0, 0.0, 0.0],
        };
        let item = test_memory_item("以后默认用中文简洁回答");
        let score = semantic_memory_score(&query, &item).expect("semantic score");
        assert!(score > 0.99);

        let mismatched_query = MemoryEmbedding {
            model: "other-embedding".to_string(),
            vector: vec![1.0, 0.0, 0.0],
        };
        assert!(semantic_memory_score(&mismatched_query, &item).is_none());
    }

    #[test]
    fn memory_hybrid_score_uses_semantic_signal_without_losing_keyword_fallback() {
        let item = test_memory_item("以后默认用中文简洁回答");
        let lexical_only = memory_hybrid_score(&item, 0.4, None);
        let semantic = memory_hybrid_score(&item, 0.0, Some(1.0));
        let hybrid = memory_hybrid_score(&item, 0.4, Some(1.0));
        assert!(semantic > lexical_only);
        assert!(hybrid >= semantic);
        assert!(lexical_only > 0.0);
    }

    #[tokio::test]
    async fn production_memory_adapter_uses_kernel_for_admission_ranking_and_relations() {
        let db = crate::test_sqlite_pool().await;
        let stored = create_memory_item_internal(
            &db,
            "tenant",
            "user",
            MemoryUpsertRequest {
                scope: Some("session".into()),
                app: Some("chat".into()),
                session_id: Some("session".into()),
                memory_type: Some("business_context".into()),
                content: "ROI is calculated from net revenue after refunds".into(),
                source_type: Some("manual".into()),
                confidence: Some(0.9),
                pinned: Some(false),
                enabled: Some(true),
                stale_at: None,
                verified_at: None,
                metadata: None,
            },
        )
        .await
        .expect("production adapter should admit a governed memory");

        let rejected = create_memory_item_internal(
            &db,
            "tenant",
            "user",
            MemoryUpsertRequest {
                scope: Some("session".into()),
                app: Some("chat".into()),
                session_id: Some("session".into()),
                memory_type: Some("note".into()),
                content: "authorization: Bearer tenant-secret".into(),
                source_type: Some("manual".into()),
                confidence: None,
                pinned: None,
                enabled: None,
                stale_at: None,
                verified_at: None,
                metadata: None,
            },
        )
        .await;
        assert!(matches!(rejected, Err(AppError::ValidationError(_))));

        let results = search_memory_internal(
            &db,
            "tenant",
            "user",
            &MemorySearchRequest {
                query: "ROI net revenue refunds".into(),
                scope: Some("session".into()),
                app: Some("chat".into()),
                session_id: Some("session".into()),
                include_legacy: Some(false),
                pinned_only: Some(false),
                limit: Some(5),
            },
        )
        .await
        .expect("production search");
        assert_eq!(
            results.first().map(|item| item.id.as_str()),
            Some(stored.id.as_str())
        );
        assert!(results[0].lexical_score > 0.0);
        assert!(search_memory_internal(
            &db,
            "other-tenant",
            "user",
            &MemorySearchRequest {
                query: "ROI net revenue refunds".into(),
                scope: None,
                app: Some("chat".into()),
                session_id: Some("session".into()),
                include_legacy: Some(false),
                pinned_only: None,
                limit: Some(5),
            },
        )
        .await
        .unwrap()
        .is_empty());

        assert_eq!(
            memory_engine::MemoryEngine::temporal_relation("conflicts_with").unwrap(),
            memory_engine::TemporalRelation::ConflictsWith
        );
    }

    #[tokio::test]
    async fn structured_fact_failure_rolls_back_searchable_memory_projection() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query(
            "CREATE TRIGGER reject_structured_memory_insert
             BEFORE INSERT ON structured_memory_facts
             BEGIN
               SELECT RAISE(ABORT, 'forced structured memory failure');
             END",
        )
        .execute(&db)
        .await
        .expect("install failure injection trigger");

        let result = create_memory_item_internal(
            &db,
            "tenant",
            "user",
            MemoryUpsertRequest {
                content: "The approved reporting timezone is Asia/Shanghai".to_string(),
                scope: Some("global".to_string()),
                app: Some("chat".to_string()),
                memory_type: Some("constraint".to_string()),
                source_type: Some("manual".to_string()),
                session_id: None,
                confidence: Some(1.0),
                pinned: Some(false),
                enabled: Some(true),
                stale_at: None,
                verified_at: None,
                metadata: None,
            },
        )
        .await;
        assert!(result.is_err(), "structured write failure must fail closed");

        let projection_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_memory_items WHERE tenant_id = 'tenant' AND user_id = 'user'",
        )
        .fetch_one(&db)
        .await
        .expect("count searchable projections");
        let fact_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM structured_memory_facts WHERE tenant_id = 'tenant' AND user_id = 'user'",
        )
        .fetch_one(&db)
        .await
        .expect("count structured facts");
        assert_eq!(projection_count, 0);
        assert_eq!(fact_count, 0);
    }

    #[test]
    fn pinned_memory_still_wins_hybrid_scoring() {
        let mut item = test_memory_item("低相关记忆");
        item.pinned = true;
        assert_eq!(memory_hybrid_score(&item, 0.0, None), 1.0);
    }

    #[test]
    fn memory_citation_line_range_reads_structured_artifact_fields() {
        let (start, end) = memory_citation_line_range(&json!({
            "lineStart": 12,
            "lineEnd": 14,
        }));
        assert_eq!(start, Some(12));
        assert_eq!(end, Some(14));

        let (start, end) = memory_citation_line_range(&json!({
            "lineStart": "bad",
            "lineEnd": 999999999999_i64,
        }));
        assert_eq!(start, None);
        assert_eq!(end, None);
    }

    #[test]
    fn memory_tool_output_becomes_structured_citation_artifact() {
        let call = agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "memory_search".to_string(),
            source: "builtin".to_string(),
            source_name: String::new(),
            input: r#"{"query":"口径"}"#.to_string(),
            output: json!({
                "items": [{
                    "id": "mem-1",
                    "path": "pm/session-1.md",
                    "memoryType": "business_context",
                    "app": "pm",
                    "scope": "session",
                    "sessionId": "session-1",
                    "lineStart": 7,
                    "lineEnd": 9,
                    "sourceType": "explicit_user"
                }]
            })
            .to_string(),
            is_error: false,
            duration_ms: 12,
        };

        let artifact = memory_artifact_from_tool_call(&call).expect("citation artifact");
        assert_eq!(artifact["mode"], "runtime_tool");
        assert_eq!(artifact["items"][0]["id"], "mem-1");
        assert_eq!(artifact["items"][0]["path"], "pm/session-1.md");
        assert_eq!(artifact["items"][0]["lineStart"], 7);
    }
}
