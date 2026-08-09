//! Outbound Bot Gateway adapters.
//!
//! The Bot Gateway stores channel configuration in `bot_agent_channels`, but
//! platform-specific delivery belongs here. Route handlers can call this module
//! without knowing DingTalk/Feishu/etc. payload quirks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::{
    header::{HeaderName, HeaderValue, AUTHORIZATION},
    RequestBuilder,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{AppError, Result};
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

const FEISHU_MARKDOWN_CARD_MAX_BYTES: usize = 24_000;
const WECOM_MARKDOWN_MAX_BYTES: usize = 4_096;

#[derive(Debug, Clone)]
pub struct BotOutboundMessage {
    pub title: Option<String>,
    pub text: String,
    pub external_conversation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BotOutboundResult {
    pub ok: bool,
    pub status: String,
    pub log_id: String,
    pub provider_response: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct BotNotificationDispatchSummary {
    pub event_key: String,
    pub capability_key: String,
    pub attempted: usize,
    pub sent: usize,
    pub failed: usize,
}

#[derive(Debug)]
struct ChannelDeliveryConfig {
    tenant_id: String,
    agent_id: String,
    id: String,
    platform: String,
    enabled: bool,
    outbound_webhook_url: Option<String>,
    outbound_token: Option<String>,
    signing_secret: Option<String>,
    outbound_signing_secret: Option<String>,
    config_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct DingTalkResponse {
    #[serde(default)]
    errcode: i64,
    #[serde(default)]
    errmsg: String,
}

#[derive(Debug, Deserialize)]
struct FeishuTenantAccessTokenResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    msg: String,
    tenant_access_token: Option<String>,
}

fn clean_opt(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn outbound_dedupe_key(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Option<String> {
    let conversation = message
        .external_conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let canonical_text = message
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!canonical_text.is_empty()).then(|| {
        let digest = Sha256::digest(
            format!(
                "{}:{}:{}:{}",
                channel.tenant_id, channel.id, conversation, canonical_text
            )
            .as_bytes(),
        );
        hex::encode(digest)
    })
}

fn outbound_dedupe_lock(key: &str) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

async fn recent_sent_duplicate(
    state: &AppState,
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<Option<BotOutboundResult>> {
    let Some(conversation) = message
        .external_conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let canonical_text = message
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if canonical_text.is_empty() {
        return Ok(None);
    }
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, CAST(content_json AS TEXT) AS content_json
         FROM bot_message_logs
         WHERE tenant_id = ? AND channel_id = ? AND direction = 'outbound'
           AND status = 'sent' AND external_conversation_id = ?
           AND created_at >= datetime(CURRENT_TIMESTAMP, '-45 seconds')
         ORDER BY created_at DESC LIMIT 8",
    )
    .bind(&channel.tenant_id)
    .bind(&channel.id)
    .bind(conversation)
    .fetch_all(state.control_db())
    .await?;
    for row in rows {
        let content = row
            .get::<Option<String>, _>("content_json")
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let Some(content) = content else { continue };
        let existing = content
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "));
        if existing.as_deref() == Some(canonical_text.as_str()) {
            return Ok(Some(BotOutboundResult {
                ok: true,
                status: "deduplicated".to_string(),
                log_id: row.get("id"),
                provider_response: None,
            }));
        }
    }
    Ok(None)
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

fn protect_outbound_text(value: &str) -> runtime::ProtectedText {
    runtime::protect_sensitive_text(value, runtime::configured_data_protection_mode())
}

fn protect_outbound_json(value: &Value) -> (Value, runtime::DataProtectionReport) {
    runtime::protect_sensitive_json(value, runtime::configured_data_protection_mode())
}

fn report_contains_credentials(report: &runtime::DataProtectionReport) -> bool {
    report
        .categories
        .keys()
        .any(|category| !matches!(category.as_str(), "email" | "phone" | "payment_card"))
}

fn outbound_http_error(context: &str, error: &reqwest::Error) -> AppError {
    let class = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_builder() {
        "builder"
    } else {
        "http"
    };
    let status = error
        .status()
        .map(|status| format!(" status={}", status.as_u16()))
        .unwrap_or_default();
    AppError::Internal(format!("{context}: {class} error{status}"))
}

fn json_string(config: Option<&Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn json_bool(config: Option<&Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

fn json_string_array(config: Option<&Value>, key: &str) -> Vec<String> {
    config
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn json_object(config: Option<&Value>, keys: &[&str]) -> Option<Map<String, Value>> {
    keys.iter().find_map(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(Value::as_object)
            .cloned()
    })
}

fn feishu_app_id(channel: &ChannelDeliveryConfig) -> Option<String> {
    json_string(
        channel.config_json.as_ref(),
        &["appId", "app_id", "appKey", "app_key"],
    )
}

fn feishu_app_secret(channel: &ChannelDeliveryConfig) -> Option<String> {
    clean_opt(channel.signing_secret.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["appSecret", "app_secret", "clientSecret", "client_secret"],
        )
    })
}

fn feishu_api_base(channel: &ChannelDeliveryConfig) -> String {
    json_string(
        channel.config_json.as_ref(),
        &["apiBase", "api_base", "baseUrl", "base_url"],
    )
    .unwrap_or_else(|| {
        if channel.platform.eq_ignore_ascii_case("lark") {
            "https://open.larksuite.com/open-apis".to_string()
        } else {
            "https://open.feishu.cn/open-apis".to_string()
        }
    })
    .trim_end_matches('/')
    .to_string()
}

fn feishu_receive_id_type(channel: &ChannelDeliveryConfig) -> String {
    json_string(
        channel.config_json.as_ref(),
        &["receiveIdType", "receive_id_type"],
    )
    .unwrap_or_else(|| "chat_id".to_string())
}

fn value_to_header_string(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn message_text_with_title(message: &BotOutboundMessage) -> String {
    let title = message
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match title {
        Some(title) => format!("{title}\n\n{}", message.text),
        None => message.text.clone(),
    }
}

pub(crate) fn notification_event_enabled(config: Option<&Value>, event_key: &str) -> bool {
    let event_key = event_key.trim();
    if event_key.is_empty() {
        return false;
    }
    for key in ["notifyOnEvents", "notify_on_events", "notificationEvents"] {
        let events = json_string_array(config, key);
        if events
            .iter()
            .any(|event| event == "*" || event.eq_ignore_ascii_case(event_key))
        {
            return true;
        }
    }
    json_bool(
        config,
        &["notifyOnAnswerCompleted", "notify_on_answer_completed"],
    ) && event_key.ends_with(".answer_completed")
}

fn append_query_param(url: &str, key: &str, value: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{key}={}", urlencoding::encode(value))
}

fn normalize_dingtalk_keyword(keyword: &str) -> String {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return String::new();
    }
    if keyword.starts_with('[')
        || keyword.starts_with('【')
        || keyword.starts_with('#')
        || keyword.starts_with('@')
    {
        keyword.to_string()
    } else {
        format!("[{keyword}]")
    }
}

fn apply_keyword_prefix(text: &str, config: Option<&Value>) -> String {
    let Some(keyword) = json_string(config, &["keywordPrefix", "keyword_prefix", "keyword"]) else {
        return text.to_string();
    };
    let normalized = normalize_dingtalk_keyword(&keyword);
    if normalized.is_empty() || text.contains(&keyword) || text.contains(&normalized) {
        return text.to_string();
    }
    format!("{normalized} {text}")
}

fn sign_dingtalk_url(url: &str, secret: &str) -> Result<String> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Ok(url.to_string());
    }
    let timestamp = Utc::now().timestamp_millis().to_string();
    let string_to_sign = format!("{timestamp}\n{secret}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| AppError::Internal(format!("failed to create DingTalk HMAC signer: {e}")))?;
    mac.update(string_to_sign.as_bytes());
    let sign = BASE64.encode(mac.finalize().into_bytes());
    let with_timestamp = append_query_param(url, "timestamp", &timestamp);
    Ok(append_query_param(&with_timestamp, "sign", &sign))
}

fn build_dingtalk_url(channel: &ChannelDeliveryConfig) -> Result<String> {
    let mut url = clean_opt(channel.outbound_webhook_url.clone())
        .ok_or_else(|| AppError::ValidationError("outbound webhook url is required".to_string()))?;
    if !url.contains("access_token=") {
        if let Some(token) = clean_opt(channel.outbound_token.clone()) {
            url = append_query_param(&url, "access_token", &token);
        }
    }
    let secret = clean_opt(channel.outbound_signing_secret.clone())
        .or_else(|| clean_opt(channel.signing_secret.clone()));
    sign_dingtalk_url(&url, secret.as_deref().unwrap_or(""))
}

fn build_dingtalk_payload(channel: &ChannelDeliveryConfig, message: &BotOutboundMessage) -> Value {
    let config = channel.config_json.as_ref();
    let message_type = json_string(config, &["messageType", "message_type"])
        .unwrap_or_else(|| "text".to_string())
        .to_ascii_lowercase();
    let text = apply_keyword_prefix(&message_text_with_title(message), config);
    let at = json!({
        "isAtAll": json_bool(config, &["atAll", "mentionAll", "isAtAll"]),
        "atMobiles": json_string_array(config, "atMobiles"),
        "atUserIds": json_string_array(config, "atUserIds")
    });

    if message_type == "markdown" || message_has_markdown(&message.text) {
        let title = message
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("AOS 通知");
        json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": text
            },
            "at": at
        })
    } else {
        json!({
            "msgtype": "text",
            "text": { "content": text },
            "at": at
        })
    }
}

