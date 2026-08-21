//! Chat API — streaming LLM chat with per-user session isolation.
//!
//! All endpoints require JWT Bearer authentication. Token usage is written
//! directly to the local platform database so every request is attributed to the
//! authenticated user and their tenant.
//!
//! Session layout: `$data_dir/.aos/{tenant_id}/{user_id}/{session_id}.jsonl`

use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    routing::{delete as routing_delete, get as routing_get, post as routing_post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::auth::Claims;
use crate::error::AppError;
use crate::state::AppState;

const MAX_TOKEN_USAGE_REQUEST_ID_CHARS: usize = 255;

// ---------------------------------------------------------------------------
// Message content block — mirrors the frontend ContentBlock type.
// Serialized as a JSON array in ChatMessage.content.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        #[serde(rename = "sourceType")]
        source_type: String,
        data: String,
    },
    Document {
        media_type: String,
        #[serde(rename = "sourceType")]
        source_type: String,
        data: String,
        name: Option<String>,
    },
}

impl ContentBlock {
    fn to_api_block(&self) -> api::InputContentBlock {
        use api::{ImageSourceType, InputContentBlock};
        match self {
            Self::Text { text } => InputContentBlock::Text { text: text.clone() },
            Self::Image {
                media_type,
                source_type,
                data,
            } => InputContentBlock::Image {
                media_type: media_type.clone(),
                source_type: match source_type.as_str() {
                    "url" => ImageSourceType::Url,
                    _ => ImageSourceType::Base64,
                },
                data: data.clone(),
            },
            Self::Document {
                media_type,
                source_type,
                data,
                name,
            } => InputContentBlock::Document {
                media_type: media_type.clone(),
                source_type: match source_type.as_str() {
                    "url" => ImageSourceType::Url,
                    _ => ImageSourceType::Base64,
                },
                data: data.clone(),
                name: name.clone(),
            },
        }
    }