fn render_template_string(input: &str, context: &Map<String, Value>) -> String {
    let mut rendered = input.to_string();
    for (key, value) in context {
        let replacement = value
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string());
        for pattern in [
            format!("{{{{{key}}}}}"),
            format!("{{{{ {key} }}}}"),
            format!("${{{key}}}"),
        ] {
            rendered = rendered.replace(&pattern, &replacement);
        }
    }
    rendered
}

fn render_template_value(value: &Value, context: &Map<String, Value>) -> Value {
    match value {
        Value::String(raw) => Value::String(render_template_string(raw, context)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_template_value(item, context))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, item)| (key.clone(), render_template_value(item, context)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn generic_template_context(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Map<String, Value> {
    let text = apply_keyword_prefix(&message.text, channel.config_json.as_ref());
    let mut context = Map::new();
    context.insert(
        "title".to_string(),
        json!(message.title.clone().unwrap_or_default()),
    );
    context.insert("text".to_string(), json!(text));
    context.insert(
        "external_conversation_id".to_string(),
        json!(message.external_conversation_id.clone().unwrap_or_default()),
    );
    context.insert("channel_id".to_string(), json!(channel.id.clone()));
    context.insert("agent_id".to_string(), json!(channel.agent_id.clone()));
    context.insert("platform".to_string(), json!(channel.platform.clone()));
    context.insert("source".to_string(), json!("aos_bot_gateway"));
    context
}

fn build_generic_payload(channel: &ChannelDeliveryConfig, message: &BotOutboundMessage) -> Value {
    let config = channel.config_json.as_ref();
    let context = generic_template_context(channel, message);
    if let Some(template) = config.and_then(|value| {
        value
            .get("payloadTemplate")
            .or_else(|| value.get("payload_template"))
            .or_else(|| value.get("template"))
    }) {
        return render_template_value(template, &context);
    }
    json!({
        "title": message.title,
        "text": context.get("text").cloned().unwrap_or_else(|| json!("")),
        "external_conversation_id": message.external_conversation_id,
        "message_type": json_string(config, &["messageType", "message_type"]).unwrap_or_else(|| "text".to_string()),
        "channel_id": channel.id.clone(),
        "agent_id": channel.agent_id.clone(),
        "platform": channel.platform.clone(),
        "source": "aos_bot_gateway"
    })
}

fn apply_generic_headers(
    mut request: RequestBuilder,
    channel: &ChannelDeliveryConfig,
) -> Result<RequestBuilder> {
    let config = channel.config_json.as_ref();
    if let Some(token) = clean_opt(channel.outbound_token.clone()).or_else(|| {
        json_string(
            config,
            &[
                "token",
                "authToken",
                "auth_token",
                "bearerToken",
                "bearer_token",
            ],
        )
    }) {
        let name = json_string(config, &["authHeaderName", "auth_header_name"])
            .unwrap_or_else(|| "Authorization".to_string());
        let prefix = json_string(config, &["authHeaderPrefix", "auth_header_prefix"])
            .unwrap_or_else(|| "Bearer".to_string());
        let value = if prefix.trim().is_empty() {
            token
        } else {
            format!("{} {token}", prefix.trim())
        };
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
            AppError::ValidationError(format!("invalid generic webhook auth header name: {e}"))
        })?;
        let header_value = HeaderValue::from_str(&value).map_err(|e| {
            AppError::ValidationError(format!("invalid generic webhook auth header value: {e}"))
        })?;
        request = request.header(header_name, header_value);
    }

    if let Some(headers) = json_object(config, &["headers", "customHeaders", "custom_headers"]) {
        for (name, value) in headers {
            let Some(value) = value_to_header_string(&value) else {
                continue;
            };
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                AppError::ValidationError(format!("invalid generic webhook header '{name}': {e}"))
            })?;
            let header_value = HeaderValue::from_str(&value).map_err(|e| {
                AppError::ValidationError(format!(
                    "invalid value for generic webhook header '{name}': {e}"
                ))
            })?;
            request = request.header(header_name, header_value);
        }
    }

    Ok(request)
}

fn telegram_token(channel: &ChannelDeliveryConfig) -> Option<String> {
    clean_opt(channel.outbound_token.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["botToken", "bot_token", "telegramToken", "token"],
        )
    })
}

fn telegram_api_base(channel: &ChannelDeliveryConfig) -> String {
    json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://api.telegram.org".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn telegram_chat_id(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Option<String> {
    clean_opt(message.external_conversation_id.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &[
                "defaultChatId",
                "default_chat_id",
                "chatId",
                "chat_id",
                "recipient",
            ],
        )
    })
}

fn default_recipient(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Option<String> {
    clean_opt(message.external_conversation_id.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &[
                "defaultConversationId",
                "default_conversation_id",
                "defaultChatId",
                "default_chat_id",
                "defaultChannel",
                "default_channel",
                "defaultRecipient",
                "default_recipient",
                "recipient",
                "channel",
                "chat_id",
            ],
        )
    })
}

fn channel_delivery_skip_reason(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Option<&'static str> {
    let platform = channel.platform.trim().to_ascii_lowercase();
    let has_webhook = clean_opt(channel.outbound_webhook_url.clone()).is_some();
    let has_token = clean_opt(channel.outbound_token.clone()).is_some();
    match platform.as_str() {
        "dingtalk" | "wecom" | "generic_webhook" | "webhook" => {
            (!has_webhook).then_some("missing_webhook_url")
        }
        "feishu" | "lark" => {
            if has_webhook {
                None
            } else if feishu_app_id(channel).is_none() {
                Some("missing_feishu_app_id")
            } else if feishu_app_secret(channel).is_none() {
                Some("missing_feishu_app_secret")
            } else if default_recipient(channel, message).is_none() {
                Some("missing_default_recipient")
            } else {
                None
            }
        }
        "telegram" => {
            if telegram_token(channel).is_none() {
                Some("missing_telegram_bot_token")
            } else if telegram_chat_id(channel, message).is_none() {
                Some("missing_telegram_chat_id")
            } else {
                None
            }
        }
        "slack" | "discord" => {
            if has_webhook {
                None
            } else if !has_token {
                Some("missing_bot_token_or_webhook")
            } else if default_recipient(channel, message).is_none() {
                Some("missing_default_recipient")
            } else {
                None
            }
        }
        "whatsapp" => {
            if has_webhook {
                None
            } else if !has_token {
                Some("missing_whatsapp_access_token")
            } else if json_string(
                channel.config_json.as_ref(),
                &["phoneNumberId", "phone_number_id", "phoneId", "phone_id"],
            )
            .is_none()
            {
                Some("missing_whatsapp_phone_number_id")
            } else if default_recipient(channel, message).is_none() {
                Some("missing_default_recipient")
            } else {
                None
            }
        }
        _ => (!has_webhook).then_some("missing_webhook_url"),
    }
}