    /// Returns a plain-text summary of the block for session record display.
    #[expect(dead_code)]
    fn text_summary(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::Image { .. } => "[图片]".to_string(),
            Self::Document { name, .. } => {
                format!("[文档: {}]", name.as_deref().unwrap_or("文件"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session storage
// ---------------------------------------------------------------------------

pub(super) fn sessions_dir(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
) -> std::path::PathBuf {
    data_dir.join(".aos").join(tenant_id).join(user_id)
}

pub(super) fn session_path(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> std::path::PathBuf {
    sessions_dir(data_dir, tenant_id, user_id).join(format!("{session_id}.jsonl"))
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct SessionMessageRecord {
    pub role: String,
    /// Flexible content: either a plain string (backward compat) or a list of
    /// content blocks that may include images and documents.
    #[serde()]
    pub content: serde_json::Value,
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: u64,
}

impl SessionMessageRecord {
    /// Serialize from a `ChatMessage` that has been updated to use `ContentBlock`[].
    pub(crate) fn from_message(role: &str, content: &serde_json::Value, timestamp_ms: u64) -> Self {
        Self {
            role: role.to_string(),
            content: content.clone(),
            timestamp_ms,
        }
    }
}

pub(super) fn load_session_messages(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Result<Vec<ChatMessage>, AppError> {
    let path = session_path(data_dir, tenant_id, user_id, session_id);
    tracing::debug!(path = %path.display(), exists = path.exists(), "load_session_messages");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path).map_err(AppError::Io)?;
    let mut messages = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<SessionMessageRecord>(line) {
            // Support both legacy plain-string content and new ContentBlock[] format.
            let content_json = &record.content;
            let content_str = if content_json.is_string() {
                content_json.as_str().unwrap_or_default().to_string()
            } else {
                // New block format: extract text summary for ChatMessage display.
                // Frontend will receive raw JSON and render properly.
                content_json.to_string()
            };
            messages.push(ChatMessage {
                role: record.role,
                content: serde_json::Value::String(content_str),
            });
        }
    }
    Ok(messages)
}

#[expect(dead_code)]
async fn get_user_tenant_id(user_id: &str, db: &sqlx::SqlitePool) -> String {
    const DEFAULT_TENANT_ID: &str = "00000000-0000-0000-0000-000000000001";

    // Try to read tenant_id from users table first.
    if let Ok(Some(tenant_id)) = sqlx::query_scalar::<_, String>(
        "SELECT tenant_id FROM users WHERE id = ? AND tenant_id IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    {
        return tenant_id;
    }

    // Fallback: ensure a default tenant exists and return its id.
    sqlx::query(
        "INSERT OR IGNORE INTO tenants (id, name, slug) VALUES (?, 'Default Tenant', 'default')",
    )
    .bind(DEFAULT_TENANT_ID)
    .bind(DEFAULT_TENANT_ID)
    .execute(db)
    .await
    .ok();

    DEFAULT_TENANT_ID.to_string()
}

pub(crate) fn append_message(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    record: &SessionMessageRecord,
) -> Result<(), AppError> {
    let dir = sessions_dir(data_dir, tenant_id, user_id);
    std::fs::create_dir_all(&dir).map_err(AppError::Io)?;
    let path = session_path(data_dir, tenant_id, user_id, session_id);
    let line = serde_json::to_string(record).map_err(AppError::Json)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(AppError::Io)?;
    writeln!(file, "{line}").map_err(AppError::Io)?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct SessionInfo {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageCount")]
    pub message_count: usize,
    #[serde(rename = "lastUpdated")]
    pub last_updated: u64,
}

#[expect(dead_code, reason = "kept only for compatibility-export diagnostics")]
fn list_user_sessions(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<SessionInfo>, AppError> {
    let dir = sessions_dir(data_dir, tenant_id, user_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(AppError::Io)? {
        let entry = entry.map_err(AppError::Io)?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let content = std::fs::read_to_string(&path).map_err(AppError::Io)?;
            let count = content.lines().filter(|l| !l.trim().is_empty()).count();
            let last_updated = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
            sessions.push(SessionInfo {
                session_id,
                message_count: count,
                last_updated,
            });
        }
    }
    sessions.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));
    Ok(sessions)
}

fn delete_session_file(
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(), AppError> {
    let path = session_path(data_dir, tenant_id, user_id, session_id);
    if path.exists() {
        std::fs::remove_file(&path).map_err(AppError::Io)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub session_id: Option<String>,
    /// Stable client request id. Reusing it with different content is rejected
    /// by the canonical ledger; omitting it preserves backward compatibility
    /// but cannot deduplicate a retry made as a new HTTP request.
    pub request_id: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    /// Content is stored as `serde_json::Value` to support both:
    /// - Plain string (backward compat, legacy sessions)
    /// - Array of `ContentBlock` (new format with images/docs)
    pub content: serde_json::Value,
}

impl ChatMessage {
    fn to_api_message(&self) -> api::InputMessage {
        use api::InputContentBlock;
        let blocks = if self.content.is_string() {
            vec![InputContentBlock::Text {
                text: self.content.as_str().unwrap_or("").to_string(),
            }]
        } else if let Some(arr) = self.content.as_array() {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<ContentBlock>(v.clone()).ok())
                .map(|b| b.to_api_block())
                .collect()
        } else {
            // Fallback: treat as plain text
            vec![InputContentBlock::Text {
                text: self.content.to_string(),
            }]
        };
        api::InputMessage {
            role: self.role.clone(),
            content: blocks,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub session_id: String,
    pub message: ChatMessage,
    pub usage: Option<TokenUsageDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageDto {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub model: String,
}

// ---------------------------------------------------------------------------
// Token usage writer - direct SQLite insert (bypasses telemetry.jsonl)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TokenUsageWriter {
    pool: SqlitePool,
}

impl TokenUsageWriter {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn write(&self, record: &TokenUsageRecord) -> Result<(), AppError> {
        let id = uuid::Uuid::new_v4().to_string();
        let request_id = normalize_token_usage_request_id(record.request_id.as_deref());
        sqlx::query(
            "
            INSERT INTO token_usage
                (id, tenant_id, user_id, session_id, request_id, model,
                 input_tokens, output_tokens, cache_creation_tokens,
                 cache_read_tokens, total_tokens, estimated_cost_usd,
                 api_key_id, provider, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(&id)
        .bind(&record.tenant_id)
        .bind(&record.user_id)
        .bind(&record.session_id)
        .bind(&request_id)
        .bind(&record.model)
        .bind(i64::from(record.input_tokens))
        .bind(i64::from(record.output_tokens))
        .bind(i64::from(record.cache_creation_tokens))
        .bind(i64::from(record.cache_read_tokens))
        .bind(i64::from(record.total_tokens))
        .bind(record.estimated_cost_usd)
        .bind(&record.api_key_id)
        .bind(&record.provider)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert token usage");
            AppError::Internal(format!("failed to record token usage: {e}"))
        })?;
        Ok(())
    }
}

fn normalize_token_usage_request_id(value: Option<&str>) -> Option<String> {
    let raw = value.map(str::trim).filter(|value| !value.is_empty())?;
    if raw.chars().count() <= MAX_TOKEN_USAGE_REQUEST_ID_CHARS {
        return Some(raw.to_string());
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut hasher);
    let suffix = format!("{:016x}", hasher.finish());
    let head_chars = MAX_TOKEN_USAGE_REQUEST_ID_CHARS
        .saturating_sub(suffix.len())
        .saturating_sub(1);
    let mut normalized = raw.chars().take(head_chars).collect::<String>();
    normalized.push('#');
    normalized.push_str(&suffix);
    tracing::warn!(
        original_chars = raw.chars().count(),
        normalized_chars = normalized.chars().count(),
        "token usage request_id exceeded storage budget; stored compact form"
    );
    Some(normalized)
}

#[derive(Debug, Clone)]
pub struct TokenUsageRecord {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub request_id: Option<String>,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_read_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// AppState extension — embed TokenUsageWriter
// ---------------------------------------------------------------------------

impl AppState {
    #[must_use]
    pub fn with_usage_writer(self, writer: TokenUsageWriter) -> Self {
        Self {
            usage_writer: Some(Arc::new(writer)),
            ..self
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn to_api_messages(chat_messages: &[ChatMessage]) -> Vec<api::InputMessage> {
    chat_messages
        .iter()
        .map(ChatMessage::to_api_message)
        .collect()
}

fn chat_message_from_api(message: api::InputMessage) -> ChatMessage {
    let content = if let [api::InputContentBlock::Text { text }] = message.content.as_slice() {
        serde_json::Value::String(text.clone())
    } else {
        serde_json::to_value(&message.content).unwrap_or(serde_json::Value::Array(Vec::new()))
    };
    ChatMessage {
        role: message.role,
        content,
    }
}

async fn list_canonical_sessions(
    db: &SqlitePool,
    tenant_id: &str,
    user_id: &str,
) -> Result<Vec<SessionInfo>, AppError> {
    let rows = sqlx::query(
        "SELECT t.id,
                SUM(CASE WHEN e.event_type IN ('runtime.chat_input', 'runtime.assistant_message')
                         THEN 1 ELSE 0 END) AS message_count,
                COALESCE(MAX(e.occurred_at), t.updated_at) AS last_updated
         FROM agent_threads t
         LEFT JOIN agent_event_ledger e
           ON e.tenant_id = t.tenant_id AND e.thread_id = t.id
         WHERE t.tenant_id = ? AND t.owner_user_id = ? AND t.status <> 'deleted'
           AND EXISTS (
               SELECT 1 FROM agent_event_ledger marker
               WHERE marker.tenant_id = t.tenant_id AND marker.thread_id = t.id
                 AND marker.event_type = 'runtime.chat_input'
           )
         GROUP BY t.id, t.updated_at
         ORDER BY last_updated DESC",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(db)
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    rows.into_iter()
        .map(|row| {
            let last_updated = row
                .try_get::<String, _>("last_updated")
                .ok()
                .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                .and_then(|value| u64::try_from(value.timestamp_millis()).ok())
                .unwrap_or_default();
            Ok(SessionInfo {
                session_id: row
                    .try_get("id")
                    .map_err(|error| AppError::Internal(error.to_string()))?,
                message_count: usize::try_from(
                    row.try_get::<i64, _>("message_count").unwrap_or_default(),
                )
                .unwrap_or_default(),
                last_updated,
            })
        })
        .collect()
}

async fn import_legacy_chat_if_needed(
    kernel: &crate::semantic_kernel_store::RuntimeExecutionKernel,
    data_dir: &std::path::Path,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(), AppError> {
    if !kernel
        .load_chat_messages()
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?
        .is_empty()
    {
        return Ok(());
    }
    let legacy = load_session_messages(data_dir, tenant_id, user_id, session_id)?;
    if legacy.is_empty() {
        return Ok(());
    }
    kernel
        .import_legacy_chat_messages(&to_api_messages(&legacy))
        .await
        .map_err(|error| AppError::Internal(format!("legacy chat import failed: {error}")))
}

#[allow(clippy::cast_possible_truncation)]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn estimate_cost(
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
    model: &str,
) -> f64 {
    use runtime::{ModelPricing, TokenUsage};
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
    };
    let pricing =
        runtime::pricing_for_model(model).unwrap_or_else(ModelPricing::default_sonnet_tier);
    usage
        .estimate_cost_usd_with_pricing(pricing)
        .total_cost_usd()
}

#[expect(dead_code)]
fn provider_kind_str(kind: api::ProviderKind) -> &'static str {
    match kind {
        api::ProviderKind::Anthropic => "anthropic",
        api::ProviderKind::OpenAi => "openai",
        api::ProviderKind::Xai => "xai",
    }
}

fn new_session_id() -> String {
    format!("sess-{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
}

// ---------------------------------------------------------------------------
// API key failover — resolves the best API client for the tenant.
// ---------------------------------------------------------------------------

/// Resolves a `ProviderClient` with automatic failover across multiple API keys.
/// Priority order (lower number = higher priority). Falls back to env if no DB keys.
/// Returns the client and the `key_id` of the resolved key.
/// `scenario` filters API keys by their `scenarios` JSON column (NULL = all scenarios).
pub(crate) async fn resolve_api_client_with_failover(
    tenant_id: &str,
    user_id: &str,
    model: &str,
    config_registry: Option<&std::sync::Arc<agent_gateway::TenantConfigRegistry>>,
    scenario: Option<&str>,
) -> Result<(api::ProviderClient, Option<String>, String), AppError> {
    let fallback_client = || {
        if !runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK") {
            return Err(AppError::Internal(
                "no approved tenant chat model is configured".to_string(),
            ));
        }
        api::ProviderClient::from_model(model)
            .map(|c| (c, None, "env-fallback".to_string()))
            .map_err(|e| AppError::Internal(format!("no API keys available: {e}")))
    };

    let Some(config_registry) = config_registry else {
        return fallback_client();
    };

    // Load API keys for this tenant, scoped to the requested scenario.
    let runtime_config = match config_registry
        .load_user_config(tenant_id, user_id, scenario)
        .await
    {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                "failed to load config for failover, using env fallback: {}",
                e
            );
            return fallback_client();
        }
    };

    if runtime_config.api_keys.is_empty() {
        tracing::info!(
            "no API keys in DB for tenant {}, using env fallback",
            tenant_id
        );
        return fallback_client();
    }

    // Try keys in priority order
    for entry in &runtime_config.api_keys {
        let model_fallback = model.to_string();
        let effective_model = entry.model.as_ref().unwrap_or(&model_fallback);
        // Use `build_provider` so the decrypted DB key is injected
        // directly, bypassing env-var heuristics. This is the same path
        // used by NL2SQL / agent runtime and correctly handles custom
        // providers (OpenRouter, NovitaAI, ...) whose keys live only
        // in the database.
        let result = api::build_provider(
            &entry.provider,
            effective_model,
            &entry.key,
            entry.base_url.as_deref(),
        );

        match result {
            Ok(client) => {
                tracing::info!(
                    "using API key '{}' (priority={}) for model {}",
                    entry.id,
                    entry.priority,
                    effective_model
                );
                return Ok((client, Some(entry.id.clone()), entry.provider.clone()));
            }
            Err(e) => {
                tracing::warn!(
                    "API key '{}' failed to create client: {}, trying next key",
                    entry.id,
                    e
                );
            }
        }
    }

    fallback_client()
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// POST /api/v1/chat/message — non-streaming chat with full response.
#[allow(clippy::too_many_lines)]
async fn send_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendMessageRequest>,
) -> impl IntoResponse {
    let user_id = &claims.sub;
    let tenant_id = claims.tenant_id.clone();

    if req.messages.is_empty() {
        return AppError::ValidationError("messages cannot be empty".into()).into_response();
    }

    let Some(last_msg) = req.messages.last() else {
        return AppError::ValidationError("messages cannot be empty".into()).into_response();
    };
    let last_msg_role = last_msg.role.clone();
    let last_msg_content = last_msg.content.clone();
    let session_id = req.session_id.unwrap_or_else(new_session_id);
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let model = req.model.unwrap_or_else(|| state.default_model.clone());
    let incoming_messages = to_api_messages(&req.messages);
    let kernel = crate::semantic_kernel_store::RuntimeExecutionKernel::new(
        state.db.clone(),
        tenant_id.clone(),
        user_id.clone(),
        session_id.clone(),
    );
    if let Err(error) =
        import_legacy_chat_if_needed(&kernel, &state.data_dir, &tenant_id, user_id, &session_id)
            .await
    {
        return error.into_response();
    }
    let api_messages = match kernel
        .prepare_chat_request(&request_id, &incoming_messages, &model)
        .await
    {
        Ok(messages) => messages,
        Err(error) => {
            return AppError::from(error).into_response();
        }
    };

    let (provider, api_key_id, provider_name) = match resolve_api_client_with_failover(
        &tenant_id,
        user_id,
        &model,
        state.config_registry.as_ref(),
        Some("chat"),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            if let Err(checkpoint_error) = kernel
                .record_chat_failure(&request_id, &format!("provider resolution failed: {e}"))
                .await
            {
                return AppError::Internal(format!(
                    "{e}; failed to persist terminal chat failure: {checkpoint_error}"
                ))
                .into_response();
            }
            return e.into_response();
        }
    };

    let api_req = api::MessageRequest {
        model: model.clone(),
        max_tokens: 8192,
        messages: api_messages,
        system: None,
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = match provider.send_message(&api_req).await {
        Ok(r) => r,
        Err(e) => {
            let detail = format!("LLM call failed: {e}");
            if let Err(checkpoint_error) = kernel.record_chat_failure(&request_id, &detail).await {
                return AppError::Internal(format!(
                    "{detail}; failed to persist terminal chat failure: {checkpoint_error}"
                ))
                .into_response();
            }
            return AppError::Internal(detail).into_response();
        }
    };

    let assistant_text: String = response
        .content
        .iter()
        .filter_map(|block| {
            if let api::OutputContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let ts = now_ms();

    if let Err(error) = kernel
        .record_chat_assistant(&request_id, &assistant_text)
        .await
    {
        return AppError::Internal(format!(
            "model response was not committed to the canonical ledger: {error}"
        ))
        .into_response();
    }

    // Compatibility JSONL is an export only. Durable success has already
    // committed user, assistant and terminal events above.
    if let Err(e) = append_message(
        &state.data_dir,
        &tenant_id,
        user_id,
        &session_id,
        &SessionMessageRecord::from_message(&last_msg_role, &last_msg_content, ts),
    ) {
        tracing::error!(
            error = %e,
            session_id = %session_id,
            "failed to persist user message to session file"
        );
    }
    if let Err(e) = append_message(
        &state.data_dir,
        &tenant_id,
        user_id,
        &session_id,
        &SessionMessageRecord::from_message(
            "assistant",
            &serde_json::json!(assistant_text),
            ts + 1,
        ),
    ) {
        tracing::error!(
            error = %e,
            session_id = %session_id,
            "failed to persist assistant message to session file"
        );
    }

    let usage = response.usage;
    let total_tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    let cost = estimate_cost(
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
        &model,
    );

    let record = TokenUsageRecord {
        tenant_id: tenant_id.clone(),
        user_id: user_id.clone(),
        session_id: session_id.clone(),
        request_id: Some(request_id),
        model: model.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        total_tokens,
        estimated_cost_usd: cost,
        api_key_id,
        provider: provider_name,
        created_at: Utc::now(),
    };
    if let Some(usage_writer) = state.usage_writer.as_ref() {
        let _ = usage_writer.write(&record).await;
    } else {
        tracing::warn!("token usage writer is not configured; chat usage was not persisted");
    }

    Json(SendMessageResponse {
        session_id,
        message: ChatMessage {
            role: "assistant".to_string(),
            content: serde_json::json!(assistant_text),
        },
        usage: Some(TokenUsageDto {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens,
            estimated_cost_usd: cost,
            model,
        }),
    })
    .into_response()
}

/// POST /api/v1/chat/stream — SSE streaming chat.
#[allow(clippy::too_many_lines)]
async fn stream_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendMessageRequest>,
) -> impl IntoResponse {
    let user_id = claims.sub.clone();
    let tenant_id = claims.tenant_id.clone();
    let req_session_id: Option<String> = req.session_id.clone();

    if req.messages.is_empty() {
        return AppError::ValidationError("messages cannot be empty".into()).into_response();
    }

    let Some(last_msg) = req.messages.last() else {
        return AppError::ValidationError("messages cannot be empty".into()).into_response();
    };
    let last_msg_role = last_msg.role.clone();
    let last_msg_content = last_msg.content.clone();
    let has_session_id = req_session_id.is_some();
    let session_id = req_session_id.unwrap_or_else(new_session_id);
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    tracing::debug!(has_session_id, session_id = %session_id, "stream_message using session");
    let model = req.model.unwrap_or_else(|| state.default_model.clone());
    let incoming_messages = to_api_messages(&req.messages);
    let kernel = crate::semantic_kernel_store::RuntimeExecutionKernel::new(
        state.db.clone(),
        tenant_id.clone(),
        user_id.clone(),
        session_id.clone(),
    );
    if let Err(error) =
        import_legacy_chat_if_needed(&kernel, &state.data_dir, &tenant_id, &user_id, &session_id)
            .await
    {
        return error.into_response();
    }
    let api_messages = match kernel
        .prepare_chat_request(&request_id, &incoming_messages, &model)
        .await
    {
        Ok(messages) => messages,
        Err(error) => {
            return AppError::from(error).into_response();
        }
    };

    let (provider, api_key_id, provider_name) = match resolve_api_client_with_failover(
        &tenant_id,
        &user_id,
        &model,
        state.config_registry.as_ref(),
        Some("chat"),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            if let Err(checkpoint_error) = kernel
                .record_chat_failure(&request_id, &format!("provider resolution failed: {e}"))
                .await
            {
                return AppError::Internal(format!(
                    "{e}; failed to persist terminal chat failure: {checkpoint_error}"
                ))
                .into_response();
            }
            return e.into_response();
        }
    };

    let api_req = api::MessageRequest {
        model: model.clone(),
        max_tokens: 8192,
        messages: api_messages,
        system: None,
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let data_dir = state.data_dir.clone();
    let usage_writer = state.usage_writer.clone();
    let user_id_clone = user_id.clone();
    let session_id_clone = session_id.clone();
    let model_clone = model.clone();
    let tenant_id_clone = tenant_id.clone();
    let ts = now_ms();

    if let Err(e) = append_message(
        &data_dir,
        &tenant_id_clone,
        &user_id_clone,
        &session_id_clone,
        &SessionMessageRecord::from_message(&last_msg_role, &last_msg_content, ts),
    ) {
        tracing::error!(
            error = %e,
            session_id = %session_id_clone,
            "failed to persist user message to session file"
        );
    }

    let session_id_clone2 = session_id.clone();
    let data_dir_clone = state.data_dir.clone();
    let provider_name_clone = provider_name.clone();
    let api_key_id_clone = api_key_id.clone();
    let request_id_clone = request_id.clone();
    let kernel_for_stream = kernel.clone();

    let sse_stream = async_stream::try_stream! {
        let mut stream = match provider.stream_message(&api_req).await {
            Ok(stream) => stream,
            Err(error) => {
                let detail = format!("LLM stream start failed: {error}");
                kernel_for_stream
                    .record_chat_failure(&request_id_clone, &detail)
                    .await
                    .map_err(|checkpoint_error| std::io::Error::other(format!(
                        "{detail}; failed to persist terminal chat failure: {checkpoint_error}"
                    )))?;
                Err(std::io::Error::other(detail))?;
                unreachable!();
            }
        };

        let mut full_text = String::new();
        let mut acc = StreamAcc::default();

        loop {
            match stream.next_event().await {
                Ok(Some(event)) => {
                    match event {
                        api::StreamEvent::ContentBlockStart(cb) => {
                            let block_type = match &cb.content_block {
                                api::OutputContentBlock::Text { .. } => "text",
                                api::OutputContentBlock::ToolUse { .. } => "tool_use",
                                api::OutputContentBlock::Thinking { .. } => "thinking",
                                api::OutputContentBlock::RedactedThinking { .. } => "redacted_thinking",
                            };
                            yield axum::response::sse::Event::default()
                                .event("content_block_start")
                                .data(serde_json::json!({
                                    "type": "content_block_start",
                                    "index": cb.index,
                                    "blockType": block_type,
                                }).to_string());
                        }
                        api::StreamEvent::ContentBlockDelta(d) => {
                            match &d.delta {
                                api::ContentBlockDelta::TextDelta { text } => {
                                    full_text.push_str(text);
                                    yield axum::response::sse::Event::default()
                                        .event("content_block_delta")
                                        .data(serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": d.index,
                                            "delta": { "type": "text_delta", "text": text }
                                        }).to_string());
                                }
                                api::ContentBlockDelta::ThinkingDelta { thinking } => {
                                    yield axum::response::sse::Event::default()
                                        .event("thinking_delta")
                                        .data(serde_json::json!({
                                            "type": "thinking_delta",
                                            "index": d.index,
                                            "thinking": thinking
                                        }).to_string());
                                }
                                api::ContentBlockDelta::InputJsonDelta { .. }
                                | api::ContentBlockDelta::SignatureDelta { .. } => {}
                            }
                        }
                        api::StreamEvent::MessageDelta(d) => {
                            acc.input = d.usage.input_tokens;
                            acc.output = d.usage.output_tokens;
                            acc.cache_creation = d.usage.cache_creation_input_tokens;
                            acc.cache_read = d.usage.cache_read_input_tokens;
                            yield axum::response::sse::Event::default()
                                .event("message_delta")
                                .data(serde_json::json!({
                                    "type": "message_delta",
                                    "usage": {
                                        "inputTokens": d.usage.input_tokens,
                                        "outputTokens": d.usage.output_tokens,
                                        "cacheCreationInputTokens": d.usage.cache_creation_input_tokens,
                                        "cacheReadInputTokens": d.usage.cache_read_input_tokens,
                                    },
                                    "stopReason": d.delta.stop_reason,
                                }).to_string());
                        }
                        _ => {}
                    }
                }
                Ok(None) => {
                    if let Err(error) = kernel_for_stream
                        .record_chat_assistant(&request_id_clone, &full_text)
                        .await
                    {
                        yield axum::response::sse::Event::default()
                            .event("error")
                            .data(serde_json::json!({
                                "error": format!("model response was not committed to the canonical ledger: {error}")
                            }).to_string());
                        break;
                    }
                    if let Err(error) = append_message(
                        &data_dir_clone,
                        &tenant_id_clone,
                        &user_id_clone,
                        &session_id_clone2,
                        &SessionMessageRecord::from_message(
                            "assistant",
                            &serde_json::json!(full_text),
                            ts + 1,
                        ),
                    ) {
                        tracing::error!(
                            error = %error,
                            session_id = %session_id_clone2,
                            "failed to export assistant message to compatibility JSONL"
                        );
                    }
                    let total_tokens = acc.input.saturating_add(acc.output);
                    let cost = estimate_cost(
                        acc.input,
                        acc.output,
                        acc.cache_creation,
                        acc.cache_read,
                        &model_clone,
                    );
                    let record = TokenUsageRecord {
                        tenant_id: tenant_id_clone.clone(),
                        user_id: user_id_clone.clone(),
                        session_id: session_id_clone2.clone(),
                        request_id: Some(request_id_clone.clone()),
                        model: model_clone.clone(),
                        input_tokens: acc.input,
                        output_tokens: acc.output,
                        cache_creation_tokens: acc.cache_creation,
                        cache_read_tokens: acc.cache_read,
                        total_tokens,
                        estimated_cost_usd: cost,
                        api_key_id: api_key_id_clone.clone(),
                        provider: provider_name_clone.clone(),
                        created_at: Utc::now(),
                    };
                    if let Some(writer) = usage_writer.as_ref() {
                        if let Err(error) = writer.write(&record).await {
                            tracing::error!(error = %error, "failed to write token usage for stream");
                        }
                    } else {
                        tracing::warn!("token usage writer is not configured; streaming chat usage was not persisted");
                    }

                    yield axum::response::sse::Event::default()
                        .event("stream_end")
                        .data(serde_json::json!({ "sessionId": session_id_clone2 }).to_string());
                    break;
                }
                Err(e) => {
                    let detail = format!("LLM stream failed: {e}");
                    if let Err(checkpoint_error) = kernel_for_stream
                        .record_chat_failure(&request_id_clone, &detail)
                        .await
                    {
                        yield axum::response::sse::Event::default()
                            .event("error")
                            .data(serde_json::json!({
                                "error": format!("{detail}; failed to persist terminal chat failure: {checkpoint_error}")
                            }).to_string());
                        break;
                    }
                    yield axum::response::sse::Event::default()
                        .event("error")
                        .data(serde_json::json!({ "error": detail }).to_string());
                    break;
                }
            }
        }
    };

    axum::response::Sse::new(sse_stream.map_err(|e: std::io::Error| {
        let boxed: axum::BoxError = e.into();
        boxed
    }))
    .into_response()
}

/// GET /api/v1/chat/sessions — list all sessions for the authenticated user.
async fn list_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> impl IntoResponse {
    match list_canonical_sessions(&state.db, &claims.tenant_id, &claims.sub).await {
        Ok(sessions) => {
            let total = sessions.len();
            Json(serde_json::json!({ "sessions": sessions, "total": total })).into_response()
        }
        Err(error) => error.into_response(),
    }
}

/// GET `/api/v1/chat/sessions/{session_id}` — get all messages in a session.
async fn get_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let kernel = crate::semantic_kernel_store::RuntimeExecutionKernel::new(
        state.db.clone(),
        claims.tenant_id.clone(),
        claims.sub.clone(),
        session_id.clone(),
    );
    if let Err(error) = import_legacy_chat_if_needed(
        &kernel,
        &state.data_dir,
        &claims.tenant_id,
        &claims.sub,
        &session_id,
    )
    .await
    {
        return error.into_response();
    }
    match kernel.load_chat_messages().await {
        Ok(messages) => Json(serde_json::json!({
            "sessionId": session_id,
            "userId": claims.sub,
            "messages": messages.into_iter().map(chat_message_from_api).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => AppError::Internal(format!("failed to load canonical chat session: {error}"))
            .into_response(),
    }
}

/// DELETE `/api/v1/chat/sessions/{session_id}` — delete a session.
async fn delete_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let kernel = crate::semantic_kernel_store::RuntimeExecutionKernel::new(
        state.db.clone(),
        claims.tenant_id.clone(),
        claims.sub.clone(),
        session_id.clone(),
    );
    if let Err(error) = kernel.delete_chat_session().await {
        return AppError::Internal(format!("failed to revoke canonical chat session: {error}"))
            .into_response();
    }
    if let Err(error) = crate::semantic_kernel_store::delete_session_artifacts(
        &state.db,
        &claims.tenant_id,
        &session_id,
    )
    .await
    {
        tracing::error!(
            tenant_id = %claims.tenant_id,
            session_id = %session_id,
            error = %error,
            "canonical chat session revoked but artifact tombstone cleanup failed"
        );
        return AppError::Internal(
            "session revoked; artifact cleanup will be retried by retention recovery".to_string(),
        )
        .into_response();
    }
    if let Err(error) =
        delete_session_file(&state.data_dir, &claims.tenant_id, &claims.sub, &session_id)
    {
        tracing::warn!(
            error = %error,
            session_id = %session_id,
            "canonical session was revoked but compatibility JSONL removal failed"
        );
    }
    Json(serde_json::json!({ "deleted": true })).into_response()
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/message", routing_post(send_message))
        .route("/stream", routing_post(stream_message))
        .route("/sessions", routing_get(list_sessions))
        .route("/sessions/{session_id}", routing_get(get_session))
        .route("/sessions/{session_id}", routing_delete(delete_session))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

// ---------------------------------------------------------------------------
// Stream accumulator
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StreamAcc {
    input: u32,
    output: u32,
    cache_creation: u32,
    cache_read: u32,
}