async fn load_channel(
    state: &AppState,
    tenant_id: &str,
    channel_id: &str,
) -> Result<ChannelDeliveryConfig> {
    let row = sqlx::query(
        r"
        SELECT tenant_id, agent_id, id, platform, enabled, outbound_webhook_url,
               outbound_token, signing_secret, outbound_signing_secret,
               CAST(config_json AS TEXT) AS config_json
        FROM bot_agent_channels
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(channel_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{channel_id}' not found")))?;

    Ok(ChannelDeliveryConfig {
        tenant_id: row.get("tenant_id"),
        agent_id: row.get("agent_id"),
        id: row.get("id"),
        platform: row.get("platform"),
        enabled: row.get("enabled"),
        outbound_webhook_url: row.get("outbound_webhook_url"),
        outbound_token: row.get("outbound_token"),
        signing_secret: row.get("signing_secret"),
        outbound_signing_secret: row.get("outbound_signing_secret"),
        config_json: parse_json_opt(row.get("config_json")),
    })
}

async fn insert_outbound_log(
    state: &AppState,
    channel: &ChannelDeliveryConfig,
    external_conversation_id: Option<&str>,
    content_json: Value,
    status: &str,
    error_message: Option<&str>,
) -> Result<String> {
    let log_id = uuid::Uuid::new_v4().to_string();
    let (content_json, mut protection_report) = protect_outbound_json(&content_json);
    let protected_external_conversation_id = external_conversation_id.map(protect_outbound_text);
    let protected_error_message = error_message.map(protect_outbound_text);
    if let Some(value) = &protected_external_conversation_id {
        protection_report.merge(&value.report);
    }
    if let Some(value) = &protected_error_message {
        protection_report.merge(&value.report);
    }
    if protection_report.redacted {
        tracing::warn!(
            tenant_id = %channel.tenant_id,
            agent_id = %channel.agent_id,
            channel_id = %channel.id,
            outbound_log_id = %log_id,
            finding_count = protection_report.finding_count,
            categories = ?protection_report.categories,
            "bot outbound audit data protection redacted sensitive values"
        );
    }
    sqlx::query(
        r"
        INSERT INTO bot_message_logs
            (id, tenant_id, agent_id, channel_id, direction, platform,
             external_conversation_id, message_type, content_json, status, error_message)
        VALUES (?, ?, ?, ?, 'outbound', ?, ?, 'text', ?, ?, ?)
        ",
    )
    .bind(&log_id)
    .bind(&channel.tenant_id)
    .bind(&channel.agent_id)
    .bind(&channel.id)
    .bind(&channel.platform)
    .bind(
        protected_external_conversation_id
            .as_ref()
            .map(|value| value.value.as_str()),
    )
    .bind(content_json)
    .bind(status)
    .bind(
        protected_error_message
            .as_ref()
            .map(|value| value.value.as_str()),
    )
    .execute(state.control_db())
    .await?;
    Ok(log_id)
}

async fn read_provider_json_response(platform: &str, response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| outbound_http_error(&format!("failed to read {platform} response"), &e))?;
    let provider_response =
        serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    let (provider_response, _) = protect_outbound_json(&provider_response);
    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "{platform} returned HTTP {status}: {}",
            provider_response
        )));
    }
    Ok(provider_response)
}

async fn post_platform_json(
    channel: &ChannelDeliveryConfig,
    url: &str,
    payload: &Value,
    platform: &str,
    with_generic_headers: bool,
) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let mut request = client.post(url);
    if with_generic_headers {
        request = apply_generic_headers(request, channel)?;
    }
    let response = request
        .json(payload)
        .send()
        .await
        .map_err(|e| outbound_http_error(&format!("{platform} request failed"), &e))?;
    read_provider_json_response(platform, response).await
}

async fn send_dingtalk(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    let url = build_dingtalk_url(channel)?;
    let payload = build_dingtalk_payload(channel, message);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let response = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| outbound_http_error("DingTalk request failed", &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| outbound_http_error("failed to read DingTalk response", &e))?;
    let provider_response =
        serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    let (provider_response, _) = protect_outbound_json(&provider_response);
    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "DingTalk returned HTTP {status}: {}",
            provider_response
        )));
    }
    if let Ok(parsed) = serde_json::from_value::<DingTalkResponse>(provider_response.clone()) {
        if parsed.errcode != 0 {
            return Err(AppError::Internal(format!(
                "DingTalk returned errcode {}: {}",
                parsed.errcode, parsed.errmsg
            )));
        }
    }
    Ok((payload, provider_response))
}

async fn send_generic_webhook(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    let url = clean_opt(channel.outbound_webhook_url.clone())
        .ok_or_else(|| AppError::ValidationError("outbound webhook url is required".to_string()))?;
    let payload = build_generic_payload(channel, message);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let request = apply_generic_headers(client.post(&url), channel)?;
    let response = request
        .json(&payload)
        .send()
        .await
        .map_err(|e| outbound_http_error("webhook request failed", &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| outbound_http_error("failed to read webhook response", &e))?;
    let provider_response =
        serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    let (provider_response, _) = protect_outbound_json(&provider_response);
    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "webhook returned HTTP {status}: {}",
            provider_response
        )));
    }
    Ok((payload, provider_response))
}

fn message_has_markdown(text: &str) -> bool {
    text.contains("```")
        || text.contains("**")
        || text.contains("](")
        || text.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("# ")
                || line.starts_with("## ")
                || line.starts_with("### ")
                || line.starts_with("- ")
                || line.starts_with("* ")
                || line.starts_with("> ")
                || (line.starts_with('|') && line.ends_with('|'))
                || line
                    .split_once(". ")
                    .is_some_and(|(prefix, _)| prefix.chars().all(|ch| ch.is_ascii_digit()))
        })
}

fn flatten_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(label_start) = remaining.find('[') {
        out.push_str(&remaining[..label_start]);
        let candidate = &remaining[label_start + 1..];
        let Some(label_end) = candidate.find("](") else {
            out.push_str(&remaining[label_start..]);
            return out;
        };
        let url_start = label_end + 2;
        let Some(url_end) = candidate[url_start..].find(')') else {
            out.push_str(&remaining[label_start..]);
            return out;
        };
        let label = &candidate[..label_end];
        let url = &candidate[url_start..url_start + url_end];
        out.push_str(label);
        out.push_str(" (");
        out.push_str(url);
        out.push(')');
        remaining = &candidate[url_start + url_end + 1..];
    }
    out.push_str(remaining);
    out
}

fn adapt_gfm_to_single_asterisk_markdown(input: &str, preserve_links: bool) -> String {
    let mut in_code_fence = false;
    let mut lines = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            lines.push(line.to_string());
            continue;
        }
        if in_code_fence {
            lines.push(line.to_string());
            continue;
        }
        let heading = trimmed
            .chars()
            .take_while(|character| *character == '#')
            .count();
        let mut adapted = if (1..=6).contains(&heading)
            && trimmed
                .chars()
                .nth(heading)
                .is_some_and(char::is_whitespace)
        {
            format!("*{}*", trimmed[heading..].trim())
        } else {
            line.to_string()
        };
        adapted = adapted.replace("**", "*");
        if !preserve_links {
            adapted = flatten_markdown_links(&adapted);
        }
        lines.push(adapted);
    }
    lines.join("\n")
}

fn build_feishu_card(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Option<Value> {
    let text = apply_keyword_prefix(&message.text, channel.config_json.as_ref());
    if text.len() > FEISHU_MARKDOWN_CARD_MAX_BYTES || !message_has_markdown(&text) {
        return None;
    }
    let mut card = json!({
        "config": { "wide_screen_mode": true },
        "elements": [{
            "tag": "div",
            "text": {
                "tag": "lark_md",
                "content": text
            }
        }]
    });
    if let Some(title) = message
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        card["header"] = json!({
            "template": "blue",
            "title": {
                "tag": "plain_text",
                "content": title.chars().take(120).collect::<String>()
            }
        });
    }
    Some(card)
}

fn add_feishu_webhook_signature(channel: &ChannelDeliveryConfig, payload: &mut Value) {
    if let Some(secret) = clean_opt(channel.outbound_signing_secret.clone()) {
        let timestamp = Utc::now().timestamp();
        payload["timestamp"] = Value::String(timestamp.to_string());
        payload["sign"] = Value::String(sign_feishu_webhook(timestamp, &secret));
    }
}

fn build_feishu_text_payload(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Value {
    let text = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    let mut payload = json!({
        "msg_type": "text",
        "content": { "text": text }
    });
    add_feishu_webhook_signature(channel, &mut payload);
    payload
}

fn build_feishu_payload(channel: &ChannelDeliveryConfig, message: &BotOutboundMessage) -> Value {
    if let Some(card) = build_feishu_card(channel, message) {
        let mut payload = json!({
            "msg_type": "interactive",
            "card": card
        });
        add_feishu_webhook_signature(channel, &mut payload);
        payload
    } else {
        build_feishu_text_payload(channel, message)
    }
}

fn feishu_provider_code(response: &Value) -> Option<i64> {
    ["code", "StatusCode", "status_code"]
        .iter()
        .find_map(|key| {
            response.get(*key).and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()))
            })
        })
}

fn feishu_webhook_response_accepted(response: &Value) -> bool {
    feishu_provider_code(response).is_none_or(|code| code == 0)
}

fn feishu_openapi_response_accepted(response: &Value) -> bool {
    feishu_provider_code(response) == Some(0)
}

fn feishu_card_was_explicitly_rejected(error: &AppError) -> bool {
    matches!(
        error,
        AppError::Internal(message)
            if message.starts_with("Feishu returned HTTP 400")
                || message.starts_with("Feishu returned HTTP 422")
    )
}

fn sign_feishu_webhook(timestamp: i64, secret: &str) -> String {
    // Feishu/Lark custom bot signs an empty body with "<timestamp>\n<secret>" as the HMAC key.
    let string_to_sign = format!("{timestamp}\n{}", secret.trim());
    let mut mac = HmacSha256::new_from_slice(string_to_sign.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(b"");
    BASE64.encode(mac.finalize().into_bytes())
}

async fn send_feishu(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    if let Some(url) = clean_opt(channel.outbound_webhook_url.clone()) {
        let payload = build_feishu_payload(channel, message);
        let rich = payload["msg_type"] == "interactive";
        match post_platform_json(channel, &url, &payload, "Feishu", true).await {
            Ok(provider_response) if feishu_webhook_response_accepted(&provider_response) => {
                return Ok((payload, provider_response));
            }
            Ok(_provider_response) if rich => {
                tracing::warn!(
                    channel_id = %channel.id,
                    "Feishu rejected the Markdown card; retrying once as plain text"
                );
                let fallback = build_feishu_text_payload(channel, message);
                let fallback_response =
                    post_platform_json(channel, &url, &fallback, "Feishu", true).await?;
                if !feishu_webhook_response_accepted(&fallback_response) {
                    return Err(AppError::Internal(format!(
                        "Feishu returned a non-zero status: {fallback_response}"
                    )));
                }
                return Ok((fallback, fallback_response));
            }
            Ok(provider_response) => {
                return Err(AppError::Internal(format!(
                    "Feishu returned a non-zero status: {provider_response}"
                )));
            }
            Err(error) if rich && feishu_card_was_explicitly_rejected(&error) => {
                tracing::warn!(
                    channel_id = %channel.id,
                    "Feishu rejected the Markdown card; retrying once as plain text"
                );
                let fallback = build_feishu_text_payload(channel, message);
                let fallback_response =
                    post_platform_json(channel, &url, &fallback, "Feishu", true).await?;
                if !feishu_webhook_response_accepted(&fallback_response) {
                    return Err(AppError::Internal(format!(
                        "Feishu returned a non-zero status: {fallback_response}"
                    )));
                }
                return Ok((fallback, fallback_response));
            }
            Err(error) => return Err(error),
        }
    }

    send_feishu_openapi(channel, message).await
}

async fn fetch_feishu_tenant_access_token(channel: &ChannelDeliveryConfig) -> Result<String> {
    let app_id = feishu_app_id(channel).ok_or_else(|| {
        AppError::ValidationError(
            "Feishu App ID is required when no custom bot webhook url is configured".to_string(),
        )
    })?;
    let app_secret = feishu_app_secret(channel).ok_or_else(|| {
        AppError::ValidationError(
            "Feishu App Secret is required when no custom bot webhook url is configured"
                .to_string(),
        )
    })?;
    let api_base = feishu_api_base(channel);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build Feishu client", &e))?;
    let response = client
        .post(format!("{api_base}/auth/v3/tenant_access_token/internal"))
        .json(&json!({
            "app_id": app_id,
            "app_secret": app_secret
        }))
        .send()
        .await
        .map_err(|e| outbound_http_error("Feishu tenant_access_token request failed", &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| outbound_http_error("failed to read Feishu token response", &e))?;
    let payload = serde_json::from_str::<FeishuTenantAccessTokenResponse>(&body).map_err(|e| {
        let protected_body = protect_outbound_text(&body);
        AppError::Internal(format!(
            "Feishu tenant_access_token returned non-JSON body: {e}; {}",
            protected_body.value
        ))
    })?;
    if !status.is_success() || payload.code != 0 {
        return Err(AppError::Internal(format!(
            "Feishu tenant_access_token returned HTTP {status}, code={}, msg={}",
            payload.code, payload.msg
        )));
    }
    payload.tenant_access_token.ok_or_else(|| {
        AppError::Internal("Feishu tenant_access_token response missing token".to_string())
    })
}

async fn send_feishu_openapi(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    let receive_id = default_recipient(channel, message).ok_or_else(|| {
        AppError::ValidationError(
            "Feishu default recipient/chat_id is required when no custom bot webhook url is configured".to_string(),
        )
    })?;
    let token = fetch_feishu_tenant_access_token(channel).await?;
    let api_base = feishu_api_base(channel);
    let receive_id_type = feishu_receive_id_type(channel);
    let build_payload = |force_text: bool| {
        if !force_text {
            if let Some(card) = build_feishu_card(channel, message) {
                return json!({
                    "receive_id": receive_id,
                    "msg_type": "interactive",
                    "content": card.to_string()
                });
            }
        }
        let text = apply_keyword_prefix(
            &message_text_with_title(message),
            channel.config_json.as_ref(),
        );
        json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": json!({ "text": text }).to_string()
        })
    };
    let payload = build_payload(false);
    let rich = payload["msg_type"] == "interactive";
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build Feishu client", &e))?;
    let send_payload = |payload: Value| {
        client
            .post(format!("{api_base}/im/v1/messages"))
            .query(&[("receive_id_type", receive_id_type.clone())])
            .bearer_auth(token.clone())
            .json(&payload)
            .send()
    };
    let response = send_payload(payload.clone())
        .await
        .map_err(|e| outbound_http_error("Feishu message create failed", &e))?;
    match read_provider_json_response("Feishu", response).await {
        Ok(provider_response) if feishu_openapi_response_accepted(&provider_response) => {
            Ok((payload, provider_response))
        }
        Ok(_provider_response) if rich => {
            tracing::warn!(
                channel_id = %channel.id,
                "Feishu OpenAPI rejected the Markdown card; retrying once as plain text"
            );
            let fallback = build_payload(true);
            let response = send_payload(fallback.clone())
                .await
                .map_err(|e| outbound_http_error("Feishu message create failed", &e))?;
            let fallback_response = read_provider_json_response("Feishu", response).await?;
            if !feishu_openapi_response_accepted(&fallback_response) {
                return Err(AppError::Internal(format!(
                    "Feishu message create returned non-zero code: {fallback_response}"
                )));
            }
            Ok((fallback, fallback_response))
        }
        Ok(provider_response) => Err(AppError::Internal(format!(
            "Feishu message create returned non-zero code: {provider_response}"
        ))),
        Err(error) if rich && feishu_card_was_explicitly_rejected(&error) => {
            tracing::warn!(
                channel_id = %channel.id,
                "Feishu OpenAPI rejected the Markdown card; retrying once as plain text"
            );
            let fallback = build_payload(true);
            let response = send_payload(fallback.clone())
                .await
                .map_err(|e| outbound_http_error("Feishu message create failed", &e))?;
            let fallback_response = read_provider_json_response("Feishu", response).await?;
            if !feishu_openapi_response_accepted(&fallback_response) {
                return Err(AppError::Internal(format!(
                    "Feishu message create returned non-zero code: {fallback_response}"
                )));
            }
            Ok((fallback, fallback_response))
        }
        Err(error) => Err(error),
    }
}

fn build_wecom_payload(channel: &ChannelDeliveryConfig, message: &BotOutboundMessage) -> Value {
    let text = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    if text.len() <= WECOM_MARKDOWN_MAX_BYTES && message_has_markdown(&message.text) {
        json!({
            "msgtype": "markdown",
            "markdown": { "content": text }
        })
    } else {
        json!({
            "msgtype": "text",
            "text": { "content": text }
        })
    }
}

fn build_wecom_text_payload(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Value {
    let text = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    json!({
        "msgtype": "text",
        "text": { "content": text }
    })
}

fn wecom_response_accepted(response: &Value) -> bool {
    response
        .get("errcode")
        .and_then(Value::as_i64)
        .is_none_or(|code| code == 0)
}

async fn send_wecom(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    let url = clean_opt(channel.outbound_webhook_url.clone())
        .ok_or_else(|| AppError::ValidationError("WeCom webhook url is required".to_string()))?;
    let payload = build_wecom_payload(channel, message);
    let provider_response = post_platform_json(channel, &url, &payload, "WeCom", true).await?;
    if wecom_response_accepted(&provider_response) {
        return Ok((payload, provider_response));
    }
    if payload["msgtype"] == "markdown" {
        tracing::warn!(
            channel_id = %channel.id,
            "WeCom rejected the Markdown payload; retrying once as plain text"
        );
        let fallback = build_wecom_text_payload(channel, message);
        let fallback_response = post_platform_json(channel, &url, &fallback, "WeCom", true).await?;
        if wecom_response_accepted(&fallback_response) {
            return Ok((fallback, fallback_response));
        }
        return Err(AppError::Internal(format!(
            "WeCom returned a non-zero status: {fallback_response}"
        )));
    }
    Err(AppError::Internal(format!(
        "WeCom returned a non-zero status: {provider_response}"
    )))
}

fn build_slack_webhook_payload(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Value {
    let text = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    let mut payload = json!({ "text": text });
    if let Some(username) = json_string(
        channel.config_json.as_ref(),
        &["username", "botName", "bot_name"],
    ) {
        payload["username"] = Value::String(username);
    }
    if let Some(icon_emoji) =
        json_string(channel.config_json.as_ref(), &["iconEmoji", "icon_emoji"])
    {
        payload["icon_emoji"] = Value::String(icon_emoji);
    }
    payload
}

async fn send_slack(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    if let Some(url) = clean_opt(channel.outbound_webhook_url.clone()) {
        let payload = build_slack_webhook_payload(channel, message);
        let provider_response = post_platform_json(channel, &url, &payload, "Slack", true).await?;
        return Ok((payload, provider_response));
    }

    let token = clean_opt(channel.outbound_token.clone())
        .ok_or_else(|| AppError::ValidationError("Slack Bot Token is required".to_string()))?;
    let channel_id = default_recipient(channel, message)
        .ok_or_else(|| AppError::ValidationError("Slack channel id is required".to_string()))?;
    let payload = json!({
        "channel": channel_id,
        "text": message_text_with_title(message)
    });
    let api_base = json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://slack.com/api".to_string())
        .trim_end_matches('/')
        .to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let response = client
        .post(format!("{api_base}/chat.postMessage"))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .map_err(|e| outbound_http_error("Slack chat.postMessage failed", &e))?;
    let provider_response = read_provider_json_response("Slack", response).await?;
    if provider_response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(AppError::Internal(format!(
            "Slack returned ok=false: {}",
            provider_response
        )));
    }
    Ok((payload, provider_response))
}

fn build_discord_payload(channel: &ChannelDeliveryConfig, message: &BotOutboundMessage) -> Value {
    let content = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    let mut payload = json!({ "content": content });
    if let Some(username) = json_string(
        channel.config_json.as_ref(),
        &["username", "botName", "bot_name"],
    ) {
        payload["username"] = Value::String(username);
    }
    payload
}

async fn send_discord(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    if let Some(url) = clean_opt(channel.outbound_webhook_url.clone()) {
        let payload = build_discord_payload(channel, message);
        let provider_response =
            post_platform_json(channel, &url, &payload, "Discord", true).await?;
        return Ok((payload, provider_response));
    }

    let token = clean_opt(channel.outbound_token.clone()).ok_or_else(|| {
        AppError::ValidationError("Discord Bot Token or webhook url is required".to_string())
    })?;
    let channel_id = default_recipient(channel, message)
        .ok_or_else(|| AppError::ValidationError("Discord channel id is required".to_string()))?;
    let api_base = json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://discord.com/api/v10".to_string())
        .trim_end_matches('/')
        .to_string();
    let authorization = if token.starts_with("Bot ") || token.starts_with("Bearer ") {
        token
    } else {
        format!("Bot {token}")
    };
    let payload = build_discord_payload(channel, message);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let response = client
        .post(format!("{api_base}/channels/{channel_id}/messages"))
        .header(AUTHORIZATION, authorization)
        .json(&payload)
        .send()
        .await
        .map_err(|e| outbound_http_error("Discord message create failed", &e))?;
    let provider_response = read_provider_json_response("Discord", response).await?;
    Ok((payload, provider_response))
}

fn build_whatsapp_cloud_payload(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
    recipient: &str,
) -> Value {
    let body = apply_keyword_prefix(
        &message_text_with_title(message),
        channel.config_json.as_ref(),
    );
    let body = if message_has_markdown(&body) {
        adapt_gfm_to_single_asterisk_markdown(&body, false)
    } else {
        body
    };
    json!({
        "messaging_product": "whatsapp",
        "to": recipient,
        "type": "text",
        "text": {
            "preview_url": json_bool(channel.config_json.as_ref(), &["previewUrl", "preview_url"]),
            "body": body
        }
    })
}

async fn send_whatsapp(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    if let Some(url) = clean_opt(channel.outbound_webhook_url.clone()) {
        let mut payload = build_generic_payload(channel, message);
        if payload.get("platform").is_none() {
            payload["platform"] = json!("whatsapp");
        }
        let provider_response =
            post_platform_json(channel, &url, &payload, "WhatsApp", true).await?;
        return Ok((payload, provider_response));
    }

    let token = clean_opt(channel.outbound_token.clone()).ok_or_else(|| {
        AppError::ValidationError("WhatsApp access token is required".to_string())
    })?;
    let phone_number_id = json_string(
        channel.config_json.as_ref(),
        &["phoneNumberId", "phone_number_id", "phoneId", "phone_id"],
    )
    .ok_or_else(|| AppError::ValidationError("WhatsApp phoneNumberId is required".to_string()))?;
    let recipient = default_recipient(channel, message)
        .ok_or_else(|| AppError::ValidationError("WhatsApp recipient is required".to_string()))?;
    let api_base = json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://graph.facebook.com/v20.0".to_string())
        .trim_end_matches('/')
        .to_string();
    let payload = build_whatsapp_cloud_payload(channel, message, &recipient);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let response = client
        .post(format!("{api_base}/{phone_number_id}/messages"))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .map_err(|e| outbound_http_error("WhatsApp Cloud API request failed", &e))?;
    let provider_response = read_provider_json_response("WhatsApp", response).await?;
    Ok((payload, provider_response))
}

async fn send_telegram(
    channel: &ChannelDeliveryConfig,
    message: &BotOutboundMessage,
) -> Result<(Value, Value)> {
    let token = telegram_token(channel).ok_or_else(|| {
        AppError::ValidationError("Telegram delivery requires Bot Token on the channel".to_string())
    })?;
    let chat_id = telegram_chat_id(channel, message).ok_or_else(|| {
        AppError::ValidationError(
            "Telegram delivery requires an inbound chat_id or Default Chat ID on the channel"
                .to_string(),
        )
    })?;
    let original_text = message_text_with_title(message);
    let configured_parse_mode =
        json_string(channel.config_json.as_ref(), &["parseMode", "parse_mode"]);
    let parse_mode = configured_parse_mode
        .clone()
        .or_else(|| message_has_markdown(&original_text).then(|| "Markdown".to_string()));
    let rendered_text = if parse_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("markdown"))
    {
        adapt_gfm_to_single_asterisk_markdown(&original_text, true)
    } else {
        original_text.clone()
    };
    let mut payload = json!({
        "chat_id": chat_id,
        "text": rendered_text,
    });
    if let Some(parse_mode) = parse_mode {
        payload["parse_mode"] = Value::String(parse_mode);
    }
    let api_base = telegram_api_base(channel);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| outbound_http_error("failed to build HTTP client", &e))?;
    let endpoint = format!("{api_base}/bot{token}/sendMessage");
    let send = |payload: Value| {
        let client = client.clone();
        let endpoint = endpoint.clone();
        async move {
            let response = client
                .post(endpoint)
                .json(&payload)
                .send()
                .await
                .map_err(|e| outbound_http_error("Telegram sendMessage failed", &e))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|e| outbound_http_error("failed to read Telegram response", &e))?;
            let provider_response =
                serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
            let (provider_response, _) = protect_outbound_json(&provider_response);
            Ok::<_, AppError>((payload, status, provider_response))
        }
    };
    let (sent_payload, status, provider_response) = send(payload.clone()).await?;
    if status.is_success() && provider_response.get("ok").and_then(Value::as_bool) != Some(false) {
        return Ok((sent_payload, provider_response));
    }
    if payload.get("parse_mode").is_some() {
        tracing::warn!(
            channel_id = %channel.id,
            "Telegram rejected the Markdown payload; retrying once as plain text"
        );
        let fallback = json!({
            "chat_id": chat_id,
            "text": original_text,
        });
        let (fallback, fallback_status, fallback_response) = send(fallback).await?;
        if fallback_status.is_success()
            && fallback_response.get("ok").and_then(Value::as_bool) != Some(false)
        {
            return Ok((fallback, fallback_response));
        }
        return Err(AppError::Internal(format!(
            "Telegram returned HTTP {fallback_status}: {fallback_response}"
        )));
    }
    Err(AppError::Internal(format!(
        "Telegram returned HTTP {status}: {provider_response}"
    )))
}

/// Send one outbound message through a configured Bot Gateway channel.
pub async fn send_channel_message(
    state: &AppState,
    tenant_id: &str,
    channel_id: &str,
    message: BotOutboundMessage,
) -> Result<BotOutboundResult> {
    let channel = load_channel(state, tenant_id, channel_id).await?;
    let protected_title = message.title.as_deref().map(protect_outbound_text);
    let protected_text = protect_outbound_text(&message.text);
    let mut protection_report = protected_text.report.clone();
    if let Some(title) = &protected_title {
        protection_report.merge(&title.report);
    }
    let message = BotOutboundMessage {
        title: protected_title.map(|value| value.value),
        text: protected_text.value,
        external_conversation_id: message.external_conversation_id,
    };
    if protection_report.redacted {
        tracing::warn!(
            tenant_id,
            agent_id = %channel.agent_id,
            channel_id = %channel.id,
            finding_count = protection_report.finding_count,
            categories = ?protection_report.categories,
            "bot outbound message data protection applied"
        );
    }
    if report_contains_credentials(&protection_report) {
        let content = json!({
            "title": message.title,
            "text": message.text,
            "reason": "outbound_data_protection",
            "findingCount": protection_report.finding_count,
            "categories": protection_report.categories,
        });
        let log_id = insert_outbound_log(
            state,
            &channel,
            message.external_conversation_id.as_deref(),
            content,
            "failed",
            Some("outbound message blocked because it contains credentials"),
        )
        .await?;
        return Err(AppError::ValidationError(format!(
            "bot outbound delivery blocked by data protection; log_id={log_id}"
        )));
    }
    if !channel.enabled {
        let content = json!({
            "title": message.title,
            "text": message.text,
            "reason": "channel_disabled"
        });
        let log_id = insert_outbound_log(
            state,
            &channel,
            message.external_conversation_id.as_deref(),
            content,
            "failed",
            Some("channel disabled"),
        )
        .await?;
        return Err(AppError::ValidationError(format!(
            "bot channel '{channel_id}' is disabled; log_id={log_id}"
        )));
    }

    // Direct replies and opt-in task notifications can be produced by
    // different workers for the same state transition. Serialize identical
    // deliveries per conversation and reuse a recent successful log entry so
    // a channel never receives the same body twice within the retry window.
    let dedupe_lock = outbound_dedupe_key(&channel, &message).map(|key| outbound_dedupe_lock(&key));
    let _dedupe_guard = match dedupe_lock.as_ref() {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };
    if let Some(existing) = recent_sent_duplicate(state, &channel, &message).await? {
        tracing::info!(
            tenant_id,
            channel_id = %channel.id,
            conversation_id = ?message.external_conversation_id,
            log_id = %existing.log_id,
            "bot outbound delivery deduplicated"
        );
        return Ok(existing);
    }

    let platform = channel.platform.to_ascii_lowercase();
    let started = Instant::now();
    tracing::info!(
        tenant_id = %tenant_id,
        agent_id = %channel.agent_id,
        channel_id = %channel.id,
        platform = %platform,
        external_conversation_id = ?message.external_conversation_id,
        text_chars = message.text.chars().count(),
        has_custom_webhook = channel.outbound_webhook_url.is_some(),
        has_outbound_token = channel.outbound_token.is_some(),
        "bot outbound delivery started"
    );
    let sent = match platform.as_str() {
        "dingtalk" => send_dingtalk(&channel, &message).await,
        "telegram" => send_telegram(&channel, &message).await,
        "feishu" | "lark" => send_feishu(&channel, &message).await,
        "wecom" => send_wecom(&channel, &message).await,
        "slack" => send_slack(&channel, &message).await,
        "discord" => send_discord(&channel, &message).await,
        "whatsapp" => send_whatsapp(&channel, &message).await,
        _ => send_generic_webhook(&channel, &message).await,
    };

    match sent {
        Ok((request_payload, provider_response)) => {
            let provider_response_chars = provider_response.to_string().chars().count();
            let content = json!({
                "title": message.title,
                "text": message.text,
                "external_conversation_id": message.external_conversation_id,
                "request_payload": request_payload,
                "provider_response": provider_response
            });
            let log_id = insert_outbound_log(
                state,
                &channel,
                message.external_conversation_id.as_deref(),
                content,
                "sent",
                None,
            )
            .await?;
            tracing::info!(
                tenant_id = %tenant_id,
                agent_id = %channel.agent_id,
                channel_id = %channel.id,
                platform = %platform,
                outbound_log_id = %log_id,
                provider_response_chars,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bot outbound delivery completed"
            );
            Ok(BotOutboundResult {
                ok: true,
                status: "sent".to_string(),
                log_id,
                provider_response: Some(provider_response),
            })
        }
        Err(err) => {
            let error_message = err.to_string();
            let is_validation_error = matches!(err, AppError::ValidationError(_));
            let content = json!({
                "title": message.title,
                "text": message.text,
                "external_conversation_id": message.external_conversation_id,
                "error": error_message
            });
            let log_id = insert_outbound_log(
                state,
                &channel,
                message.external_conversation_id.as_deref(),
                content,
                "failed",
                Some(&error_message),
            )
            .await?;
            tracing::warn!(
                tenant_id = %tenant_id,
                agent_id = %channel.agent_id,
                channel_id = %channel.id,
                platform = %platform,
                outbound_log_id = %log_id,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bot outbound delivery failed: {}",
                error_message
            );
            let message = format!("bot outbound delivery failed; log_id={log_id}; {error_message}");
            if is_validation_error {
                Err(AppError::ValidationError(message))
            } else {
                Err(AppError::Internal(message))
            }
        }
    }
}

/// Dispatch a capability event to every enabled channel that explicitly opted in.
///
/// This is deliberately non-magical: no channel receives event notifications
/// unless its `config_json.notifyOnEvents` includes `event_key` (or `*`).
pub async fn notify_capability_event(
    state: &AppState,
    tenant_id: &str,
    capability_key: &str,
    event_key: &str,
    message: BotOutboundMessage,
) -> Result<BotNotificationDispatchSummary> {
    let rows = sqlx::query(
        r"
        SELECT c.id, CAST(c.config_json AS TEXT) AS config_json
        FROM bot_agent_channels c
        INNER JOIN bot_agents a
            ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
        INNER JOIN bot_agent_capabilities cap
            ON cap.tenant_id = c.tenant_id AND cap.agent_id = c.agent_id
        WHERE c.tenant_id = ?
          AND c.enabled = 1
          AND a.enabled = 1
          AND cap.enabled = 1
          AND cap.capability_key = ?
        ORDER BY c.updated_at DESC
        ",
    )
    .bind(tenant_id)
    .bind(capability_key)
    .fetch_all(state.control_db())
    .await?;

    let mut attempted = 0usize;
    let mut sent = 0usize;
    let mut failed = 0usize;
    for row in rows {
        let channel_id: String = row.get("id");
        let config = parse_json_opt(row.get("config_json"));
        if !notification_event_enabled(config.as_ref(), event_key) {
            continue;
        }
        let channel = match load_channel(state, tenant_id, &channel_id).await {
            Ok(channel) => channel,
            Err(error) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    tenant_id = %tenant_id,
                    channel_id = %channel_id,
                    capability_key = %capability_key,
                    event_key = %event_key,
                    "bot notification channel load failed: {}",
                    error
                );
                continue;
            }
        };
        if let Some(reason) = channel_delivery_skip_reason(&channel, &message) {
            tracing::info!(
                tenant_id = %tenant_id,
                channel_id = %channel_id,
                capability_key = %capability_key,
                event_key = %event_key,
                reason,
                "bot notification skipped because channel delivery is not fully configured"
            );
            continue;
        }
        attempted = attempted.saturating_add(1);
        match send_channel_message(state, tenant_id, &channel_id, message.clone()).await {
            Ok(_) => sent = sent.saturating_add(1),
            Err(error) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    tenant_id = %tenant_id,
                    channel_id = %channel_id,
                    capability_key = %capability_key,
                    event_key = %event_key,
                    "bot notification dispatch failed: {}",
                    error
                );
            }
        }
    }

    Ok(BotNotificationDispatchSummary {
        event_key: event_key.to_string(),
        capability_key: capability_key.to_string(),
        attempted,
        sent,
        failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generic_channel(config_json: Option<Value>) -> ChannelDeliveryConfig {
        ChannelDeliveryConfig {
            tenant_id: "tenant-1".to_string(),
            agent_id: "agent-1".to_string(),
            id: "channel-1".to_string(),
            platform: "generic_webhook".to_string(),
            enabled: true,
            outbound_webhook_url: Some("https://example.com/webhook".to_string()),
            outbound_token: None,
            signing_secret: None,
            outbound_signing_secret: None,
            config_json,
        }
    }

    #[test]
    fn keyword_prefix_is_bracketed_once() {
        let config = json!({ "keywordPrefix": "AOS" });
        assert_eq!(
            apply_keyword_prefix("任务完成", Some(&config)),
            "[AOS] 任务完成"
        );
        assert_eq!(
            apply_keyword_prefix("[AOS] 任务完成", Some(&config)),
            "[AOS] 任务完成"
        );
    }

    #[test]
    fn outbound_dedupe_key_is_scoped_to_conversation_and_canonical_text() {
        let channel = generic_channel(None);
        let first = BotOutboundMessage {
            title: Some("first title".to_string()),
            text: "  task   completed\n".to_string(),
            external_conversation_id: Some("room-1".to_string()),
        };
        let same_body = BotOutboundMessage {
            title: Some("different title".to_string()),
            text: "task completed".to_string(),
            external_conversation_id: Some("room-1".to_string()),
        };
        let other_room = BotOutboundMessage {
            external_conversation_id: Some("room-2".to_string()),
            ..same_body.clone()
        };
        assert_eq!(
            outbound_dedupe_key(&channel, &first),
            outbound_dedupe_key(&channel, &same_body)
        );
        assert_ne!(
            outbound_dedupe_key(&channel, &first),
            outbound_dedupe_key(&channel, &other_room)
        );
        assert!(outbound_dedupe_key(
            &channel,
            &BotOutboundMessage {
                external_conversation_id: None,
                ..first
            }
        )
        .is_none());
    }

    #[test]
    fn explicit_keyword_prefix_is_preserved() {
        assert_eq!(normalize_dingtalk_keyword("【AOS】"), "【AOS】");
        assert_eq!(normalize_dingtalk_keyword("#AOS"), "#AOS");
    }

    #[test]
    fn outbound_protection_blocks_credentials_but_can_redact_pii() {
        let credential = runtime::protect_sensitive_text(
            "api_key=sk-1234567890abcdef",
            runtime::DataProtectionMode::SecretsOnly,
        );
        assert!(report_contains_credentials(&credential.report));
        assert!(!credential.value.contains("sk-1234567890abcdef"));

        let pii = runtime::protect_sensitive_text(
            "owner@example.test",
            runtime::DataProtectionMode::StrictPii,
        );
        assert!(pii.report.redacted);
        assert!(!report_contains_credentials(&pii.report));
        assert!(!pii.value.contains("owner@example.test"));
    }

    #[test]
    fn notification_event_requires_explicit_opt_in() {
        let config = json!({ "notifyOnEvents": ["pm_assistant.answer_completed"] });
        assert!(notification_event_enabled(
            Some(&config),
            "pm_assistant.answer_completed"
        ));
        assert!(!notification_event_enabled(
            Some(&config),
            "nl2sql.answer_completed"
        ));
        assert!(!notification_event_enabled(
            None,
            "pm_assistant.answer_completed"
        ));
    }

    #[test]
    fn generic_payload_template_replaces_context_values() {
        let channel = generic_channel(Some(json!({
            "payloadTemplate": {
                "body": "{{text}}",
                "room": "{{ external_conversation_id }}",
                "source": "${source}",
                "platform": "{{platform}}"
            }
        })));
        let payload = build_generic_payload(
            &channel,
            &BotOutboundMessage {
                title: Some("标题".to_string()),
                text: "hello".to_string(),
                external_conversation_id: Some("room-1".to_string()),
            },
        );
        assert_eq!(payload["body"], "hello");
        assert_eq!(payload["room"], "room-1");
        assert_eq!(payload["source"], "aos_bot_gateway");
        assert_eq!(payload["platform"], "generic_webhook");
    }

    #[test]
    fn generic_default_payload_contains_routing_context() {
        let channel = generic_channel(None);
        let payload = build_generic_payload(
            &channel,
            &BotOutboundMessage {
                title: None,
                text: "hello".to_string(),
                external_conversation_id: None,
            },
        );
        assert_eq!(payload["channel_id"], "channel-1");
        assert_eq!(payload["agent_id"], "agent-1");
        assert_eq!(payload["platform"], "generic_webhook");
        assert_eq!(payload["source"], "aos_bot_gateway");
    }

    #[test]
    fn platform_webhook_payloads_match_provider_shapes() {
        let message = BotOutboundMessage {
            title: Some("标题".to_string()),
            text: "hello".to_string(),
            external_conversation_id: Some("room-1".to_string()),
        };
        let mut channel = generic_channel(None);

        channel.platform = "feishu".to_string();
        let feishu = build_feishu_payload(&channel, &message);
        assert_eq!(feishu["msg_type"], "text");
        assert!(feishu["content"]["text"]
            .as_str()
            .unwrap()
            .contains("hello"));

        channel.platform = "wecom".to_string();
        let wecom = build_wecom_payload(&channel, &message);
        assert_eq!(wecom["msgtype"], "text");
        assert!(wecom["text"]["content"].as_str().unwrap().contains("hello"));

        channel.platform = "discord".to_string();
        let discord = build_discord_payload(&channel, &message);
        assert!(discord["content"].as_str().unwrap().contains("hello"));

        channel.platform = "whatsapp".to_string();
        let whatsapp = build_whatsapp_cloud_payload(&channel, &message, "6281234567890");
        assert_eq!(whatsapp["messaging_product"], "whatsapp");
        assert_eq!(whatsapp["to"], "6281234567890");
        assert_eq!(whatsapp["type"], "text");
    }

    #[test]
    fn feishu_markdown_uses_a_lark_md_card_without_changing_plain_text_messages() {
        let mut channel = generic_channel(None);
        channel.platform = "feishu".to_string();
        let markdown = build_feishu_payload(
            &channel,
            &BotOutboundMessage {
                title: Some("研究结论".to_string()),
                text: "## 结论\n\n- **北京**：晴\n- [来源](https://example.com)".to_string(),
                external_conversation_id: Some("room-1".to_string()),
            },
        );
        assert_eq!(markdown["msg_type"], "interactive");
        assert_eq!(markdown["card"]["elements"][0]["text"]["tag"], "lark_md");
        assert!(markdown["card"]["elements"][0]["text"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("**北京**")));
        assert_eq!(markdown["card"]["header"]["title"]["content"], "研究结论");

        let plain = build_feishu_payload(
            &channel,
            &BotOutboundMessage {
                title: None,
                text: "绑定成功".to_string(),
                external_conversation_id: Some("room-1".to_string()),
            },
        );
        assert_eq!(plain["msg_type"], "text");
    }

    #[test]
    fn dingtalk_and_wecom_auto_render_markdown_for_legacy_configs() {
        let message = BotOutboundMessage {
            title: Some("研究结论".to_string()),
            text: "## 结论\n\n- **北京**：晴".to_string(),
            external_conversation_id: Some("room-1".to_string()),
        };
        let mut channel = generic_channel(None);
        channel.platform = "dingtalk".to_string();
        let dingtalk = build_dingtalk_payload(&channel, &message);
        assert_eq!(dingtalk["msgtype"], "markdown");
        assert!(dingtalk["markdown"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("**北京**")));

        channel.platform = "wecom".to_string();
        let wecom = build_wecom_payload(&channel, &message);
        assert_eq!(wecom["msgtype"], "markdown");
        assert!(wecom["markdown"]["content"]
            .as_str()
            .is_some_and(|text| text.contains("**北京**")));
    }

    #[test]
    fn single_asterisk_platforms_receive_compatible_markdown_without_losing_links() {
        let source = "## 结论\n\n- **北京**：晴\n- [来源](https://example.com)";
        let telegram = adapt_gfm_to_single_asterisk_markdown(source, true);
        assert!(telegram.contains("*结论*"));
        assert!(telegram.contains("*北京*"));
        assert!(telegram.contains("[来源](https://example.com)"));

        let whatsapp = adapt_gfm_to_single_asterisk_markdown(source, false);
        assert!(whatsapp.contains("*结论*"));
        assert!(whatsapp.contains("*北京*"));
        assert!(whatsapp.contains("来源 (https://example.com)"));
        assert!(!whatsapp.contains("[来源]("));
    }

    #[test]
    fn whatsapp_payload_uses_its_supported_markdown_subset() {
        let mut channel = generic_channel(None);
        channel.platform = "whatsapp".to_string();
        let payload = build_whatsapp_cloud_payload(
            &channel,
            &BotOutboundMessage {
                title: Some("报告".to_string()),
                text: "**完成**：[查看](https://example.com)".to_string(),
                external_conversation_id: None,
            },
            "6281234567890",
        );
        let body = payload["text"]["body"].as_str().unwrap_or_default();
        assert!(body.contains("*完成*"));
        assert!(body.contains("查看 (https://example.com)"));
    }

    #[test]
    fn feishu_oversized_markdown_falls_back_to_lossless_plain_text() {
        let mut channel = generic_channel(None);
        channel.platform = "feishu".to_string();
        let text = format!("## 报告\n{}", "内容".repeat(FEISHU_MARKDOWN_CARD_MAX_BYTES));
        let payload = build_feishu_payload(
            &channel,
            &BotOutboundMessage {
                title: None,
                text: text.clone(),
                external_conversation_id: Some("room-1".to_string()),
            },
        );
        assert_eq!(payload["msg_type"], "text");
        assert_eq!(payload["content"]["text"], text);
    }

    #[test]
    fn feishu_provider_status_detection_supports_webhook_and_openapi_shapes() {
        assert!(feishu_webhook_response_accepted(&json!({
            "StatusCode": 0,
            "StatusMessage": "success"
        })));
        assert!(!feishu_webhook_response_accepted(&json!({
            "StatusCode": 19001
        })));
        assert!(feishu_openapi_response_accepted(&json!({ "code": 0 })));
        assert!(!feishu_openapi_response_accepted(&json!({
            "raw": "unexpected response"
        })));
    }

    #[test]
    fn feishu_signing_adds_timestamp_and_sign() {
        let mut channel = generic_channel(None);
        channel.outbound_signing_secret = Some("secret".to_string());
        let payload = build_feishu_payload(
            &channel,
            &BotOutboundMessage {
                title: None,
                text: "hello".to_string(),
                external_conversation_id: None,
            },
        );
        assert!(payload["timestamp"].as_str().is_some());
        assert!(payload["sign"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn feishu_app_secret_is_not_reused_for_custom_webhook_signing() {
        let mut channel = generic_channel(None);
        channel.signing_secret = Some("app-secret".to_string());
        channel.outbound_signing_secret = None;
        let payload = build_feishu_payload(
            &channel,
            &BotOutboundMessage {
                title: None,
                text: "hello".to_string(),
                external_conversation_id: None,
            },
        );
        assert!(payload.get("timestamp").is_none());
        assert!(payload.get("sign").is_none());
    }
}
