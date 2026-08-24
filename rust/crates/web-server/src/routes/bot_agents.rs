//! Bot Gateway API.
//!
//! This module owns robot/channel/rule configuration and the public inbound
//! webhook entrypoint. It intentionally stays separate from hooks: hooks react
//! inside AOS runtime, while bot agents expose AOS capabilities to external chat
//! platforms such as DingTalk, Feishu/Lark, Slack, Telegram, etc.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get as routing_get, post as routing_post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::agent_ops::{self, CreateAgentTaskInput};
use crate::routes::bot_agents_outbound::{
    send_channel_message, BotOutboundMessage, BotOutboundResult,
};
use crate::routes::bot_agents_router::{
    bot_router_confidence_threshold, normalize_router_capability, parse_router_intent_with_llm,
    router_enabled_capabilities, rule_router_target,
};
pub use crate::routes::bot_agents_types::{
    capability_binding_parts, clean_opt, decrypt_bot_channel_secret, default_capability_bindings,
    encrypt_bot_channel_secret, first_capability_key, BotAgentCapabilityInfo, BotAgentChannelInfo,
    BotAgentInfo, BotChannelSecretKind, BotMessageLogInfo, CapabilityBindingInput,
    ChannelListQuery, CreateAgentRequest, CreateChannelRequest, ListResponse, LogListQuery,
    TestChannelRequest, UpdateAgentRequest, UpdateChannelRequest, WebhookQuery,
};
use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};
use crate::routes::PaginationParams;
use crate::state::AppState;

const BOT_CONTEXT_MAX_HISTORY_MESSAGES: usize = 20;
const BOT_CONTEXT_KEEP_RECENT_MESSAGES: usize = 6;
const BOT_CONTEXT_SUMMARIZE_THRESHOLD_CHARS: usize = 12_000;
const BOT_CONTEXT_RECENT_BUDGET_CHARS: usize = 6_000;
const BOT_CONTEXT_SUMMARY_TARGET_CHARS: usize = 2_000;
const BOT_QUEUE_DEFAULT_BATCH_SIZE: i64 = 5;
const BOT_QUEUE_DEFAULT_POLL_MS: u64 = 1_000;
const BOT_QUEUE_DEFAULT_CLAIM_TIMEOUT_SECS: u64 = 600;
const BOT_QUEUE_DEFAULT_RECOVERY_INTERVAL_SECS: u64 = 60;
const BOT_QUEUE_MAX_ATTEMPTS: i32 = 3;
const BOT_ATTACHMENT_MAX_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;

static BOT_SESSION_SCOPE_LOCKS: OnceLock<StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>> =
    OnceLock::new();

fn bot_session_scope_lock(scope: &str) -> Arc<AsyncMutex<()>> {
    let locks = BOT_SESSION_SCOPE_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(scope).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(scope.to_string(), Arc::downgrade(&lock));
    lock
}

#[derive(Debug, Clone)]
struct BotConversationMessage {
    direction: String,
    status: String,
    text: String,
    created_at: String,
}

#[derive(Debug, Clone, Default)]
struct BotConversationContext {
    summary: Option<String>,
    recent_messages: Vec<BotConversationMessage>,
    loaded_messages: usize,
    summarized_messages: usize,
    total_chars: usize,
}

fn normalize_inbound_mode(value: Option<String>) -> Result<String> {
    let mode = clean_opt(value)
        .unwrap_or_else(|| "auto".to_string())
        .to_ascii_lowercase();
    match mode.as_str() {
        "auto" | "webhook" | "stream" | "socket" | "polling" => Ok(mode),
        other => Err(AppError::ValidationError(format!(
            "unsupported inbound_mode '{other}'"
        ))),
    }
}

fn normalize_platform(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn validate_platform_inbound_mode(platform: &str, inbound_mode: &str) -> Result<()> {
    let platform = normalize_platform(platform);
    let mode = inbound_mode.trim().to_ascii_lowercase();
    if !matches!(
        platform.as_str(),
        "dingtalk"
            | "feishu"
            | "lark"
            | "wecom"
            | "slack"
            | "discord"
            | "telegram"
            | "whatsapp"
            | "generic_webhook"
    ) {
        return Err(AppError::ValidationError(format!(
            "unsupported bot platform '{platform}'"
        )));
    }
    // Keep `auto` valid for existing records and API clients. The runtime maps
    // it to the platform's one preferred local transport.
    if mode == "auto" {
        return Ok(());
    }

    let supported = match platform.as_str() {
        "dingtalk" | "feishu" | "lark" | "wecom" => mode == "stream",
        "slack" | "discord" => mode == "socket",
        "telegram" => matches!(mode.as_str(), "polling" | "webhook"),
        "whatsapp" | "generic_webhook" => mode == "webhook",
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err(AppError::ValidationError(format!(
            "inbound_mode '{mode}' is not supported for platform '{platform}'"
        )))
    }
}

fn platform_accepts_webhook_ingress(platform: &str, inbound_mode: &str) -> bool {
    let platform = normalize_platform(platform);
    let mode = inbound_mode.trim().to_ascii_lowercase();
    mode == "webhook"
        || (mode == "auto" && matches!(platform.as_str(), "whatsapp" | "generic_webhook"))
}

fn normalize_inbound_mode_update(value: Option<String>) -> Result<Option<String>> {
    clean_opt(value)
        .map(|mode| normalize_inbound_mode(Some(mode)))
        .transpose()
}

fn json_opt(value: Option<Value>) -> Option<Value> {
    value
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

fn dt_to_rfc3339(row: &sqlx::sqlite::SqliteRow, name: &str) -> String {
    row.get::<chrono::DateTime<chrono::Utc>, _>(name)
        .to_rfc3339()
}

fn opt_dt_to_rfc3339(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<String> {
    row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(name)
        .map(|value| value.to_rfc3339())
}

async fn load_capabilities(
    state: &AppState,
    tenant_id: &str,
    agent_id: &str,
) -> Result<Vec<BotAgentCapabilityInfo>> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, agent_id, capability_key, enabled, CAST(config_json AS TEXT) AS config_json
        FROM bot_agent_capabilities
        WHERE tenant_id = ? AND agent_id = ?
        ORDER BY capability_key ASC
        ",
    )
    .bind(tenant_id)
    .bind(agent_id)
    .fetch_all(state.control_db())
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| BotAgentCapabilityInfo {
            id: row.get("id"),
            agent_id: row.get("agent_id"),
            capability_key: row.get("capability_key"),
            enabled: row.get("enabled"),
            config_json: parse_json_opt(row.get("config_json")),
        })
        .collect())
}

async fn channel_count(state: &AppState, tenant_id: &str, agent_id: &str) -> Result<u64> {
    let count: (i64,) = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_agent_channels WHERE tenant_id = ? AND agent_id = ?",
    )
    .bind(tenant_id)
    .bind(agent_id)
    .fetch_one(state.control_db())
    .await?;
    Ok(count.0.max(0) as u64)
}

async fn row_to_agent(
    state: &AppState,
    tenant_id: &str,
    row: sqlx::sqlite::SqliteRow,
) -> Result<BotAgentInfo> {
    let id: String = row.get("id");
    Ok(BotAgentInfo {
        id: id.clone(),
        tenant_id: row.get("tenant_id"),
        name: row.get("name"),
        description: row.get("description"),
        enabled: row.get("enabled"),
        default_capability: row.get("default_capability"),
        persona_prompt: row.get("persona_prompt"),
        capabilities: load_capabilities(state, tenant_id, &id).await?,
        channels_count: channel_count(state, tenant_id, &id).await?,
        created_by: row.get("created_by"),
        created_at: dt_to_rfc3339(&row, "created_at"),
        updated_at: dt_to_rfc3339(&row, "updated_at"),
    })
}

fn row_to_channel(row: sqlx::sqlite::SqliteRow) -> BotAgentChannelInfo {
    let inbound_secret: Option<String> = row.get("inbound_secret");
    let outbound_token: Option<String> = row.get("outbound_token");
    let signing_secret: Option<String> = row.get("signing_secret");
    let outbound_signing_secret: Option<String> = row.get("outbound_signing_secret");
    BotAgentChannelInfo {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        platform: row.get("platform"),
        name: row.get("name"),
        enabled: row.get("enabled"),
        inbound_mode: row.get("inbound_mode"),
        inbound_secret_set: inbound_secret.is_some_and(|v| !v.is_empty()),
        inbound_status: row.get("inbound_status"),
        inbound_error: row.get("inbound_error"),
        inbound_last_seen_at: opt_dt_to_rfc3339(&row, "inbound_last_seen_at"),
        inbound_last_message_at: opt_dt_to_rfc3339(&row, "inbound_last_message_at"),
        outbound_webhook_url: row.get("outbound_webhook_url"),
        outbound_token_set: outbound_token.is_some_and(|v| !v.is_empty()),
        signing_secret_set: signing_secret.is_some_and(|v| !v.is_empty()),
        outbound_signing_secret_set: outbound_signing_secret.is_some_and(|v| !v.is_empty()),
        config_json: parse_json_opt(row.get("config_json")),
        created_at: dt_to_rfc3339(&row, "created_at"),
        updated_at: dt_to_rfc3339(&row, "updated_at"),
    }
}

fn row_to_log(row: sqlx::sqlite::SqliteRow) -> BotMessageLogInfo {
    let content_json = parse_json_opt(row.get("content_json"));
    let agent_task_id = content_json
        .as_ref()
        .and_then(|value| {
            value
                .get("agentTaskId")
                .or_else(|| value.get("agent_task_id"))
        })
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    BotMessageLogInfo {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        channel_id: row.get("channel_id"),
        agent_task_id,
        direction: row.get("direction"),
        platform: row.get("platform"),
        external_user_id: row.get("external_user_id"),
        external_conversation_id: row.get("external_conversation_id"),
        message_type: row.get("message_type"),
        content_json,
        status: row.get("status"),
        queue_status: row.get("queue_status"),
        attempt_count: row.get("attempt_count"),
        max_attempts: row.get("max_attempts"),
        claimed_by: row.get("claimed_by"),
        claimed_at: opt_dt_to_rfc3339(&row, "claimed_at"),
        last_error: row.get("last_error"),
        finished_at: opt_dt_to_rfc3339(&row, "finished_at"),
        error_message: row.get("error_message"),
        created_at: dt_to_rfc3339(&row, "created_at"),
    }
}

fn payload_string_field(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload.get(*key).and_then(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| value.as_i64().map(|v| v.to_string()))
                .or_else(|| value.as_u64().map(|v| v.to_string()))
        })
    })
}

fn payload_text(payload: &Value) -> String {
    [
        payload.pointer("/text/content"),
        payload.pointer("/message/text"),
        payload.pointer("/data/text"),
        payload.pointer("/data/body"),
        payload.pointer("/textContent"),
        payload.pointer("/body/text"),
        payload.get("text"),
        payload.get("message"),
        payload.get("content"),
        payload.get("body"),
        payload.get("caption"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .unwrap_or_default()
    .trim()
    .to_string()
}

fn payload_mentions_bot(payload: &Value, agent_name: &str) -> bool {
    if payload
        .get("mentioned")
        .or_else(|| payload.get("at_bot"))
        .or_else(|| payload.get("is_at"))
        .or_else(|| payload.get("isInAtList"))
        .or_else(|| payload.get("is_at_bot"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    if payload
        .get("chat_type")
        .or_else(|| payload.get("chatType"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| {
            matches!(
                value.as_str(),
                "p2p" | "private" | "single" | "direct" | "direct_message"
            )
        })
    {
        return true;
    }
    let text = payload_text(payload);
    !agent_name.trim().is_empty() && text.contains(&format!("@{}", agent_name.trim()))
}

fn payload_sender_name(payload: &Value) -> Option<String> {
    payload
        .get("senderNick")
        .or_else(|| payload.get("sender_name"))
        .or_else(|| payload.get("senderName"))
        .or_else(|| payload.get("pushName"))
        .or_else(|| payload.get("fromName"))
        .or_else(|| payload.get("user_name"))
        .or_else(|| payload.get("username"))
        .or_else(|| payload.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn external_pairing_code(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let raw = if let Some(value) = lower.strip_prefix("bind ") {
        value
    } else if let Some(value) = lower.strip_prefix("/bind ") {
        value
    } else if let Some(value) = trimmed.strip_prefix("绑定 ") {
        value
    } else if let Some(value) = trimmed.strip_prefix("/绑定 ") {
        value
    } else {
        return None;
    };
    let code = raw
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    (matches!(code.len(), 8 | 12) && code.chars().all(|ch| ch.is_ascii_hexdigit())).then_some(code)
}

fn pairing_allowed_in_private_conversation(platform: &str, payload: &Value) -> bool {
    let raw = payload.get("platform_payload").unwrap_or(payload);
    let platform = platform.trim().to_ascii_lowercase();
    let explicit_type = first_json_string(
        raw,
        &[
            "/chat_type",
            "/chatType",
            "/conversation_type",
            "/conversationType",
            "/event/message/chat_type",
            "/event/chat_type",
            "/event/channel_type",
            "/channel_type",
            "/message/chat/type",
            "/chat/type",
            "/type",
        ],
    )
    .or_else(|| {
        payload_string_field(
            payload,
            &[
                "chat_type",
                "chatType",
                "conversation_type",
                "conversationType",
                "channel_type",
            ],
        )
    })
    .map(|value| value.trim().to_ascii_lowercase());
    if explicit_type.as_deref().is_some_and(|value| {
        matches!(
            value,
            "p2p" | "private" | "single" | "direct" | "direct_message" | "im"
        ) || ((platform == "dingtalk" || platform == "discord") && value == "1")
    }) {
        return true;
    }
    if explicit_type.as_deref().is_some_and(|value| {
        matches!(value, "group" | "channel" | "mpim" | "group_chat")
            || ((platform == "dingtalk" || platform == "discord") && value == "2")
    }) {
        return false;
    }

    match platform.as_str() {
        "feishu" | "lark" => raw
            .get("event")
            .and_then(|event| event.get("message").or(Some(event)))
            .is_some_and(|message| feishu_payload_is_direct_chat(raw, message)),
        "slack" => first_json_string(raw, &["/event/channel", "/channel"])
            .is_some_and(|channel| channel.starts_with('D')),
        "whatsapp" => raw
            .pointer("/entry/0/changes/0/value/messages/0/from")
            .and_then(Value::as_str)
            .is_some(),
        "wecom" => {
            first_json_string(raw, &["/RoomId", "/room_id"]).is_none()
                && first_json_string(
                    raw,
                    &["/FromUserName", "/from_user", "/fromUser", "/user_id"],
                )
                .is_some()
        }
        "discord" => {
            first_json_string(raw, &["/guild_id", "/data/guild_id"]).is_none()
                && first_json_string(raw, &["/author/id", "/member/user/id", "/user/id"]).is_some()
        }
        "telegram" => explicit_type.as_deref() == Some("private"),
        _ => false,
    }
}

fn external_message_id_from_payload(payload: &Value) -> Option<String> {
    let candidate = payload_string_field(
        payload,
        &[
            "message_id",
            "messageId",
            "msg_id",
            "msgId",
            "event_id",
            "eventId",
        ],
    )
    .or_else(|| {
        let raw = payload.get("platform_payload").unwrap_or(payload);
        first_json_string(
            raw,
            &[
                "/event/message/message_id",
                "/event/client_msg_id",
                "/event/event_ts",
                "/event/ts",
                "/event_id",
                "/entry/0/changes/0/value/messages/0/id",
                "/MsgId",
                "/msgid",
                "/msgId",
                "/message/msg_id",
                "/message/message_id",
                "/data/id",
                "/id",
            ],
        )
    })?;
    let candidate = candidate.trim();
    if candidate.is_empty() {
        None
    } else if candidate.chars().count() <= 200 {
        Some(candidate.to_string())
    } else {
        Some(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(candidate.as_bytes()))
        ))
    }
}

fn external_user_id_from_payload(payload: &Value) -> Option<String> {
    let candidate = payload_string_field(
        payload,
        &[
            "user_id",
            "userId",
            "sender_id",
            "senderId",
            "sender",
            "from",
            "open_id",
            "openId",
        ],
    )
    .or_else(|| {
        let raw = payload.get("platform_payload").unwrap_or(payload);
        first_json_string(
            raw,
            &[
                "/event/sender/sender_id/open_id",
                "/event/sender/sender_id/user_id",
                "/event/user",
                "/entry/0/changes/0/value/messages/0/from",
                "/FromUserName",
                "/from_user",
                "/author/id",
                "/member/user/id",
                "/message/from/id",
                "/from/id",
            ],
        )
    })?;
    let candidate = candidate.trim();
    if candidate.is_empty() {
        None
    } else if candidate.chars().count() <= 255 {
        Some(candidate.to_string())
    } else {
        Some(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(candidate.as_bytes()))
        ))
    }
}

fn external_conversation_id_from_payload(payload: &Value) -> Option<String> {
    let candidate = payload_string_field(
        payload,
        &[
            "conversation_id",
            "conversationId",
            "chat_id",
            "chatId",
            "room_id",
            "roomId",
            "group_id",
            "groupId",
            "thread_id",
            "threadId",
        ],
    )
    .or_else(|| {
        let raw = payload.get("platform_payload").unwrap_or(payload);
        first_json_string(
            raw,
            &[
                "/event/message/chat_id",
                "/event/channel",
                "/entry/0/changes/0/value/messages/0/from",
                "/RoomId",
                "/room_id",
                "/channel_id",
                "/message/chat/id",
                "/chat/id",
            ],
        )
    })?;
    let candidate = candidate.trim();
    (!candidate.is_empty() && candidate.chars().count() <= 255).then(|| candidate.to_string())
}

fn bot_message_idempotency_key(
    scope: &str,
    platform: &str,
    channel_id: &str,
    message_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [platform, channel_id, message_id] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("bot-{scope}:{}", hex::encode(hasher.finalize()))
}

fn value_string_field(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn bot_log_text(direction: &str, content: &Value) -> String {
    match direction {
        "outbound" => value_string_field(content, &["text", "message", "content", "body"])
            .or_else(|| {
                content.get("request_payload").and_then(|payload| {
                    value_string_field(payload, &["text", "message", "content", "body"])
                })
            })
            .unwrap_or_default(),
        _ => payload_text(content),
    }
    .trim()
    .to_string()
}

fn bot_context_env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn bot_context_max_history_messages() -> usize {
    bot_context_env_usize(
        "BOT_GATEWAY_CONTEXT_MAX_HISTORY_MESSAGES",
        BOT_CONTEXT_MAX_HISTORY_MESSAGES,
        4,
        100,
    )
}

fn bot_context_keep_recent_messages() -> usize {
    bot_context_env_usize(
        "BOT_GATEWAY_CONTEXT_KEEP_RECENT_MESSAGES",
        BOT_CONTEXT_KEEP_RECENT_MESSAGES,
        2,
        20,
    )
}

fn bot_context_summarize_threshold_chars() -> usize {
    bot_context_env_usize(
        "BOT_GATEWAY_CONTEXT_SUMMARIZE_THRESHOLD_CHARS",
        BOT_CONTEXT_SUMMARIZE_THRESHOLD_CHARS,
        3_000,
        80_000,
    )
}

fn bot_context_recent_budget_chars() -> usize {
    bot_context_env_usize(
        "BOT_GATEWAY_CONTEXT_RECENT_BUDGET_CHARS",
        BOT_CONTEXT_RECENT_BUDGET_CHARS,
        1_000,
        30_000,
    )
}

fn bot_context_summary_target_chars() -> usize {
    bot_context_env_usize(
        "BOT_GATEWAY_CONTEXT_SUMMARY_TARGET_CHARS",
        BOT_CONTEXT_SUMMARY_TARGET_CHARS,
        600,
        8_000,
    )
}

fn format_bot_context_messages(messages: &[BotConversationMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            let role = if message.direction == "outbound" {
                "assistant"
            } else {
                "user"
            };
            format!(
                "[{}][{}][{}] {}",
                message.created_at,
                role,
                message.status,
                message.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn trim_recent_context_by_budget(
    messages: &[BotConversationMessage],
    budget_chars: usize,
) -> Vec<BotConversationMessage> {
    let mut total = 0usize;
    let mut selected = Vec::new();
    for message in messages.iter().rev() {
        let chars = message.text.chars().count();
        if !selected.is_empty() && total.saturating_add(chars) > budget_chars {
            break;
        }
        total = total.saturating_add(chars);
        selected.push(message.clone());
    }
    selected.reverse();
    selected
}

fn clean_value_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| value.as_i64().map(|v| v.to_string()))
                .or_else(|| value.as_u64().map(|v| v.to_string()))
        })
        .and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
}

fn first_json_string(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| clean_value_string(payload.pointer(pointer)))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BotInboundAttachment {
    provider_file_id: Option<String>,
    provider_message_id: Option<String>,
    download_url: Option<String>,
    filename: String,
    media_type: String,
    size_bytes: Option<u64>,
    kind: String,
}

fn sanitized_bot_attachment_filename(value: Option<&str>, fallback: &str) -> String {
    let candidate = value
        .unwrap_or(fallback)
        .split(['/', '\\'])
        .next_back()
        .unwrap_or(fallback)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(255)
        .collect::<String>();
    let candidate = candidate.trim();
    if candidate.is_empty() {
        fallback.to_string()
    } else {
        candidate.to_string()
    }
}

fn bot_attachment_media_type(value: Option<&str>, filename: &str, kind: &str) -> String {
    let declared = value
        .and_then(|raw| raw.split(';').next())
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && *raw != "application/octet-stream")
        .map(str::to_ascii_lowercase);
    declared
        .or_else(|| {
            crate::routes::upload::media_type_for_filename(filename).map(ToString::to_string)
        })
        .unwrap_or_else(|| {
            if kind == "image" {
                "image/jpeg".to_string()
            } else {
                "text/plain".to_string()
            }
        })
}

fn attachment_from_value(
    value: &Value,
    provider_message_id: Option<&str>,
    default_kind: &str,
) -> Option<BotInboundAttachment> {
    let provider_file_id = first_json_string(
        value,
        &[
            "/id",
            "/file_id",
            "/fileId",
            "/file_key",
            "/fileKey",
            "/image_key",
        ],
    );
    let download_url = first_json_string(
        value,
        &[
            "/download_url",
            "/downloadUrl",
            "/url_private_download",
            "/url_private",
            "/url",
            "/proxy_url",
        ],
    );
    if provider_file_id.is_none() && download_url.is_none() {
        return None;
    }
    let raw_kind = first_json_string(value, &["/type", "/kind"])
        .unwrap_or_else(|| default_kind.to_string())
        .to_ascii_lowercase();
    let declared_media_type = first_json_string(
        value,
        &["/mimetype", "/mime_type", "/media_type", "/content_type"],
    );
    let declared_filename =
        first_json_string(value, &["/name", "/filename", "/file_name", "/title"]);
    let filename_suggests_image = declared_filename.as_deref().is_some_and(|filename| {
        matches!(
            filename
                .rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase())
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp")
        )
    });
    let kind = if raw_kind.contains("image")
        || raw_kind == "photo"
        || declared_media_type
            .as_deref()
            .is_some_and(|media_type| media_type.to_ascii_lowercase().starts_with("image/"))
        || filename_suggests_image
    {
        "image"
    } else {
        "document"
    };
    let fallback = if kind == "image" {
        "bot-image.jpg"
    } else {
        "bot-attachment.txt"
    };
    let filename = sanitized_bot_attachment_filename(declared_filename.as_deref(), fallback);
    let media_type = bot_attachment_media_type(declared_media_type.as_deref(), &filename, kind);
    let size_bytes = value
        .get("size")
        .or_else(|| value.get("file_size"))
        .or_else(|| value.get("size_bytes"))
        .and_then(Value::as_u64);
    Some(BotInboundAttachment {
        provider_file_id,
        provider_message_id: provider_message_id.map(ToString::to_string),
        download_url,
        filename,
        media_type,
        size_bytes,
        kind: kind.to_string(),
    })
}

fn bot_inbound_attachments(platform: &str, payload: &Value) -> Vec<BotInboundAttachment> {
    let raw = payload.get("platform_payload").unwrap_or(payload);
    let platform = platform.trim().to_ascii_lowercase();
    let message_id = external_message_id_from_payload(payload);
    let mut out = Vec::new();
    let mut extend_array = |value: Option<&Value>, kind: &str| {
        if let Some(items) = value.and_then(Value::as_array) {
            for item in items {
                if let Some(attachment) = attachment_from_value(item, message_id.as_deref(), kind) {
                    out.push(attachment);
                }
            }
        }
    };
    extend_array(raw.get("attachments"), "document");
    extend_array(raw.pointer("/event/files"), "document");
    extend_array(raw.pointer("/event/attachments"), "document");
    extend_array(raw.pointer("/message/attachments"), "document");

    match platform.as_str() {
        "slack" => extend_array(raw.pointer("/event/files"), "document"),
        "discord" => extend_array(raw.get("attachments"), "document"),
        "telegram" => {
            let message = raw.get("message").or_else(|| raw.get("channel_post"));
            if let Some(message) = message {
                for (key, kind) in [
                    ("document", "document"),
                    ("audio", "document"),
                    ("video", "document"),
                    ("voice", "document"),
                    ("animation", "document"),
                ] {
                    if let Some(value) = message.get(key) {
                        if let Some(attachment) =
                            attachment_from_value(value, message_id.as_deref(), kind)
                        {
                            out.push(attachment);
                        }
                    }
                }
                if let Some(photo) = message
                    .get("photo")
                    .and_then(Value::as_array)
                    .and_then(|items| items.last())
                {
                    if let Some(attachment) =
                        attachment_from_value(photo, message_id.as_deref(), "image")
                    {
                        out.push(attachment);
                    }
                }
            }
        }
        "whatsapp" => {
            if let Some(message) = raw.pointer("/entry/0/changes/0/value/messages/0") {
                for (key, kind) in [
                    ("image", "image"),
                    ("document", "document"),
                    ("audio", "document"),
                    ("video", "document"),
                    ("sticker", "image"),
                ] {
                    if let Some(value) = message.get(key) {
                        if let Some(attachment) =
                            attachment_from_value(value, message_id.as_deref(), kind)
                        {
                            out.push(attachment);
                        }
                    }
                }
            }
        }
        "feishu" | "lark" => {
            if let Some(message) = raw.pointer("/event/message") {
                let message_type = first_json_string(message, &["/message_type", "/msg_type"])
                    .unwrap_or_else(|| "document".to_string());
                if let Some(content) = message.get("content").and_then(Value::as_str) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(content) {
                        if let Some(attachment) = attachment_from_value(
                            &parsed,
                            message_id.as_deref(),
                            if message_type == "image" {
                                "image"
                            } else {
                                "document"
                            },
                        ) {
                            out.push(attachment);
                        }
                    }
                }
            }
        }
        "dingtalk" => {
            for pointer in ["/content", "/data/content", "/file", "/image"] {
                if let Some(value) = raw.pointer(pointer) {
                    if let Some(attachment) =
                        attachment_from_value(value, message_id.as_deref(), "document")
                    {
                        out.push(attachment);
                    }
                }
            }
        }
        "wecom" => {
            if let Some(media_id) = first_json_string(raw, &["/MediaId", "/media_id"]) {
                out.push(BotInboundAttachment {
                    provider_file_id: Some(media_id),
                    provider_message_id: message_id.clone(),
                    download_url: None,
                    filename: sanitized_bot_attachment_filename(
                        first_json_string(raw, &["/FileName", "/file_name"]).as_deref(),
                        "wecom-attachment.txt",
                    ),
                    media_type: "text/plain".to_string(),
                    size_bytes: None,
                    kind: "document".to_string(),
                });
            }
        }
        _ => {}
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|item| {
        let key = format!(
            "{}:{}:{}",
            item.provider_file_id.as_deref().unwrap_or_default(),
            item.download_url.as_deref().unwrap_or_default(),
            item.filename
        );
        seen.insert(key)
    });
    out.truncate(10);
    out
}

fn normalize_text_fields(
    adapter: &str,
    raw: &Value,
    text: String,
    user_id: Option<String>,
    conversation_id: Option<String>,
    sender_name: Option<String>,
    mentioned: bool,
) -> Value {
    let mut out = json!({
        "text": text,
        "platform_adapter": adapter,
        "platform_payload": raw,
    });
    let text = out
        .get("text")
        .cloned()
        .unwrap_or(Value::String(String::new()));
    out["message"] = text.clone();
    out["content"] = text.clone();
    out["body"] = text;
    if let Some(user_id) = user_id {
        out["user_id"] = Value::String(user_id.clone());
        out["sender_id"] = Value::String(user_id);
    }
    if let Some(conversation_id) = conversation_id {
        out["conversation_id"] = Value::String(conversation_id.clone());
        out["chat_id"] = Value::String(conversation_id);
    }
    if let Some(sender_name) = sender_name {
        out["sender_name"] = Value::String(sender_name);
    }
    if mentioned {
        out["mentioned"] = Value::Bool(true);
    }
    out
}

fn feishu_chat_type(raw: &Value) -> Option<String> {
    first_json_string(
        raw,
        &[
            "/event/message/chat_type",
            "/event/chat_type",
            "/message/chat_type",
            "/chat_type",
        ],
    )
    .map(|value| value.to_ascii_lowercase())
}

fn feishu_payload_is_direct_chat(raw: &Value, message: &Value) -> bool {
    if let Some(chat_type) = feishu_chat_type(raw) {
        return matches!(
            chat_type.as_str(),
            "p2p" | "private" | "single" | "direct" | "direct_message"
        );
    }
    if message
        .get("mentions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return false;
    }
    let has_group_hint = first_json_string(
        raw,
        &[
            "/event/message/open_chat_id",
            "/event/message/open_thread_id",
            "/event/message/thread_id",
            "/event/message/root_id",
            "/event/message/parent_id",
        ],
    )
    .is_some();
    !has_group_hint
}

fn flatten_feishu_rich_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    let blocks = value.as_array()?;
    for block in blocks {
        let Some(items) = block.as_array() else {
            continue;
        };
        for item in items {
            if let Some(text) = clean_value_string(item.get("text")) {
                parts.push(text);
            } else if let Some(name) = clean_value_string(item.get("name")) {
                parts.push(name);
            } else if let Some(href) = clean_value_string(item.get("href")) {
                parts.push(href);
            }
        }
        if !parts.last().is_some_and(|item| item == "\n") {
            parts.push("\n".to_string());
        }
    }
    let text = parts.join(" ").replace(" \n ", "\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn feishu_content_to_text(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        if let Some(text) = clean_value_string(parsed.get("text")) {
            return Some(text);
        }
        if let Some(text) = clean_value_string(parsed.get("content")) {
            return Some(text);
        }
        if let Some(text) = parsed
            .pointer("/post/zh_cn/content")
            .and_then(flatten_feishu_rich_text)
        {
            return Some(text);
        }
        if let Some(text) = parsed
            .pointer("/post/en_us/content")
            .and_then(flatten_feishu_rich_text)
        {
            return Some(text);
        }
    }
    Some(trimmed.to_string())
}

fn normalize_feishu_inbound_payload(raw: &Value) -> Option<Value> {
    let event = raw.get("event")?;
    let message = event.get("message").unwrap_or(event);
    let text = first_json_string(message, &["/text", "/body"])
        .or_else(|| first_json_string(raw, &["/text", "/message", "/content"]))
        .or_else(|| {
            message
                .get("content")
                .and_then(Value::as_str)
                .and_then(feishu_content_to_text)
        })?;
    let user_id = first_json_string(
        raw,
        &[
            "/event/sender/sender_id/open_id",
            "/event/sender/sender_id/user_id",
            "/event/sender/sender_id/union_id",
            "/event/sender/open_id",
            "/event/sender/user_id",
        ],
    );
    let conversation_id = first_json_string(
        raw,
        &[
            "/event/message/chat_id",
            "/event/message/open_chat_id",
            "/event/message/thread_id",
            "/event/message/message_id",
        ],
    );
    let sender_name = first_json_string(
        raw,
        &[
            "/event/sender/name",
            "/event/sender/sender_name",
            "/event/sender/tenant_key",
        ],
    );
    let mentioned = message
        .get("mentions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
        || feishu_payload_is_direct_chat(raw, message);
    Some(normalize_text_fields(
        "feishu_event",
        raw,
        text,
        user_id,
        conversation_id,
        sender_name,
        mentioned,
    ))
}

fn normalize_slack_inbound_payload(raw: &Value) -> Option<Value> {
    let event = raw.get("event").unwrap_or(raw);
    if clean_value_string(event.get("bot_id")).is_some()
        || clean_value_string(event.get("subtype")).as_deref() == Some("bot_message")
    {
        return None;
    }
    let text = first_json_string(event, &["/text"])?;
    let user_id = first_json_string(event, &["/user", "/actor_user_id"]);
    let conversation_id = first_json_string(event, &["/channel", "/conversation", "/team"]);
    let sender_name = first_json_string(event, &["/user_profile/real_name", "/user_profile/name"]);
    let mentioned = text.contains("<@");
    Some(normalize_text_fields(
        "slack_event",
        raw,
        text,
        user_id,
        conversation_id,
        sender_name,
        mentioned,
    ))
}

fn normalize_whatsapp_inbound_payload(raw: &Value) -> Option<Value> {
    let value = raw.pointer("/entry/0/changes/0/value")?;
    let message = value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|items| items.first())?;
    let text = first_json_string(
        message,
        &[
            "/text/body",
            "/button/text",
            "/interactive/button_reply/title",
            "/interactive/list_reply/title",
        ],
    )?;
    let user_id = first_json_string(message, &["/from"]);
    let conversation_id = user_id.clone().or_else(|| {
        first_json_string(
            value,
            &[
                "/metadata/phone_number_id",
                "/metadata/display_phone_number",
            ],
        )
    });
    let sender_name = value
        .get("contacts")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|contact| first_json_string(contact, &["/profile/name", "/wa_id"]));
    Some(normalize_text_fields(
        "whatsapp_cloud_event",
        raw,
        text,
        user_id,
        conversation_id,
        sender_name,
        false,
    ))
}

fn normalize_wecom_inbound_payload(raw: &Value) -> Option<Value> {
    let text = first_json_string(
        raw,
        &["/Content", "/content", "/text/content", "/message/text"],
    )?;
    let user_id = first_json_string(
        raw,
        &[
            "/FromUserName",
            "/from_user",
            "/fromUser",
            "/user_id",
            "/sender_id",
        ],
    );
    let conversation_id = first_json_string(
        raw,
        &[
            "/RoomId",
            "/room_id",
            "/conversation_id",
            "/chat_id",
            "/ToUserName",
        ],
    );
    let sender_name =
        first_json_string(raw, &["/sender_name", "/senderName", "/UserName", "/name"]);
    Some(normalize_text_fields(
        "wecom_json_event",
        raw,
        text,
        user_id,
        conversation_id,
        sender_name,
        false,
    ))
}

fn normalize_discord_inbound_payload(raw: &Value) -> Option<Value> {
    let text = first_json_string(raw, &["/content", "/data/content", "/message/content"])?;
    let user_id = first_json_string(raw, &["/author/id", "/member/user/id", "/user/id"]);
    let conversation_id = first_json_string(raw, &["/channel_id", "/guild_id"]);
    let sender_name = first_json_string(
        raw,
        &[
            "/author/username",
            "/member/user/username",
            "/user/username",
        ],
    );
    Some(normalize_text_fields(
        "discord_event",
        raw,
        text,
        user_id,
        conversation_id,
        sender_name,
        false,
    ))
}

fn normalize_platform_inbound_payload(platform: &str, payload: Value) -> Value {
    let normalized = match platform.trim().to_ascii_lowercase().as_str() {
        "feishu" | "lark" => normalize_feishu_inbound_payload(&payload),
        "slack" => normalize_slack_inbound_payload(&payload),
        "whatsapp" => normalize_whatsapp_inbound_payload(&payload),
        "wecom" => normalize_wecom_inbound_payload(&payload),
        "discord" => normalize_discord_inbound_payload(&payload),
        _ => None,
    };
    let attachments = bot_inbound_attachments(platform, &payload);
    let mut output = normalized.unwrap_or(payload);
    if !attachments.is_empty() {
        let fallback_text = format!(
            "请处理附件：{}",
            attachments
                .iter()
                .map(|item| item.filename.as_str())
                .collect::<Vec<_>>()
                .join("、")
        );
        if let Some(object) = output.as_object_mut() {
            object.insert("attachments".to_string(), json!(attachments));
            if payload_text(&Value::Object(object.clone())).is_empty() {
                object.insert("text".to_string(), Value::String(fallback_text.clone()));
                object.insert("message".to_string(), Value::String(fallback_text.clone()));
                object.insert("content".to_string(), Value::String(fallback_text.clone()));
                object.insert("body".to_string(), Value::String(fallback_text));
            }
        }
    }
    output
}

fn webhook_challenge_response(payload: &Value) -> Option<Value> {
    let challenge = payload
        .get("challenge")
        .or_else(|| payload.pointer("/event/challenge"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let kind = payload
        .get("type")
        .or_else(|| payload.get("event_type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        kind.as_str(),
        "url_verification" | "endpoint.url_validation" | "verification"
    ) || payload.get("challenge").is_some()
    {
        return Some(json!({ "challenge": challenge }));
    }
    None
}

fn config_bool(config: Option<&Value>, key: &str) -> bool {
    config
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn config_prefixes(config: Option<&Value>) -> Vec<String> {
    config
        .and_then(|value| value.get("trigger_prefixes"))
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

fn capability_allows_payload(
    capability: &BotAgentCapabilityInfo,
    mentioned: bool,
    default_if_missing: bool,
) -> bool {
    if !capability.enabled {
        return false;
    }
    let config = capability.config_json.as_ref();
    let requires_mention = config_bool(config, "require_mention");
    if requires_mention && !mentioned {
        return false;
    }
    // A configured prefix is an explicit gate. This also repairs legacy rows
    // whose fallback flag was previously overwritten to true on every save.
    if !config_prefixes(config).is_empty() {
        return false;
    }
    // A direct mention is itself an explicit trigger. Feishu private chats are
    // normalized as mentions, so `fallback_when_no_prefix = false` must not
    // disable a mention-only default capability.
    if requires_mention {
        return true;
    }
    if let Some(configured) = config
        .and_then(|value| value.get("fallback_when_no_prefix"))
        .and_then(Value::as_bool)
    {
        return configured;
    }
    default_if_missing && config_prefixes(config).is_empty()
}

fn select_capability_for_payload(
    capabilities: &[BotAgentCapabilityInfo],
    payload: &Value,
    agent_name: &str,
    default_capability: &str,
) -> Option<String> {
    let text = payload_text(payload);
    let mentioned = payload_mentions_bot(payload, agent_name);
    let normalized_text = text.trim_start();

    for capability in capabilities.iter().filter(|item| item.enabled) {
        let config = capability.config_json.as_ref();
        if config_bool(config, "require_mention") && !mentioned {
            continue;
        }
        let prefixes = config_prefixes(config);
        if prefixes
            .iter()
            .any(|prefix| normalized_text.starts_with(prefix))
        {
            return Some(capability.capability_key.clone());
        }
    }

    if let Some(router) = capabilities
        .iter()
        .find(|item| item.enabled && item.capability_key == "aos_router")
    {
        if capability_allows_payload(router, mentioned, false) {
            return Some("aos_router".to_string());
        }
    }

    for capability in capabilities.iter().filter(|item| item.enabled) {
        let config = capability.config_json.as_ref();
        if config_bool(config, "require_mention") && !mentioned {
            continue;
        }
        if config_prefixes(config).is_empty() && config_bool(config, "fallback_when_no_prefix") {
            return Some(capability.capability_key.clone());
        }
    }

    capabilities
        .iter()
        .find(|item| {
            item.capability_key == default_capability
                && capability_allows_payload(item, mentioned, true)
        })
        .map(|item| item.capability_key.clone())
}

fn strip_selected_trigger_prefix(
    capabilities: &[BotAgentCapabilityInfo],
    selected_capability: &str,
    text: &str,
    agent_name: &str,
) -> String {
    let mut cleaned = text.trim().to_string();
    if !agent_name.trim().is_empty() {
        let mention = format!("@{}", agent_name.trim());
        if cleaned.starts_with(&mention) {
            cleaned = cleaned[mention.len()..].trim_start().to_string();
        }
    }

    let Some(capability) = capabilities
        .iter()
        .find(|item| item.enabled && item.capability_key == selected_capability)
    else {
        return cleaned;
    };
    for prefix in config_prefixes(capability.config_json.as_ref()) {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            continue;
        }
        if cleaned.starts_with(prefix) {
            return cleaned[prefix.len()..]
                .trim_start_matches(|ch: char| matches!(ch, ' ' | '：' | ':' | '-' | '，' | ','))
                .trim()
                .to_string();
        }
    }
    cleaned
}

async fn update_inbound_log_status(
    state: &AppState,
    tenant_id: &str,
    log_id: &str,
    status: &str,
    error_message: Option<&str>,
) {
    if let Err(error) = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE bot_message_logs
        SET status = ?, error_message = ?
        WHERE tenant_id = ? AND id = ?
          AND status NOT IN ('cancelling','cancelled')
          AND queue_status NOT IN ('cancelling','cancelled')
        ",
    )
    .bind(status)
    .bind(error_message)
    .bind(tenant_id)
    .bind(log_id)
    .execute(state.control_db())
    .await
    {
        tracing::warn!(
            tenant_id = %tenant_id,
            log_id = %log_id,
            status = %status,
            "failed to update bot inbound log status: {}",
            error
        );
    }
}

async fn load_bot_conversation_messages(
    state: &AppState,
    tenant_id: &str,
    agent_id: &str,
    channel_id: &str,
    external_conversation_id: Option<&str>,
    exclude_log_id: &str,
) -> Result<Vec<BotConversationMessage>> {
    let Some(external_conversation_id) =
        external_conversation_id.filter(|value| !value.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    let limit = bot_context_max_history_messages() as i64;
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, direction, status, CAST(content_json AS TEXT) AS content_json, created_at
        FROM bot_message_logs
        WHERE tenant_id = ?
          AND agent_id = ?
          AND channel_id = ?
          AND external_conversation_id = ?
          AND id <> ?
        ORDER BY created_at DESC, id DESC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(agent_id)
    .bind(channel_id)
    .bind(external_conversation_id)
    .bind(exclude_log_id)
    .bind(limit)
    .fetch_all(state.control_db())
    .await?;

    let mut messages = rows
        .into_iter()
        .filter_map(|row| {
            let direction: String = row.get("direction");
            let content = parse_json_opt(row.get("content_json"))?;
            let text = bot_log_text(&direction, &content);
            if text.trim().is_empty() {
                return None;
            }
            Some(BotConversationMessage {
                direction,
                status: row.get("status"),
                text,
                created_at: dt_to_rfc3339(&row, "created_at"),
            })
        })
        .collect::<Vec<_>>();
    messages.reverse();
    Ok(messages)
}

#[cfg(feature = "nl2sql")]
async fn load_latest_bot_conversation_state(
    state: &AppState,
    tenant_id: &str,
    agent_id: &str,
    channel_id: &str,
    external_conversation_id: Option<&str>,
    capability_key: &str,
) -> Result<Option<Value>> {
    let Some(external_conversation_id) =
        external_conversation_id.filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT CAST(content_json AS TEXT) AS content_json
        FROM bot_message_logs
        WHERE tenant_id = ?
          AND agent_id = ?
          AND channel_id = ?
          AND external_conversation_id = ?
          AND direction = 'outbound'
          AND status = 'sent'
        ORDER BY created_at DESC, id DESC
        LIMIT 1
        ",
    )
    .bind(tenant_id)
    .bind(agent_id)
    .bind(channel_id)
    .bind(external_conversation_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let content = parse_json_opt(row.get("content_json"));
    let state = content
        .as_ref()
        .and_then(|value| value.get("conversationState"))
        .filter(|value| {
            value
                .get("capability")
                .and_then(Value::as_str)
                .is_some_and(|value| value == capability_key)
        })
        .cloned();
    Ok(state)
}

async fn attach_outbound_conversation_state(
    state: &AppState,
    tenant_id: &str,
    outbound_log_id: &str,
    conversation_state: &Value,
) -> Result<()> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT CAST(content_json AS TEXT) AS content_json FROM bot_message_logs WHERE tenant_id = ? AND id = ?",
    )
    .bind(tenant_id)
    .bind(outbound_log_id)
    .fetch_optional(state.control_db())
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    let mut content = parse_json_opt(row.get("content_json")).unwrap_or_else(|| json!({}));
    if let Some(object) = content.as_object_mut() {
        object.insert("conversationState".to_string(), conversation_state.clone());
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE bot_message_logs SET content_json = ? WHERE tenant_id = ? AND id = ?",
    )
    .bind(content)
    .bind(tenant_id)
    .bind(outbound_log_id)
    .execute(state.control_db())
    .await?;
    Ok(())
}

async fn summarize_bot_conversation_messages(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    capability_key: &str,
    agent_id: &str,
    agent_name: &str,
    messages: &[BotConversationMessage],
) -> Result<String> {
    let started = Instant::now();
    let target_chars = bot_context_summary_target_chars();
    let transcript = format_bot_context_messages(messages);
    let prompt = format!(
        "你正在为 AOS Bot 网关压缩外部聊天会话历史。\n\
         绑定能力: {capability_key}\n\
         Agent ID: {agent_id}\n\
         机器人名称: {agent_name}\n\
         目标: 用不超过 {target_chars} 个中文字符保留后续追问必须知道的上下文。\n\
         要求: 保留用户目标、已确认事实、关键约束、待办/未解决问题、最近结论；删除寒暄、重复和无关实现细节；不要编造。\n\n\
         会话历史:\n{transcript}\n\n\
         请输出压缩摘要:"
    );
    let result = crate::routes::pm::run_pm_chat_completion(
        state,
        tenant_id,
        user_id,
        state.default_model.clone(),
        vec![crate::routes::chat::ChatMessage {
            role: "user".to_string(),
            content: Value::String(prompt),
        }],
    )
    .await?;
    tracing::info!(
        tenant_id = %tenant_id,
        user_id = %user_id,
        capability = %capability_key,
        provider = %result.provider_name,
        model = %result.usage.model,
        summarized_messages = messages.len(),
        input_tokens = result.usage.input_tokens,
        output_tokens = result.usage.output_tokens,
        total_tokens = result.usage.total_tokens,
        summary_chars = result.answer.chars().count(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "bot conversation context summarized"
    );
    let usage_record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        session_id: "bot-gateway-context-summary".to_string(),
        request_id: None,
        model: result.usage.model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        total_tokens: result.usage.total_tokens,
        estimated_cost_usd: result.usage.estimated_cost_usd,
        api_key_id: Some(result.api_key_id),
        provider: result.provider_name,
        created_at: Utc::now(),
    };
    if let Err(error) = state.usage_writer().write(&usage_record).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            "failed to record bot context summary token usage: {}",
            error
        );
    }
    Ok(result.answer.trim().to_string())
}

async fn build_bot_conversation_context(
    state: &AppState,
    job: &InboundExecutionJob,
) -> Result<BotConversationContext> {
    let messages = load_bot_conversation_messages(
        state,
        &job.tenant_id,
        &job.agent_id,
        &job.channel_id,
        job.external_conversation_id.as_deref(),
        &job.inbound_log_id,
    )
    .await?;
    let total_chars = messages
        .iter()
        .map(|message| message.text.chars().count())
        .sum::<usize>();
    if messages.is_empty() {
        return Ok(BotConversationContext::default());
    }

    let threshold = bot_context_summarize_threshold_chars();
    let keep_recent = bot_context_keep_recent_messages().min(messages.len());
    if total_chars <= threshold {
        let loaded_messages = messages.len();
        return Ok(BotConversationContext {
            summary: None,
            recent_messages: messages,
            loaded_messages,
            summarized_messages: 0,
            total_chars,
        });
    }

    let split_at = messages.len().saturating_sub(keep_recent);
    let summary = if split_at > 0 {
        Some(
            summarize_bot_conversation_messages(
                state,
                &job.tenant_id,
                &job.actor_user_id,
                &job.selected_capability,
                &job.agent_id,
                &job.agent_name,
                &messages[..split_at],
            )
            .await?,
        )
    } else {
        None
    };
    let recent_messages =
        trim_recent_context_by_budget(&messages[split_at..], bot_context_recent_budget_chars());
    Ok(BotConversationContext {
        summary,
        recent_messages,
        loaded_messages: messages.len(),
        summarized_messages: split_at,
        total_chars,
    })
}

fn unsupported_capability_reply(capability_key: &str) -> String {
    format!(
        "AOS Bot 网关已收到消息，但能力 `{capability_key}` 的完整入站执行 adapter 还没有接入。当前已支持：AI 对话、通用 AI、产运短答/深度分析、超级对抗创建、看门狗、RD 研发任务、数据探索。"
    )
}

fn format_pm_assistant_short_answer(answer: &str) -> String {
    format!(
        "产运助手 · 短答（LLM）\n未执行深度检索；如需来源和证据，请在 Bot 能力配置中开启“深度分析”。\n\n{}",
        answer.trim()
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_aos_router_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    agent_task_id: &str,
    agent_id: &str,
    agent_name: &str,
    persona_prompt: Option<&str>,
    sender_name: Option<&str>,
    context: &BotConversationContext,
    router_config: Option<&Value>,
    capabilities: &[BotAgentCapabilityInfo],
    external_conversation_id: Option<&str>,
    external_channel_id: &str,
    user_text: &str,
    attachments: &[StagedBotAttachment],
) -> Result<BotCapabilityOutput> {
    let available = router_enabled_capabilities(capabilities);
    if available.is_empty() {
        return Ok(BotCapabilityOutput::answer(
            "智能分发没有可用目标能力。请先在 Bot 里绑定 AI 对话、产运助手、代码开发、数据探索、超级对抗或看门狗。".to_string(),
        ));
    }
    let threshold = bot_router_confidence_threshold(router_config);
    let llm_intent =
        parse_router_intent_with_llm(state, tenant_id, user_text, &available, router_config).await;
    let rule_target = rule_router_target(user_text, &available);
    let target = llm_intent
        .as_ref()
        .and_then(|intent| intent.target_capability.as_deref())
        .and_then(normalize_router_capability)
        .filter(|target| available.iter().any(|item| item.capability_key == *target))
        .map(ToOwned::to_owned)
        .or_else(|| rule_target.clone())
        .or_else(|| {
            available
                .iter()
                .find(|item| item.capability_key == "ai_chat")
                .or_else(|| {
                    available
                        .iter()
                        .find(|item| item.capability_key == "generic_ai")
                })
                .map(|item| item.capability_key.clone())
        });
    let Some(target) = target else {
        return Ok(BotCapabilityOutput::answer(
            "智能分发无法找到合适的目标能力，请绑定至少一个可执行菜单能力。".to_string(),
        ));
    };
    let confidence = llm_intent
        .as_ref()
        .and_then(|intent| intent.confidence)
        .unwrap_or_else(|| {
            if rule_target.as_deref() == Some(target.as_str()) {
                0.75
            } else {
                0.60
            }
        });
    if llm_intent
        .as_ref()
        .and_then(|intent| intent.needs_clarification)
        .unwrap_or(false)
        || confidence < threshold
    {
        let question = llm_intent
            .as_ref()
            .and_then(|intent| intent.clarification_question.as_deref())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("我不确定应该交给哪个能力处理。你希望我用 AI 对话、产运助手、代码开发、数据探索、超级对抗还是看门狗？");
        let _ = agent_ops::add_event(
            state,
            tenant_id,
            agent_task_id,
            "router.clarification_required",
            Some(agent_ops::PHASE_CAPABILITY_MATCHING),
            Some(agent_ops::STATUS_WAITING_INPUT),
            "warn",
            "智能分发置信度不足，等待用户澄清",
            Some(json!({
                "targetCapability": target,
                "confidence": confidence,
                "threshold": threshold,
                "reason": llm_intent.as_ref().and_then(|intent| intent.reason.clone()),
            })),
        )
        .await;
        return Ok(BotCapabilityOutput::answer(question.to_string())
            .with_output(json!({
                "executionPath": "aos_router",
                "router": {
                    "status": "clarification_required",
                    "targetCapability": target,
                    "confidence": confidence,
                    "threshold": threshold,
                }
            }))
            .waiting_for_input());
    }

    let delegated_prompt = llm_intent
        .as_ref()
        .and_then(|intent| intent.rewritten_prompt.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(user_text);
    let target_config = capability_config(capabilities, &target);
    let _ = agent_ops::add_event(
        state,
        tenant_id,
        agent_task_id,
        "router.decision",
        Some(agent_ops::PHASE_CAPABILITY_MATCHING),
        Some(agent_ops::STATUS_RUNNING),
        "info",
        &format!("智能分发已选择 {}", target),
        Some(json!({
            "targetCapability": target,
            "confidence": confidence,
            "threshold": threshold,
            "reason": llm_intent.as_ref().and_then(|intent| intent.reason.clone()),
            "availableCapabilities": available.iter().map(|item| item.capability_key.as_str()).collect::<Vec<_>>(),
        })),
    )
    .await;
    let mut output = Box::pin(run_text_capability(
        state,
        tenant_id,
        user_id,
        agent_task_id,
        &target,
        agent_id,
        agent_name,
        persona_prompt,
        sender_name,
        context,
        target_config,
        capabilities,
        external_conversation_id,
        external_channel_id,
        delegated_prompt,
        attachments,
    ))
    .await?;
    let router_output = json!({
        "executionPath": "aos_router",
        "router": {
            "targetCapability": target,
            "confidence": confidence,
            "threshold": threshold,
            "reason": llm_intent.as_ref().and_then(|intent| intent.reason.clone()),
        },
        "targetOutput": output.output_json.take(),
    });
    output.output_json = Some(router_output);
    Ok(output)
}

#[derive(Debug, Clone)]
struct BotCapabilityOutput {
    answer: String,
    linked_resource_type: Option<String>,
    linked_resource_id: Option<String>,
    output_json: Option<Value>,
    conversation_state: Option<Value>,
    keep_agent_task_open: bool,
    waiting_for_input: bool,
    monitor: Option<BotCapabilityMonitor>,
}

#[derive(Debug, Clone)]
enum BotCapabilityMonitor {
    SuperAssistant {
        session_id: String,
        turn_id: String,
    },
    SuperAdversarial {
        run_id: String,
    },
    PmResearch {
        task_id: String,
    },
    #[cfg(feature = "rd")]
    LinkedResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BotExecutionMode {
    Sync,
    Async,
    Hybrid,
    Clarification,
}

impl BotExecutionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
            Self::Hybrid => "hybrid",
            Self::Clarification => "clarification",
        }
    }
}

#[derive(Debug, Clone)]
struct BotExecutionPolicy {
    mode: BotExecutionMode,
    sync_timeout_ms: u64,
    ack_timeout_ms: u64,
    first_ack: bool,
    final_push: bool,
}

impl BotExecutionPolicy {
    fn for_capability(capability_key: &str, config: Option<&Value>) -> Self {
        let mut policy = match capability_key {
            "rd_agent" | "super_adversarial" => Self {
                mode: BotExecutionMode::Async,
                sync_timeout_ms: 0,
                ack_timeout_ms: 1_500,
                first_ack: true,
                final_push: true,
            },
            "pm_assistant" if pm_assistant_deep_mode_enabled(config) => Self {
                mode: BotExecutionMode::Async,
                sync_timeout_ms: 0,
                ack_timeout_ms: 1_500,
                first_ack: true,
                final_push: true,
            },
            "nl2sql" => Self {
                mode: BotExecutionMode::Clarification,
                sync_timeout_ms: 15_000,
                ack_timeout_ms: 1_500,
                first_ack: false,
                final_push: false,
            },
            "watchdog" => Self {
                mode: BotExecutionMode::Sync,
                sync_timeout_ms: 8_000,
                ack_timeout_ms: 0,
                first_ack: false,
                final_push: false,
            },
            _ => Self {
                mode: BotExecutionMode::Hybrid,
                sync_timeout_ms: 15_000,
                ack_timeout_ms: 1_500,
                first_ack: false,
                final_push: false,
            },
        };
        if let Some(mode) = config_string(config, &["executionMode", "execution_mode"]) {
            policy.mode = match mode.trim().to_ascii_lowercase().as_str() {
                "sync" => BotExecutionMode::Sync,
                "async" => BotExecutionMode::Async,
                "clarification" | "multi_turn" | "multi-turn" => BotExecutionMode::Clarification,
                _ => BotExecutionMode::Hybrid,
            };
            policy.first_ack = matches!(policy.mode, BotExecutionMode::Async);
            policy.final_push = matches!(policy.mode, BotExecutionMode::Async);
        }
        if let Some(value) = config_u64_opt(config, &["syncTimeoutMs", "sync_timeout_ms"]) {
            policy.sync_timeout_ms = value.clamp(1_000, 120_000);
        }
        if let Some(value) = config_u64_opt(config, &["ackTimeoutMs", "ack_timeout_ms"]) {
            policy.ack_timeout_ms = value.clamp(0, 30_000);
        }
        policy
    }
}

impl BotCapabilityOutput {
    fn answer(answer: String) -> Self {
        Self {
            answer,
            linked_resource_type: None,
            linked_resource_id: None,
            output_json: None,
            conversation_state: None,
            keep_agent_task_open: false,
            waiting_for_input: false,
            monitor: None,
        }
    }

    fn with_link(mut self, resource_type: &str, resource_id: &str) -> Self {
        self.linked_resource_type = Some(resource_type.to_string());
        self.linked_resource_id = Some(resource_id.to_string());
        self
    }

    fn with_output(mut self, output_json: Value) -> Self {
        self.output_json = Some(output_json);
        self
    }

    #[cfg(feature = "nl2sql")]
    fn with_conversation_state(mut self, state: Value) -> Self {
        self.conversation_state = Some(state);
        self
    }

    fn keep_open(mut self) -> Self {
        self.keep_agent_task_open = true;
        self
    }

    fn waiting_for_input(mut self) -> Self {
        self.keep_agent_task_open = true;
        self.waiting_for_input = true;
        self
    }

    fn with_monitor(mut self, monitor: BotCapabilityMonitor) -> Self {
        self.monitor = Some(monitor);
        self
    }

    fn with_execution_policy(mut self, policy: &BotExecutionPolicy) -> Self {
        let policy_json = json!({
            "mode": policy.mode.as_str(),
            "syncTimeoutMs": policy.sync_timeout_ms,
            "ackTimeoutMs": policy.ack_timeout_ms,
            "firstAck": policy.first_ack,
            "finalPush": policy.final_push,
        });
        match self.output_json.as_mut() {
            Some(Value::Object(object)) => {
                object.insert("executionPolicy".to_string(), policy_json);
            }
            Some(existing) => {
                let previous = std::mem::replace(existing, Value::Null);
                self.output_json = Some(json!({
                    "value": previous,
                    "executionPolicy": policy_json,
                }));
            }
            None => {
                self.output_json = Some(json!({ "executionPolicy": policy_json }));
            }
        }
        self
    }
}

fn build_bot_prompt(
    capability_key: &str,
    agent_name: &str,
    persona_prompt: Option<&str>,
    sender_name: Option<&str>,
    context: &BotConversationContext,
    user_text: &str,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("你正在通过 AOS Bot 网关回复外部聊天平台用户。\n");
    prompt.push_str("要求：直接回答用户问题，保持专业、清晰、可执行；不要暴露内部路由、日志、token 或系统实现细节。\n");
    prompt.push_str("如果用户问题需要实时外部资料但当前机器人入口没有检索证据，请明确说明需要联网核验，不要编造。\n");
    if user_text.trim_start().starts_with('$') {
        prompt.push_str("用户显式调用了 AOS 已安装 Skill。必须先调用名称对应的 skill__<name>__invoke 工具并遵循返回内容；禁止在 workspace、~/.claude、~/.codex 或其它宿主机路径中搜索 SKILL.md。\n");
    }
    prompt.push_str(&format!("当前能力: {capability_key}\n"));
    if !agent_name.trim().is_empty() {
        prompt.push_str(&format!("机器人名称: {}\n", agent_name.trim()));
    }
    if let Some(sender_name) = sender_name {
        prompt.push_str(&format!("用户昵称: {sender_name}\n"));
    }
    if let Some(persona) = persona_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        prompt.push_str("\n机器人提示词:\n");
        prompt.push_str(persona);
        prompt.push('\n');
    }
    if context.summary.is_some() || !context.recent_messages.is_empty() {
        prompt.push_str("\n同一外部会话的历史上下文:\n");
        if let Some(summary) = context
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            prompt.push_str("[压缩摘要]\n");
            prompt.push_str(summary);
            prompt.push('\n');
        }
        if !context.recent_messages.is_empty() {
            prompt.push_str("[最近消息]\n");
            prompt.push_str(&format_bot_context_messages(&context.recent_messages));
            prompt.push('\n');
        }
    }
    prompt.push_str("\n用户消息:\n");
    prompt.push_str(user_text.trim());
    prompt
}

fn capability_config<'a>(
    capabilities: &'a [BotAgentCapabilityInfo],
    capability_key: &str,
) -> Option<&'a Value> {
    capabilities
        .iter()
        .find(|item| item.enabled && item.capability_key == capability_key)
        .and_then(|item| item.config_json.as_ref())
}

fn config_string(config: Option<&Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn config_bool_opt(config: Option<&Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(Value::as_bool)
    })
}

fn config_bool_any(config: Option<&Value>, keys: &[&str], default_value: bool) -> bool {
    config_bool_opt(config, keys).unwrap_or(default_value)
}

fn config_string_vec(config: Option<&Value>, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(value) = config.and_then(|value| value.get(*key)) else {
            continue;
        };
        if let Some(array) = value.as_array() {
            return array
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }
        if let Some(raw) = value.as_str() {
            return raw
                .split([',', '，', ' ', '\n', '\t'])
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }
    }
    Vec::new()
}

#[cfg(feature = "nl2sql")]
fn config_string_vec_or_single(config: Option<&Value>, keys: &[&str]) -> Vec<String> {
    let values = config_string_vec(config, keys);
    if !values.is_empty() {
        return values;
    }
    config_string(config, keys)
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn config_u32_opt(config: Option<&Value>, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        let value = config.and_then(|value| value.get(*key))?;
        if let Some(num) = value.as_u64() {
            return u32::try_from(num).ok();
        }
        value.as_str()?.trim().parse::<u32>().ok()
    })
}

fn config_u64_opt(config: Option<&Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        let value = config.and_then(|value| value.get(*key))?;
        value
            .as_u64()
            .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
    })
}

fn pm_assistant_deep_mode_enabled(config: Option<&Value>) -> bool {
    if config_bool_any(
        config,
        &[
            "deepAnalysis",
            "deep_analysis",
            "enableDeepAnalysis",
            "enable_deep_analysis",
            "async",
            "asyncTask",
            "async_task",
        ],
        false,
    ) {
        return true;
    }
    config_string(config, &["mode", "executionMode", "execution_mode"])
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "deep" | "research" | "async" | "deep_analysis" | "deep-analysis"
            )
        })
        .unwrap_or(false)
}

#[cfg(feature = "nl2sql")]
async fn load_user_role_for_bot_actor(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
) -> Result<String> {
    let role = sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT role FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?;
    Ok(role.unwrap_or_else(|| "developer".to_string()))
}

async fn run_rd_agent_capability_output(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    agent_task_id: &str,
    capability_config: Option<&Value>,
    prompt: String,
) -> Result<BotCapabilityOutput> {
    #[cfg(feature = "rd")]
    {
        let claims = Claims::new(user_id, "bot-gateway@local", "admin", tenant_id);
        let result = crate::routes::rd::create_task_from_bot(
            state.clone(),
            claims,
            crate::routes::rd::RdBotTaskCreateInput {
                prompt,
                agent_task_id: Some(agent_task_id.to_string()),
                repository_id: config_string(capability_config, &["repositoryId", "repository_id"]),
                agent_profile_id: config_string(
                    capability_config,
                    &["agentProfileId", "agent_profile_id"],
                ),
                workflow_id: config_string(capability_config, &["workflowId", "workflow_id"]),
                mode: config_string(capability_config, &["mode"]),
                context_depth: config_string(capability_config, &["contextDepth", "context_depth"]),
                should_deep_scan: config_bool_opt(
                    capability_config,
                    &["shouldDeepScan", "should_deep_scan"],
                ),
                model: config_string(capability_config, &["model"]),
            },
        )
        .await?;
        let answer = format!(
            "已创建 RD 任务：{}\n任务ID：{}\n状态：{}\n模式：{}\n你可以在 AOS 研发菜单中查看任务进度和结果，WatchDog 也能查询它卡在哪个阶段。",
            result.title, result.id, result.status, result.mode
        );
        Ok(BotCapabilityOutput::answer(answer)
            .with_link("rd_task", &result.id)
            .with_output(json!({
                "taskId": result.id,
                "title": result.title,
                "status": result.status,
                "mode": result.mode,
            }))
            .keep_open()
            .with_monitor(BotCapabilityMonitor::LinkedResource))
    }
    #[cfg(not(feature = "rd"))]
    {
        let _ = (
            state,
            tenant_id,
            user_id,
            agent_task_id,
            capability_config,
            prompt,
        );
        Err(AppError::ValidationError(
            "rd_agent requires the web-server rd feature to be enabled".to_string(),
        ))
    }
}

async fn run_pm_research_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    capability_config: Option<&Value>,
    prompt: String,
) -> Result<BotCapabilityOutput> {
    #[cfg(feature = "agent")]
    {
        let claims = Claims::new(user_id, "bot-gateway@local", "admin", tenant_id);
        let result = crate::routes::agent::start_pm_research_task_from_bot(
            state,
            claims,
            crate::routes::agent::PmResearchBotTaskInput {
                message: prompt,
                visible_message: None,
                model: config_string(capability_config, &["model"]),
                session_id: config_string(
                    capability_config,
                    &["sessionId", "session_id", "pmSessionId", "pm_session_id"],
                ),
            },
        )
        .await?;
        let answer = format!(
            "已创建产运深度分析任务。\ntask_id：{}\nsession_id：{}\n状态：{}\n完成后会主动推送结果；你也可以通过 WatchDog 查询进度。",
            result.task_id, result.session_id, result.status
        );
        Ok(BotCapabilityOutput::answer(answer)
            .with_link("pm_research_task", &result.task_id)
            .with_output(json!({
                "taskId": result.task_id,
                "sessionId": result.session_id,
                "status": result.status,
            }))
            .keep_open()
            .with_monitor(BotCapabilityMonitor::PmResearch {
                task_id: result.task_id,
            }))
    }
    #[cfg(not(feature = "agent"))]
    {
        let _ = (state, tenant_id, user_id, capability_config, prompt);
        Err(AppError::ValidationError(
            "pm_assistant deep analysis requires the web-server agent feature to be enabled"
                .to_string(),
        ))
    }
}

async fn run_super_adversarial_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    capability_config: Option<&Value>,
    prompt: String,
) -> Result<BotCapabilityOutput> {
    #[cfg(feature = "agent")]
    {
        let mut models = config_string_vec(capability_config, &["models"]);
        if models.is_empty() {
            models =
                crate::routes::agent::default_chat_adversarial_models(state, tenant_id).await?;
        }
        let claims = Claims::new(user_id, "bot-gateway@local", "admin", tenant_id);
        let result = crate::routes::agent::start_chat_adversarial_run_from_bot(
            state.clone(),
            claims,
            crate::routes::agent::ChatAdversarialBotRunInput {
                question: prompt,
                models: models.clone(),
                max_rounds: config_u32_opt(capability_config, &["maxRounds", "max_rounds"]),
                parent_run_id: config_string(capability_config, &["parentRunId", "parent_run_id"]),
                session_id: None,
                evidence_search_required: false,
                evidence_search_query: None,
            },
        )
        .await?;
        let answer = format!(
            "已创建超级对抗任务。\nrun_id：{}\nthread_id：{}\n状态：{}\n轮次：{}/{}\n完成后可在 AOS 超级对抗页面查看 final answer；WatchDog 也能查询进度。",
            result.id,
            result.thread_id.as_deref().unwrap_or("-"),
            result.status,
            result.current_round,
            result.max_rounds
        );
        Ok(BotCapabilityOutput::answer(answer)
            .with_link("chat_adversarial_run", &result.id)
            .with_output(json!({
                "runId": result.id,
                "threadId": result.thread_id,
                "status": result.status,
                "models": models,
                "currentRound": result.current_round,
                "maxRounds": result.max_rounds,
            }))
            .keep_open()
            .with_monitor(BotCapabilityMonitor::SuperAdversarial { run_id: result.id }))
    }
    #[cfg(not(feature = "agent"))]
    {
        let _ = (state, tenant_id, user_id, capability_config, prompt);
        Err(AppError::ValidationError(
            "super_adversarial requires the web-server agent feature to be enabled".to_string(),
        ))
    }
}

async fn run_watchdog_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    capability_config: Option<&Value>,
    external_conversation_id: Option<&str>,
    external_channel_id: &str,
    agent_task_id: &str,
    user_text: &str,
) -> Result<BotCapabilityOutput> {
    let scope = config_string(capability_config, &["watchdogScope", "watchdog_scope"])
        .unwrap_or_else(|| "conversation".to_string());
    let allowed_actions = config_string_vec(capability_config, &["allowActions", "allow_actions"]);
    let allowed_actions = allowed_actions
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let answer = agent_ops::ask_watchdog_text_with_actions_and_audit(
        state,
        tenant_id,
        user_text,
        Some(&scope),
        None,
        Some(external_channel_id),
        external_conversation_id,
        &allowed_actions,
        Some(agent_ops::WatchDogActionAudit::bot(
            user_id,
            None,
            Some(external_channel_id),
            external_conversation_id,
        )),
    )
    .await?;
    Ok(BotCapabilityOutput::answer(answer).with_output(json!({
        "agentTaskId": agent_task_id,
        "scope": "own",
        "externalConversationId": external_conversation_id,
        "externalChannelId": external_channel_id,
    })))
}

#[cfg(feature = "nl2sql")]
fn format_nl2sql_bot_answer(resp: &crate::routes::nl2sql::AgentExecuteResponse) -> String {
    let mut lines = Vec::new();
    lines.push("数据探索已完成。".to_string());
    if let Some(query_id) = resp.query_id.as_deref() {
        lines.push(format!("query_id: {query_id}"));
    }
    if let Some(conversation_id) = resp.conversation_id.as_deref() {
        lines.push(format!("conversation_id: {conversation_id}"));
    }
    lines.push(format!(
        "执行步骤: {}，耗时: {}ms",
        resp.total_steps, resp.total_execution_ms
    ));
    if let Some(error) = resp
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("错误: {error}"));
    }
    let result = &resp.final_result;
    lines.push(format!(
        "结果: {} 行，{} 列",
        result.row_count,
        result.columns.len()
    ));
    if !result.columns.is_empty() {
        lines.push(format!("列: {}", result.columns.join(", ")));
    }
    let preview = result.rows.iter().take(5).collect::<Vec<_>>();
    if !preview.is_empty() {
        lines.push("预览:".to_string());
        for (idx, row) in preview.into_iter().enumerate() {
            let row_text = serde_json::to_string(row).unwrap_or_else(|_| row.to_string());
            lines.push(format!(
                "{}. {}",
                idx + 1,
                row_text.chars().take(300).collect::<String>()
            ));
        }
    }
    if result.row_count > 5 {
        lines.push("更多结果请回到 WebUI 数据探索查看。".to_string());
    }
    lines.join("\n")
}

#[cfg(feature = "nl2sql")]
fn format_nl2sql_query_answer(resp: &crate::routes::nl2sql::QueryResponse) -> String {
    if let Some(error) = resp
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!(
            "数据探索执行失败。\nquery_id: {}\n错误: {error}",
            resp.query_id
        );
    }
    if let Some(clarification) = resp
        .clarification_question
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let mut lines = vec![
            "数据探索需要补充信息。".to_string(),
            clarification.to_string(),
        ];
        if let Some(missing) = resp
            .missing_requirements
            .as_ref()
            .filter(|items| !items.is_empty())
        {
            lines.push(format!("缺少条件: {}", missing.join(" / ")));
        }
        lines.push("请直接回复补充条件，我会继续生成 SQL。".to_string());
        return lines.join("\n");
    }
    let mut lines = Vec::new();
    lines.push("数据探索已生成 SQL。".to_string());
    lines.push(format!("query_id: {}", resp.query_id));
    if let Some(conversation_id) = resp.conversation_id.as_deref() {
        lines.push(format!("conversation_id: {conversation_id}"));
    }
    if let Some(sql) = resp.sql.as_deref().filter(|value| !value.trim().is_empty()) {
        lines.push(String::new());
        lines.push("SQL:".to_string());
        lines.push(sql.chars().take(1200).collect::<String>());
    }
    if let Some(explanation) = resp
        .explanation
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(String::new());
        lines.push(truncate_bot_text(explanation, 1200));
    }
    lines.join("\n")
}

#[cfg(feature = "nl2sql")]
fn format_nl2sql_clarify_answer(resp: &crate::routes::nl2sql::ClarifyResponse) -> String {
    if let Some(error) = resp
        .error
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!("数据探索澄清失败。\n错误: {error}");
    }
    if let Some(ctx) = resp.pending_clarification.as_ref() {
        let mut lines = vec![
            "数据探索还需要补充信息。".to_string(),
            ctx.clarification_question.clone(),
        ];
        if !ctx.missing_requirements.is_empty() {
            lines.push(format!(
                "缺少条件: {}",
                ctx.missing_requirements.join(" / ")
            ));
        }
        lines.push("请继续直接回复补充条件。".to_string());
        return lines.join("\n");
    }
    if let Some(data) = resp.data.as_ref() {
        let mut lines = Vec::new();
        lines.push("数据探索已根据补充信息生成 SQL。".to_string());
        lines.push(format!("query_id: {}", data.query_id));
        if let Some(conversation_id) = data.conversation_id.as_deref() {
            lines.push(format!("conversation_id: {conversation_id}"));
        }
        if let Some(error) = data
            .error
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("错误: {error}"));
        }
        if let Some(sql) = data.sql.as_deref().filter(|value| !value.trim().is_empty()) {
            lines.push(String::new());
            lines.push("SQL:".to_string());
            lines.push(sql.chars().take(1200).collect::<String>());
        }
        if let Some(explanation) = data
            .explanation
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(String::new());
            lines.push(truncate_bot_text(explanation, 1200));
        }
        return lines.join("\n");
    }
    "数据探索没有返回可用结果。".to_string()
}

#[cfg(feature = "nl2sql")]
fn nl2sql_pending_state(
    conversation_id: &str,
    session_id: &str,
    ctx: &crate::nl2sql::ClarificationContext,
) -> Value {
    json!({
        "capability": "nl2sql",
        "status": "pending_clarification",
        "conversationId": conversation_id,
        "sessionId": session_id,
        "clarificationContext": ctx,
    })
}

#[cfg(feature = "nl2sql")]
fn parse_nl2sql_pending_state(
    state: Option<Value>,
) -> Option<(String, String, crate::nl2sql::ClarificationContext)> {
    let value = state?;
    if !value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "pending_clarification")
    {
        return None;
    }
    let conversation_id = value
        .get("conversationId")
        .or_else(|| value.get("conversation_id"))
        .and_then(Value::as_str)?
        .to_string();
    let session_id = value
        .get("sessionId")
        .or_else(|| value.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or(&conversation_id)
        .to_string();
    let ctx_value = value
        .get("clarificationContext")
        .or_else(|| value.get("clarification_context"))?
        .clone();
    let ctx = serde_json::from_value::<crate::nl2sql::ClarificationContext>(ctx_value).ok()?;
    Some((conversation_id, session_id, ctx))
}

#[cfg(feature = "nl2sql")]
fn parse_one_based_option_index(text: &str, option_len: usize) -> Option<usize> {
    let trimmed = text.trim();
    let value = trimmed.parse::<usize>().ok()?;
    if value == 0 || value > option_len {
        None
    } else {
        Some(value - 1)
    }
}

#[cfg(feature = "nl2sql")]
async fn run_nl2sql_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    agent_id: &str,
    channel_id: &str,
    capability_config: Option<&Value>,
    external_conversation_id: Option<&str>,
    user_text: &str,
) -> Result<BotCapabilityOutput> {
    let role = load_user_role_for_bot_actor(state, tenant_id, user_id).await?;
    let claims = Claims::new(user_id, "bot-gateway@local", &role, tenant_id);
    let pending_state = load_latest_bot_conversation_state(
        state,
        tenant_id,
        agent_id,
        channel_id,
        external_conversation_id,
        "nl2sql",
    )
    .await?;
    if let Some((conversation_id, session_id, ctx)) = parse_nl2sql_pending_state(pending_state) {
        let selected_option = parse_one_based_option_index(user_text, ctx.options.len())
            .map(|option_index| crate::routes::nl2sql::SelectedOption { option_index });
        let free_text = if selected_option.is_some() {
            None
        } else {
            Some(user_text.to_string())
        };
        let resp = crate::routes::nl2sql::clarify_request(
            State(state.clone()),
            axum::Extension(claims),
            Json(crate::routes::nl2sql::ClarifyRequest {
                session_id: session_id.clone(),
                conversation_id: Some(conversation_id.clone()),
                question: Some(ctx.original_question.clone()),
                clarification_context: Some(ctx),
                selected_option,
                free_text,
                route_confidence: None,
                routing_method: Some("bot_clarification".to_string()),
                semantic_context: None,
                source_query_task_id: None,
            }),
        )
        .await?
        .0;
        let answer = format_nl2sql_clarify_answer(&resp);
        let next_state = resp
            .pending_clarification
            .as_ref()
            .map(|ctx| nl2sql_pending_state(&conversation_id, &session_id, ctx));
        let output = BotCapabilityOutput::answer(answer)
            .with_link(
                "nl2sql_agent_query",
                resp.data
                    .as_ref()
                    .map(|data| data.query_id.as_str())
                    .unwrap_or(&conversation_id),
            )
            .with_output(json!({
                "conversationId": &conversation_id,
                "sessionId": &session_id,
                "pendingClarification": &resp.pending_clarification,
                "data": &resp.data,
                "error": &resp.error,
            }));
        return Ok(if let Some(state) = next_state {
            output.with_conversation_state(state).waiting_for_input()
        } else {
            output
        });
    }
    let datasource_ids = config_string_vec_or_single(
        capability_config,
        &[
            "dataSourceIds",
            "data_source_ids",
            "dataSourceId",
            "data_source_id",
        ],
    );
    let max_steps = config_u32_opt(capability_config, &["maxSteps", "max_steps"])
        .and_then(|value| usize::try_from(value).ok());
    let conversation_id = external_conversation_id
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("bot:{value}"))
        .unwrap_or_else(|| format!("bot-{}", uuid::Uuid::new_v4()));
    let session_id = format!("bot-nl2sql:{conversation_id}");
    if datasource_ids.len() == 1 {
        let resp = crate::routes::nl2sql::query_request(
            State(state.clone()),
            axum::Extension(claims),
            Json(crate::routes::nl2sql::QueryRequest {
                data_source_id: datasource_ids[0].clone(),
                question: user_text.to_string(),
                conversation_id: Some(conversation_id.clone()),
                route_confidence: None,
                routing_method: Some("bot_direct".to_string()),
                semantic_context: None,
                reference_bindings: None,
            }),
        )
        .await?
        .0;
        let answer = format_nl2sql_query_answer(&resp);
        let output = BotCapabilityOutput::answer(answer)
            .with_link("nl2sql_agent_query", &resp.query_id)
            .with_output(json!({
                "queryId": &resp.query_id,
                "conversationId": &resp.conversation_id,
                "sql": &resp.sql,
                "error": &resp.error,
                "clarificationQuestion": &resp.clarification_question,
                "missingRequirements": &resp.missing_requirements,
                "confirmedRequirements": &resp.confirmed_requirements,
            }));
        if let Some(question) = resp.clarification_question.as_ref() {
            let ctx = crate::nl2sql::ClarificationContext {
                original_question: user_text.to_string(),
                clarification_question: question.clone(),
                options: vec![crate::nl2sql::ClarificationOption {
                    option_index: 0,
                    data_source_id: datasource_ids[0].clone(),
                    table_name: "当前数据源".to_string(),
                    column_name: "补充需求".to_string(),
                    reason: "问题缺少必要业务约束，需要继续澄清".to_string(),
                    sim_score: 1.0,
                    business_meaning: "请补充缺失条件".to_string(),
                }],
                confirmed_requirements: resp.confirmed_requirements.clone().unwrap_or_default(),
                missing_requirements: resp.missing_requirements.clone().unwrap_or_default(),
                missing_requirement_reasons: Vec::new(),
                clarification_history: Vec::new(),
                turn: 1,
                conversation_id: conversation_id.clone(),
            };
            return Ok(output
                .with_conversation_state(nl2sql_pending_state(&conversation_id, &session_id, &ctx))
                .waiting_for_input());
        }
        return Ok(output);
    }
    if datasource_ids.is_empty() {
        let route_resp = crate::routes::nl2sql::route_request(
            State(state.clone()),
            axum::Extension(claims.clone()),
            Json(crate::routes::nl2sql::RoutingRouteRequest {
                question: user_text.to_string(),
                data_source_id: None,
            }),
        )
        .await?
        .0;
        if let Some(result) = route_resp.result.as_ref() {
            if result.method == "clarification" {
                if let Some(question) = result.clarification_question.as_ref() {
                    let options = result
                        .matched_tables
                        .iter()
                        .enumerate()
                        .map(|(idx, item)| crate::nl2sql::ClarificationOption {
                            option_index: idx,
                            data_source_id: item.data_source_id.clone(),
                            table_name: item.table_name.clone(),
                            column_name: item.best_column.clone(),
                            reason: item.column_description.clone(),
                            sim_score: item.similarity_score,
                            business_meaning: item.column_description.clone(),
                        })
                        .collect::<Vec<_>>();
                    let ctx = crate::nl2sql::ClarificationContext {
                        original_question: user_text.to_string(),
                        clarification_question: question.clone(),
                        options,
                        confirmed_requirements: Vec::new(),
                        missing_requirements: Vec::new(),
                        missing_requirement_reasons: Vec::new(),
                        clarification_history: Vec::new(),
                        turn: 1,
                        conversation_id: conversation_id.clone(),
                    };
                    let mut lines =
                        vec!["数据探索需要先确认你的问题。".to_string(), question.clone()];
                    if !ctx.options.is_empty() {
                        lines.push("候选项:".to_string());
                        for option in ctx.options.iter().take(5) {
                            lines.push(format!(
                                "{}. {}.{} - {}",
                                option.option_index + 1,
                                option.table_name,
                                option.column_name,
                                truncate_bot_text(&option.reason, 80)
                            ));
                        }
                    }
                    lines.push("请回复补充说明，或回复序号选择候选项。".to_string());
                    return Ok(BotCapabilityOutput::answer(lines.join("\n"))
                        .with_output(json!({
                            "route": route_resp,
                            "pendingClarification": &ctx,
                        }))
                        .with_conversation_state(nl2sql_pending_state(
                            &conversation_id,
                            &session_id,
                            &ctx,
                        ))
                        .waiting_for_input());
                }
            } else if route_resp.routed && !result.data_source_id.trim().is_empty() {
                let resp = crate::routes::nl2sql::query_request(
                    State(state.clone()),
                    axum::Extension(claims),
                    Json(crate::routes::nl2sql::QueryRequest {
                        data_source_id: result.data_source_id.clone(),
                        question: user_text.to_string(),
                        conversation_id: Some(conversation_id.clone()),
                        route_confidence: Some(result.confidence),
                        routing_method: Some(result.method.clone()),
                        semantic_context: Some(json!(&result.matched_tables)),
                        reference_bindings: None,
                    }),
                )
                .await?
                .0;
                let answer = format_nl2sql_query_answer(&resp);
                return Ok(BotCapabilityOutput::answer(answer)
                    .with_link("nl2sql_agent_query", &resp.query_id)
                    .with_output(json!({
                        "queryId": &resp.query_id,
                        "conversationId": &resp.conversation_id,
                        "route": route_resp,
                        "sql": &resp.sql,
                        "error": &resp.error,
                    })));
            }
        }
    }
    let resp = crate::routes::nl2sql::execute_agent_request(
        state,
        &claims,
        crate::routes::nl2sql::AgentExecuteRequest {
            question: user_text.to_string(),
            retrieval_question: None,
            preferred_model: None,
            shared_context: None,
            datasource_ids: datasource_ids.clone(),
            conversation_id: Some(conversation_id.clone()),
            max_steps,
            bounded: false,
        },
    )
    .await
    .map_err(|error| AppError::Internal(format!("nl2sql agent execution failed: {error}")))?;
    let query_id = resp
        .query_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let answer = format_nl2sql_bot_answer(&resp);
    Ok(BotCapabilityOutput::answer(answer)
        .with_link("nl2sql_agent_query", &query_id)
        .with_output(json!({
            "queryId": query_id,
            "conversationId": conversation_id,
            "datasourceIds": datasource_ids,
            "totalSteps": resp.total_steps,
            "totalExecutionMs": resp.total_execution_ms,
            "finalRowCount": resp.final_result.row_count,
            "error": resp.error,
        })))
}

#[cfg(not(feature = "nl2sql"))]
async fn run_nl2sql_capability(
    _state: &AppState,
    _tenant_id: &str,
    _user_id: &str,
    _agent_id: &str,
    _channel_id: &str,
    _capability_config: Option<&Value>,
    _external_conversation_id: Option<&str>,
    _user_text: &str,
) -> Result<BotCapabilityOutput> {
    Err(AppError::ValidationError(
        "nl2sql capability requires the web-server nl2sql feature to be enabled".to_string(),
    ))
}

#[derive(Debug, Clone)]
struct ChatAdversarialMonitorSnapshot {
    status: String,
    current_round: i32,
    max_rounds: i32,
    winner_model: Option<String>,
    winner_reason: Option<String>,
    final_answer: Option<String>,
    error_message: Option<String>,
}

async fn load_chat_adversarial_monitor_snapshot(
    state: &AppState,
    tenant_id: &str,
    run_id: &str,
) -> Result<Option<ChatAdversarialMonitorSnapshot>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, current_round, max_rounds, winner_model, winner_reason, final_answer, error_message
        FROM chat_adversarial_runs
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(state.control_db())
    .await?;
    Ok(row.map(|row| ChatAdversarialMonitorSnapshot {
        status: row.get("status"),
        current_round: row.get("current_round"),
        max_rounds: row.get("max_rounds"),
        winner_model: row.get("winner_model"),
        winner_reason: row.get("winner_reason"),
        final_answer: row.get("final_answer"),
        error_message: row.get("error_message"),
    }))
}

fn build_chat_adversarial_final_push(
    run_id: &str,
    snapshot: &ChatAdversarialMonitorSnapshot,
) -> String {
    if snapshot.status == "failed" {
        return format!(
            "超级对抗任务失败。\nrun_id：{}\n轮次：{}/{}\n原因：{}",
            run_id,
            snapshot.current_round,
            snapshot.max_rounds,
            snapshot.error_message.as_deref().unwrap_or("未知错误")
        );
    }
    if snapshot.status == "cancelled" {
        return format!(
            "超级对抗任务已取消。\nrun_id：{}\n轮次：{}/{}",
            run_id, snapshot.current_round, snapshot.max_rounds
        );
    }
    let mut lines = vec![
        "超级对抗任务已完成。".to_string(),
        format!("run_id：{run_id}"),
        format!("轮次：{}/{}", snapshot.current_round, snapshot.max_rounds),
    ];
    if let Some(winner) = snapshot
        .winner_model
        .as_deref()
        .filter(|v| !v.trim().is_empty())
    {
        lines.push(format!("胜出模型：{winner}"));
    }
    if let Some(reason) = snapshot
        .winner_reason
        .as_deref()
        .filter(|v| !v.trim().is_empty())
    {
        lines.push(format!("裁判理由：{}", truncate_bot_text(reason, 240)));
    }
    if let Some(answer) = snapshot
        .final_answer
        .as_deref()
        .filter(|v| !v.trim().is_empty())
    {
        lines.push(String::new());
        lines.push(truncate_bot_text(answer, 1800));
    }
    lines.join("\n")
}

fn truncate_bot_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(16))
        .collect::<String>();
    out.push_str("\n...(已截断)");
    out
}

#[derive(Debug, Clone)]
struct BotMonitorJob {
    tenant_id: String,
    agent_id: String,
    channel_id: String,
    agent_task_id: String,
    external_conversation_id: Option<String>,
    inbound_log_id: String,
    monitor: BotCapabilityMonitor,
}

async fn bot_monitor_delivery_suppressed(state: &AppState, job: &BotMonitorJob) -> bool {
    match bot_inbound_is_cancelled(state, &job.tenant_id, &job.inbound_log_id).await {
        Ok(cancelled) => cancelled,
        Err(error) => {
            tracing::error!(
                tenant_id = %job.tenant_id,
                inbound_log_id = %job.inbound_log_id,
                agent_task_id = %job.agent_task_id,
                error = %error,
                error_debug = ?error,
                "suppressing Bot monitor delivery because cancellation state is unavailable"
            );
            true
        }
    }
}

async fn monitor_bot_async_capability(state: AppState, job: BotMonitorJob) {
    match job.monitor.clone() {
        BotCapabilityMonitor::SuperAssistant {
            session_id,
            turn_id,
        } => {
            monitor_super_assistant_completion(state, job, session_id, turn_id).await;
        }
        BotCapabilityMonitor::SuperAdversarial { run_id } => {
            monitor_super_adversarial_completion(state, job, run_id).await;
        }
        BotCapabilityMonitor::PmResearch { task_id } => {
            monitor_pm_research_completion(state, job, task_id).await;
        }
        #[cfg(feature = "rd")]
        BotCapabilityMonitor::LinkedResource => {
            monitor_linked_resource_status(state, job).await;
        }
    }
}

async fn monitor_super_assistant_completion(
    state: AppState,
    job: BotMonitorJob,
    session_id: String,
    turn_id: String,
) {
    let max_polls = 360_u32;
    let mut last_status = String::new();
    for _ in 0..max_polls {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let row = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, final_text, error FROM super_assistant_turns
             WHERE tenant_id = ? AND turn_id = ? LIMIT 1",
        )
        .bind(&job.tenant_id)
        .bind(&turn_id)
        .fetch_optional(state.control_db())
        .await;
        let row = match row {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %job.tenant_id,
                    turn_id,
                    error = %error,
                    "failed to monitor Bot unified parent turn"
                );
                continue;
            }
        };
        let status: String = row.get("status");
        if status != last_status {
            last_status.clone_from(&status);
            let _ = agent_ops::add_event(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "parent_progress",
                Some(agent_ops::PHASE_EXECUTING),
                Some(agent_ops::STATUS_RUNNING),
                "info",
                &format!("超级助手父任务状态：{status}"),
                Some(json!({ "turnId": turn_id, "parentStatus": status })),
            )
            .await;
        }
        match status.as_str() {
            "completed" => {
                let answer = row
                    .get::<Option<String>, _>("final_text")
                    .unwrap_or_default();
                if bot_monitor_delivery_suppressed(&state, &job).await {
                    return;
                }
                let send_result = send_channel_message(
                    &state,
                    &job.tenant_id,
                    &job.channel_id,
                    BotOutboundMessage {
                        title: Some(format!(
                            "任务 #{} 已完成",
                            crate::routes::task_control::short_code_for_task_id(&job.agent_task_id)
                        )),
                        text: truncate_bot_text(&answer, 3500),
                        external_conversation_id: job.external_conversation_id.clone(),
                    },
                )
                .await;
                let output = json!({
                    "turnId": turn_id,
                    "sessionId": session_id,
                    "answer": answer,
                    "delivery": send_result.as_ref().ok().map(|result| &result.log_id),
                });
                let _ = agent_ops::complete_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "超级助手父任务已完成",
                    Some(output),
                )
                .await;
                if let Err(error) = send_result {
                    tracing::error!(
                        tenant_id = %job.tenant_id,
                        task_id = %job.agent_task_id,
                        turn_id,
                        error = %error,
                        "Bot parent result completed but final delivery failed"
                    );
                }
                return;
            }
            "failed" => {
                let error = row
                    .get::<Option<String>, _>("error")
                    .unwrap_or_else(|| "超级助手父任务失败".to_string());
                let _ = agent_ops::fail_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "parent_turn_failed",
                    &error,
                )
                .await;
                return;
            }
            "cancelled" => {
                let _ = agent_ops::mark_task_cancelled(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "超级助手父任务已取消",
                    Some(json!({ "turnId": turn_id })),
                )
                .await;
                return;
            }
            _ => {}
        }
    }
    let _ = agent_ops::add_event(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        "parent_monitor_handoff",
        Some(agent_ops::PHASE_EXECUTING),
        Some(agent_ops::STATUS_RUNNING),
        "warn",
        "Bot 本地监控窗口结束，任务继续由 AgentOps 后台值守",
        Some(json!({ "turnId": turn_id, "sessionId": session_id })),
    )
    .await;
}

const BOT_SUPER_ASSISTANT_SESSION_NAME: &str = "Bot";

async fn reusable_bot_super_assistant_session_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    current_task_id: &str,
) -> Result<Option<String>> {
    sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT previous.origin_session_id
         FROM agent_tasks current
         INNER JOIN agent_tasks previous
           ON previous.tenant_id = current.tenant_id
          AND previous.id <> current.id
          AND previous.source = 'bot'
          AND previous.external_platform = current.external_platform
          AND previous.external_channel_id = current.external_channel_id
          AND previous.external_conversation_id = current.external_conversation_id
         INNER JOIN agent_sessions sessions
           ON sessions.session_id = previous.origin_session_id
          AND sessions.tenant_id = previous.tenant_id
          AND sessions.user_id = ?
          AND sessions.state <> 'completed'
         WHERE current.tenant_id = ? AND current.id = ?
           AND COALESCE(current.initiator_user_id, current.owner_user_id) = ?
           AND current.external_conversation_id IS NOT NULL
           AND TRIM(current.external_conversation_id) <> ''
           AND previous.origin_session_id IS NOT NULL
         ORDER BY previous.updated_at DESC, previous.id DESC
         LIMIT 1",
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(current_task_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(Into::into)
}

#[cfg(feature = "rd")]
async fn monitor_linked_resource_status(state: AppState, job: BotMonitorJob) {
    let max_polls = 240_u32;
    for _ in 0..max_polls {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = agent_ops::sync_linked_resource_status(&state, &job.tenant_id, &job.agent_task_id)
            .await;
        let status = sqlx::query_scalar::<sqlx::Sqlite, String>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&job.tenant_id)
        .bind(&job.agent_task_id)
        .fetch_optional(state.control_db())
        .await;
        match status {
            Ok(Some(value))
                if matches!(
                    value.as_str(),
                    "completed" | "failed" | "cancelled" | "timed_out"
                ) =>
            {
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tenant_id = %job.tenant_id,
                    agent_task_id = %job.agent_task_id,
                    "failed to poll linked agent task status: {}",
                    error
                );
            }
        }
    }
    let _ = agent_ops::add_event(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        "linked_resource_monitor_timeout",
        Some(agent_ops::PHASE_EXECUTING),
        Some(agent_ops::STATUS_RUNNING),
        "warn",
        "真实能力任务仍在执行，Bot 后台监控已交接给 AgentOps 轮询",
        Some(json!({
            "agentId": job.agent_id,
            "channelId": job.channel_id,
            "inboundLogId": job.inbound_log_id,
            "maxPolls": max_polls,
            "pollIntervalSeconds": 5,
        })),
    )
    .await;
}

async fn monitor_super_adversarial_completion(state: AppState, job: BotMonitorJob, run_id: String) {
    let max_polls = 240_u32;
    let mut last_round = -1_i32;
    for _ in 0..max_polls {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let snapshot =
            match load_chat_adversarial_monitor_snapshot(&state, &job.tenant_id, &run_id).await {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => {
                    let _ = agent_ops::fail_task(
                        &state,
                        &job.tenant_id,
                        &job.agent_task_id,
                        "linked_run_not_found",
                        "超级对抗 run 未找到",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        tenant_id = %job.tenant_id,
                        agent_task_id = %job.agent_task_id,
                        run_id = %run_id,
                        "failed to poll chat adversarial run: {}",
                        error
                    );
                    continue;
                }
            };
        if snapshot.current_round != last_round {
            last_round = snapshot.current_round;
            let _ = agent_ops::mark_task_running(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                agent_ops::PHASE_DEBATING,
                &format!(
                    "超级对抗执行中：{}，轮次 {}/{}",
                    snapshot.status, snapshot.current_round, snapshot.max_rounds
                ),
                if snapshot.max_rounds > 0 {
                    20 + ((snapshot.current_round.clamp(0, snapshot.max_rounds) * 65)
                        / snapshot.max_rounds)
                } else {
                    50
                },
            )
            .await;
        }
        if !matches!(
            snapshot.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            continue;
        }
        if bot_monitor_delivery_suppressed(&state, &job).await {
            return;
        }
        let push_text = build_chat_adversarial_final_push(&run_id, &snapshot);
        let send_result = send_channel_message(
            &state,
            &job.tenant_id,
            &job.channel_id,
            BotOutboundMessage {
                title: Some("超级对抗结果".to_string()),
                text: push_text,
                external_conversation_id: job.external_conversation_id.clone(),
            },
        )
        .await;
        match snapshot.status.as_str() {
            "completed" => {
                let _ = agent_ops::complete_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "超级对抗完成，结果已推送",
                    Some(json!({
                        "runId": run_id,
                        "status": snapshot.status,
                        "currentRound": snapshot.current_round,
                        "maxRounds": snapshot.max_rounds,
                        "winnerModel": snapshot.winner_model,
                        "winnerReason": snapshot.winner_reason,
                        "finalAnswer": snapshot.final_answer,
                        "pushStatus": send_result.as_ref().map(|r| r.status.clone()).ok(),
                    })),
                )
                .await;
            }
            "failed" => {
                let _ = agent_ops::fail_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "chat_adversarial_failed",
                    snapshot.error_message.as_deref().unwrap_or("超级对抗失败"),
                )
                .await;
            }
            "cancelled" => {
                let _ = agent_ops::sync_linked_resource_status(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                )
                .await;
            }
            _ => {}
        }
        if let Err(error) = send_result {
            tracing::warn!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                agent_task_id = %job.agent_task_id,
                run_id = %run_id,
                "failed to push chat adversarial final result: {}",
                error
            );
        }
        return;
    }
    let _ = agent_ops::fail_task(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        "chat_adversarial_monitor_timeout",
        "超级对抗结果监控超时",
    )
    .await;
}

#[derive(Debug, Clone)]
struct PmResearchMonitorSnapshot {
    status: String,
    stage: Option<String>,
    message: Option<String>,
    response: Option<Value>,
    error_message: Option<String>,
}

#[derive(Debug, Clone)]
struct PmResearchProgressEvent {
    seq: u64,
    status: String,
    stage: Option<String>,
    message: Option<String>,
    detail: Option<Value>,
    response: Option<Value>,
    error_message: Option<String>,
}

async fn load_pm_research_monitor_snapshot(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
) -> Result<Option<PmResearchMonitorSnapshot>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT status, stage, message, CAST(response_json AS TEXT) AS response_json, error_message
        FROM pm_research_tasks
        WHERE tenant_id = ? AND task_id = ?
        ",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(state.control_db())
    .await?;
    Ok(row.map(|row| PmResearchMonitorSnapshot {
        status: row.get("status"),
        stage: row.get("stage"),
        message: row.get("message"),
        response: row
            .get::<Option<String>, _>("response_json")
            .and_then(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .ok()
                    .filter(|value| !value.is_null())
            }),
        error_message: row.get("error_message"),
    }))
}

async fn load_pm_research_progress_events_after(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    after_seq: u64,
    limit: i64,
) -> Result<Vec<PmResearchProgressEvent>> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT seq, status, stage, message, CAST(detail_json AS TEXT) AS detail_json,
               CAST(response_json AS TEXT) AS response_json, error_message
        FROM pm_research_task_events
        WHERE tenant_id = ?
          AND task_id = ?
          AND seq > ?
        ORDER BY seq ASC
        LIMIT ?
        ",
    )
    .bind(tenant_id)
    .bind(task_id)
    .bind(crate::sqlite_i64(after_seq))
    .bind(limit.clamp(1, 20))
    .fetch_all(state.control_db())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| PmResearchProgressEvent {
            seq: row.get::<u64, _>("seq"),
            status: row.get("status"),
            stage: row.get("stage"),
            message: row.get("message"),
            detail: row
                .get::<Option<String>, _>("detail_json")
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
            response: row
                .get::<Option<String>, _>("response_json")
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok()),
            error_message: row.get("error_message"),
        })
        .collect())
}

fn pm_response_answer_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    for key in [
        "answer",
        "finalAnswer",
        "final_answer",
        "text",
        "content",
        "result",
        "summary",
    ] {
        if let Some(text) = value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_string());
        }
    }
    value.as_str().map(str::trim).and_then(|text| {
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    })
}

fn build_pm_research_final_push(task_id: &str, snapshot: &PmResearchMonitorSnapshot) -> String {
    if snapshot.status == "failed" {
        return format!(
            "产运深度分析失败。\ntask_id：{}\n阶段：{}\n原因：{}",
            task_id,
            snapshot.stage.as_deref().unwrap_or("-"),
            snapshot.error_message.as_deref().unwrap_or("未知错误")
        );
    }
    if snapshot.status == "cancelled" {
        return format!(
            "产运深度分析已取消。\ntask_id：{}\n阶段：{}",
            task_id,
            snapshot.stage.as_deref().unwrap_or("-")
        );
    }
    let mut lines = vec![
        "产运深度分析已完成。".to_string(),
        format!("task_id：{task_id}"),
    ];
    if let Some(stage) = snapshot
        .stage
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("阶段：{stage}"));
    }
    if let Some(message) = snapshot
        .message
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("状态：{}", truncate_bot_text(message, 240)));
    }
    if let Some(answer) = pm_response_answer_text(snapshot.response.as_ref()) {
        lines.push(String::new());
        lines.push(truncate_bot_text(&answer, 2200));
    } else {
        lines.push("结果已生成，请回到 AOS 产运助手查看完整报告。".to_string());
    }
    lines.join("\n")
}

fn pm_stage_label(stage: Option<&str>) -> String {
    match stage.unwrap_or("").trim() {
        "queued" => "排队".to_string(),
        "planner" | "planning" => "理解与规划".to_string(),
        "retrieve" | "retrieving" => "检索与采集".to_string(),
        "verify" | "validating" => "核验".to_string(),
        "retry_repair" => "补充修复".to_string(),
        "synthesize" | "synthesizing" => "总结生成".to_string(),
        "done" => "完成".to_string(),
        "failed" => "失败".to_string(),
        "cancelled" => "取消".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "执行中".to_string(),
    }
}

fn pm_next_stage_label(stage: Option<&str>) -> Option<&'static str> {
    match stage.unwrap_or("").trim() {
        "queued" => Some("理解与规划"),
        "planner" | "planning" => Some("检索与采集"),
        "retrieve" | "retrieving" => Some("核验证据"),
        "verify" | "validating" => Some("总结生成"),
        "retry_repair" => Some("继续核验/总结"),
        "synthesize" | "synthesizing" => Some("推送最终结果"),
        _ => None,
    }
}

fn pm_detail_preview(value: Option<&Value>) -> Option<String> {
    let value = value?;
    for key in [
        "summary", "result", "answer", "message", "status", "title", "goal", "query", "variant",
    ] {
        if let Some(text) = value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_string());
        }
    }
    if let Some(count) = value
        .get("sourceCount")
        .or_else(|| value.get("source_count"))
        .or_else(|| value.get("sources"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_array().map(|items| items.len() as u64))
        })
    {
        return Some(format!("已收集/处理 {count} 个来源信号"));
    }
    None
}

fn build_pm_research_progress_push(task_id: &str, event: &PmResearchProgressEvent) -> String {
    let stage = pm_stage_label(event.stage.as_deref());
    let action = match event.status.as_str() {
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "queued" => "已排队",
        _ => "进行中",
    };
    let mut lines = vec![
        format!("产运深度分析进度 · {stage} {action}"),
        format!("task_id：{task_id}"),
    ];
    if let Some(message) = event
        .message
        .as_deref()
        .or(event.error_message.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("状态：{}", truncate_bot_text(message, 260)));
    }
    if let Some(preview) = pm_detail_preview(event.detail.as_ref()) {
        lines.push(format!("阶段输出：{}", truncate_bot_text(&preview, 320)));
    }
    if !matches!(event.status.as_str(), "failed" | "cancelled") {
        if let Some(next) = pm_next_stage_label(event.stage.as_deref()) {
            lines.push(format!("准备下一阶段：{next}"));
        }
    }
    lines.join("\n")
}

async fn monitor_pm_research_completion(state: AppState, job: BotMonitorJob, task_id: String) {
    let max_polls = 360_u32;
    let mut last_stage = String::new();
    let mut last_pushed_seq = 0_u64;
    for _ in 0..max_polls {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let snapshot =
            match load_pm_research_monitor_snapshot(&state, &job.tenant_id, &task_id).await {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => {
                    let _ = agent_ops::fail_task(
                        &state,
                        &job.tenant_id,
                        &job.agent_task_id,
                        "linked_pm_task_not_found",
                        "产运深度分析任务未找到",
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        tenant_id = %job.tenant_id,
                        agent_task_id = %job.agent_task_id,
                        pm_task_id = %task_id,
                        "failed to poll pm research task: {}",
                        error
                    );
                    continue;
                }
            };
        let current_stage = snapshot
            .stage
            .as_deref()
            .unwrap_or(snapshot.status.as_str())
            .to_string();
        if current_stage != last_stage {
            last_stage = current_stage;
            let _ =
                agent_ops::sync_linked_resource_status(&state, &job.tenant_id, &job.agent_task_id)
                    .await;
        }
        if bot_monitor_delivery_suppressed(&state, &job).await {
            if matches!(
                snapshot.status.as_str(),
                "completed" | "failed" | "cancelled"
            ) {
                return;
            }
            continue;
        }
        match load_pm_research_progress_events_after(
            &state,
            &job.tenant_id,
            &task_id,
            last_pushed_seq,
            10,
        )
        .await
        {
            Ok(events) => {
                for event in events {
                    last_pushed_seq = last_pushed_seq.max(event.seq);
                    let terminal_final_event = event.response.is_some()
                        || matches!(
                            event.stage.as_deref(),
                            Some("done" | "failed" | "cancelled")
                        );
                    if terminal_final_event {
                        continue;
                    }
                    let push_text = build_pm_research_progress_push(&task_id, &event);
                    let send_result = send_channel_message(
                        &state,
                        &job.tenant_id,
                        &job.channel_id,
                        BotOutboundMessage {
                            title: Some("产运深度分析进度".to_string()),
                            text: push_text,
                            external_conversation_id: job.external_conversation_id.clone(),
                        },
                    )
                    .await;
                    if let Err(error) = send_result {
                        tracing::warn!(
                            tenant_id = %job.tenant_id,
                            agent_id = %job.agent_id,
                            channel_id = %job.channel_id,
                            inbound_log_id = %job.inbound_log_id,
                            agent_task_id = %job.agent_task_id,
                            pm_task_id = %task_id,
                            event_seq = event.seq,
                            "failed to push pm research progress event: {}",
                            error
                        );
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    tenant_id = %job.tenant_id,
                    agent_task_id = %job.agent_task_id,
                    pm_task_id = %task_id,
                    "failed to load pm research progress events: {}",
                    error
                );
            }
        }
        if !matches!(
            snapshot.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            continue;
        }
        let push_text = build_pm_research_final_push(&task_id, &snapshot);
        let send_result = send_channel_message(
            &state,
            &job.tenant_id,
            &job.channel_id,
            BotOutboundMessage {
                title: Some("产运深度分析结果".to_string()),
                text: push_text,
                external_conversation_id: job.external_conversation_id.clone(),
            },
        )
        .await;
        match snapshot.status.as_str() {
            "completed" => {
                let _ = agent_ops::complete_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "产运深度分析完成，结果已推送",
                    Some(json!({
                        "taskId": task_id,
                        "status": snapshot.status,
                        "stage": snapshot.stage,
                        "message": snapshot.message,
                        "response": snapshot.response,
                        "pushStatus": send_result.as_ref().map(|r| r.status.clone()).ok(),
                    })),
                )
                .await;
            }
            "failed" => {
                let _ = agent_ops::fail_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "pm_research_failed",
                    snapshot
                        .error_message
                        .as_deref()
                        .unwrap_or("产运深度分析失败"),
                )
                .await;
            }
            "cancelled" => {
                let _ = agent_ops::sync_linked_resource_status(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                )
                .await;
            }
            _ => {}
        }
        if let Err(error) = send_result {
            tracing::warn!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                agent_task_id = %job.agent_task_id,
                pm_task_id = %task_id,
                "failed to push pm research final result: {}",
                error
            );
        }
        return;
    }
    let _ = agent_ops::fail_task(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        "pm_research_monitor_timeout",
        "产运深度分析结果监控超时",
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn run_super_assistant_bot_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    agent_task_id: &str,
    capability_key: &str,
    agent_name: &str,
    persona_prompt: Option<&str>,
    sender_name: Option<&str>,
    context: &BotConversationContext,
    capability_config: Option<&Value>,
    external_channel_id: &str,
    external_conversation_id: Option<&str>,
    user_text: &str,
    attachments: &[StagedBotAttachment],
) -> Result<BotCapabilityOutput> {
    let user = sqlx::query::<sqlx::Sqlite>(
        "SELECT email, role FROM users
         WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or(AppError::Forbidden)?;
    let claims = crate::auth::Claims::new(
        user_id,
        &user.get::<String, _>("email"),
        &user.get::<String, _>("role"),
        tenant_id,
    );
    let requested_model = config_string(capability_config, &["model"]);
    let session_scope = external_conversation_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|conversation_id| {
            format!("{tenant_id}\0{user_id}\0{external_channel_id}\0{conversation_id}")
        });
    let session_scope_guard = match session_scope.as_deref() {
        Some(scope) => Some(bot_session_scope_lock(scope).lock_owned().await),
        None => None,
    };
    let reusable_session_id = reusable_bot_super_assistant_session_id(
        state.control_db(),
        tenant_id,
        user_id,
        agent_task_id,
    )
    .await?;
    let reusable_session = match reusable_session_id.as_deref() {
        Some(session_id) => {
            state
                .agent_manager()
                .get_owned_session(session_id, tenant_id, user_id)
                .await
        }
        None => None,
    };
    let session_reused = reusable_session.is_some();
    let session = match reusable_session {
        Some(session) => session,
        None => state
            .agent_manager()
            .create_session(
                user_id,
                tenant_id,
                None,
                requested_model.as_deref(),
                "super_assistant",
                Some("chat"),
                None,
                None,
            )
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to create Bot parent session: {error}"))
            })?,
    };
    if !session_reused {
        state
            .agent_manager()
            .rename_session(&session.session_id, BOT_SUPER_ASSISTANT_SESSION_NAME)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to name Bot parent session: {error}"))
            })?;
    }
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks SET origin_session_id = ?, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?",
    )
    .bind(&session.session_id)
    .bind(tenant_id)
    .bind(agent_task_id)
    .execute(state.control_db())
    .await?;
    let turn_id = uuid::Uuid::new_v4().to_string();
    let empty_context = BotConversationContext::default();
    let slash_command =
        crate::routes::super_assistant::parse_super_assistant_slash_command(user_text);
    if slash_command
        .as_ref()
        .is_some_and(|command| command.prompt.is_empty())
    {
        return Err(AppError::ValidationError(
            "slash command requires a task description".to_string(),
        ));
    }
    let dispatched_skill = if slash_command.is_none() {
        crate::routes::agent::maybe_dispatch_skill_command(user_text, tenant_id, state.control_db())
            .await
    } else {
        None
    };
    let execution_user_text = slash_command
        .as_ref()
        .map(|command| command.prompt.clone())
        .or(dispatched_skill)
        .unwrap_or_else(|| user_text.to_string());
    let prompt = build_bot_prompt(
        capability_key,
        agent_name,
        persona_prompt,
        sender_name,
        if session_reused {
            &empty_context
        } else {
            context
        },
        &execution_user_text,
    );
    let mut explicit_capability = match capability_key {
        "generic_ai" | "ai_chat" => None,
        other => Some(other.to_string()),
    };
    let mut data_attribution = capability_key == "data_attribution";
    if let Some(command) = slash_command {
        explicit_capability = command.mode.explicit_capability().map(str::to_string);
        data_attribution = command.mode
            == crate::routes::super_assistant::SuperAssistantSlashMode::DataAttribution;
    }
    let images = attachments
        .iter()
        .filter_map(StagedBotAttachment::as_image_input)
        .collect::<Vec<_>>();
    let documents = attachments
        .iter()
        .filter_map(StagedBotAttachment::as_document_input)
        .collect::<Vec<_>>();
    for attachment in attachments {
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE chat_file_workspace_files SET session_id = COALESCE(session_id, ?)
             WHERE tenant_id = ? AND user_id = ? AND file_id = ?",
        )
        .bind(&session.session_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(&attachment.file_id)
        .execute(state.control_db())
        .await?;
    }
    crate::routes::super_assistant::enqueue_background_super_assistant_turn(
        state.clone(),
        claims,
        crate::routes::super_assistant::SuperAssistantMessageRequest {
            session_id: session.session_id.clone(),
            text: prompt,
            images,
            documents,
            display_text: Some(user_text.to_string()),
            turn_id: Some(turn_id.clone()),
            explicit_capability,
            app: Some("chat".to_string()),
            model: requested_model,
            data_source_id: None,
            data_attribution,
            router_config: None,
        },
    )
    .await?;
    drop(session_scope_guard);
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE agent_tasks
         SET parent_task_id = ?, root_task_id = ?
         WHERE tenant_id = ? AND origin_turn_id = ? AND id <> ?",
    )
    .bind(agent_task_id)
    .bind(agent_task_id)
    .bind(tenant_id)
    .bind(&turn_id)
    .bind(agent_task_id)
    .execute(state.control_db())
    .await?;
    Ok(BotCapabilityOutput {
        answer: format!(
            "任务已受理，正在由超级助手处理。任务编号 #{}，完成后会发送结果。",
            crate::routes::task_control::short_code_for_task_id(agent_task_id)
        ),
        linked_resource_type: Some("super_assistant_turn".to_string()),
        linked_resource_id: Some(turn_id.clone()),
        output_json: Some(json!({
            "executionPath": "super_assistant_parent",
            "sessionId": session.session_id,
            "turnId": turn_id,
            "capability": capability_key,
            "sessionReused": session_reused,
        })),
        conversation_state: None,
        keep_agent_task_open: true,
        waiting_for_input: false,
        monitor: Some(BotCapabilityMonitor::SuperAssistant {
            session_id: session.session_id,
            turn_id,
        }),
    })
}

async fn run_text_capability(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    agent_task_id: &str,
    capability_key: &str,
    agent_id: &str,
    agent_name: &str,
    persona_prompt: Option<&str>,
    sender_name: Option<&str>,
    context: &BotConversationContext,
    capability_config: Option<&Value>,
    capabilities: &[BotAgentCapabilityInfo],
    external_conversation_id: Option<&str>,
    external_channel_id: &str,
    user_text: &str,
    attachments: &[StagedBotAttachment],
) -> Result<BotCapabilityOutput> {
    if capability_key != "watchdog" {
        return run_super_assistant_bot_capability(
            state,
            tenant_id,
            user_id,
            agent_task_id,
            capability_key,
            agent_name,
            persona_prompt,
            sender_name,
            context,
            capability_config,
            external_channel_id,
            external_conversation_id,
            user_text,
            attachments,
        )
        .await;
    }
    match capability_key {
        "aos_router" => {
            run_aos_router_capability(
                state,
                tenant_id,
                user_id,
                agent_task_id,
                agent_id,
                agent_name,
                persona_prompt,
                sender_name,
                context,
                capability_config,
                capabilities,
                external_conversation_id,
                external_channel_id,
                user_text,
                attachments,
            )
            .await
        }
        "pm_assistant" if pm_assistant_deep_mode_enabled(capability_config) => {
            let started = Instant::now();
            let prompt = build_bot_prompt(
                capability_key,
                agent_name,
                persona_prompt,
                sender_name,
                context,
                user_text,
            );
            let output =
                run_pm_research_capability(state, tenant_id, user_id, capability_config, prompt)
                    .await?;
            tracing::info!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                capability = %capability_key,
                answer_chars = output.answer.chars().count(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bot inbound PM research task created"
            );
            Ok(output)
        }
        "generic_ai" | "pm_assistant" | "ai_chat" => {
            let started = Instant::now();
            let prompt = build_bot_prompt(
                capability_key,
                agent_name,
                persona_prompt,
                sender_name,
                context,
                user_text,
            );
            let model = config_string(capability_config, &["model"])
                .unwrap_or_else(|| state.default_model.clone());
            let result = crate::routes::pm::run_pm_chat_completion(
                state,
                tenant_id,
                user_id,
                model,
                vec![crate::routes::chat::ChatMessage {
                    role: "user".to_string(),
                    content: Value::String(prompt),
                }],
            )
            .await?;
            tracing::info!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                capability = %capability_key,
                provider = %result.provider_name,
                model = %result.usage.model,
                input_tokens = result.usage.input_tokens,
                output_tokens = result.usage.output_tokens,
                total_tokens = result.usage.total_tokens,
                answer_chars = result.answer.chars().count(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bot inbound capability model call completed"
            );
            let provider_name = result.provider_name.clone();
            let usage_record = crate::routes::chat::TokenUsageRecord {
                tenant_id: tenant_id.to_string(),
                user_id: user_id.to_string(),
                session_id: "bot-gateway".to_string(),
                request_id: None,
                model: result.usage.model.clone(),
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_tokens: result.usage.total_tokens,
                estimated_cost_usd: result.usage.estimated_cost_usd,
                api_key_id: Some(result.api_key_id),
                provider: provider_name.clone(),
                created_at: Utc::now(),
            };
            if let Err(error) = state.usage_writer().write(&usage_record).await {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    "failed to record bot gateway token usage: {}",
                    error
                );
            }
            let answer = if capability_key == "pm_assistant" {
                format_pm_assistant_short_answer(&result.answer)
            } else {
                result.answer
            };
            Ok(
                BotCapabilityOutput::answer(answer).with_output(json!({
                    "executionPath": if capability_key == "pm_assistant" { "short_llm" } else { "chat_llm" },
                    "provider": provider_name,
                    "usage": result.usage,
                    "appliedRules": result.applied_rules,
                })),
            )
        }
        "rd_agent" => {
            let started = Instant::now();
            let prompt = build_bot_prompt(
                capability_key,
                agent_name,
                persona_prompt,
                sender_name,
                context,
                user_text,
            );
            let output = run_rd_agent_capability_output(
                state,
                tenant_id,
                user_id,
                agent_task_id,
                capability_config,
                prompt,
            )
            .await?;
            tracing::info!(
                tenant_id = %tenant_id,
                user_id = %user_id,
                capability = %capability_key,
                answer_chars = output.answer.chars().count(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "bot inbound RD task created"
            );
            Ok(output)
        }
        "super_adversarial" => {
            let prompt = build_bot_prompt(
                capability_key,
                agent_name,
                persona_prompt,
                sender_name,
                context,
                user_text,
            );
            run_super_adversarial_capability(state, tenant_id, user_id, capability_config, prompt)
                .await
        }
        "watchdog" => {
            run_watchdog_capability(
                state,
                tenant_id,
                user_id,
                capability_config,
                external_conversation_id,
                external_channel_id,
                agent_task_id,
                user_text,
            )
            .await
        }
        "nl2sql" => {
            run_nl2sql_capability(
                state,
                tenant_id,
                user_id,
                agent_id,
                external_channel_id,
                capability_config,
                external_conversation_id,
                user_text,
            )
            .await
        }
        other => Ok(BotCapabilityOutput::answer(unsupported_capability_reply(
            other,
        ))),
    }
}

#[derive(Debug, Clone)]
struct InboundExecutionJob {
    tenant_id: String,
    agent_id: String,
    channel_id: String,
    external_conversation_id: Option<String>,
    selected_capability: String,
    inbound_log_id: String,
    agent_task_id: String,
    agent_name: String,
    persona_prompt: Option<String>,
    actor_user_id: String,
    sender_name: Option<String>,
    capability_config: Option<Value>,
    capabilities: Vec<BotAgentCapabilityInfo>,
    user_text: String,
    attachments: Vec<BotInboundAttachment>,
    staged_attachments: Vec<StagedBotAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct StagedBotAttachment {
    file_id: String,
    filename: String,
    media_type: String,
    size_bytes: u64,
    url: String,
    kind: String,
}

impl StagedBotAttachment {
    fn as_image_input(&self) -> Option<crate::routes::agent::PmTaskImageInput> {
        (self.kind == "image" || self.media_type.starts_with("image/")).then(|| {
            crate::routes::agent::PmTaskImageInput {
                url: self.url.clone(),
                file_id: Some(self.file_id.clone()),
                media_type: Some(self.media_type.clone()),
                name: Some(self.filename.clone()),
                size_bytes: Some(self.size_bytes),
            }
        })
    }

    fn as_document_input(&self) -> Option<crate::routes::agent::PmTaskDocumentInput> {
        (self.kind != "image" && !self.media_type.starts_with("image/")).then(|| {
            crate::routes::agent::PmTaskDocumentInput {
                url: self.url.clone(),
                file_id: Some(self.file_id.clone()),
                media_type: Some(self.media_type.clone()),
                name: Some(self.filename.clone()),
                size_bytes: Some(self.size_bytes),
            }
        })
    }
}

#[derive(Debug)]
struct BotAttachmentAccess {
    platform: String,
    outbound_token: Option<String>,
    config: Option<Value>,
}

async fn load_bot_attachment_access(
    state: &AppState,
    tenant_id: &str,
    channel_id: &str,
) -> Result<BotAttachmentAccess> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT platform, outbound_token, CAST(config_json AS TEXT) AS config_json
         FROM bot_agent_channels WHERE tenant_id = ? AND id = ? AND enabled = 1 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(channel_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("Bot attachment channel not found".to_string()))?;
    let outbound_token = decrypt_bot_channel_secret(
        row.get("outbound_token"),
        tenant_id,
        channel_id,
        BotChannelSecretKind::OutboundToken,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret decrypt failed: {error}")))?;
    Ok(BotAttachmentAccess {
        platform: row.get::<String, _>("platform").trim().to_ascii_lowercase(),
        outbound_token,
        config: parse_json_opt(row.get("config_json")),
    })
}

fn attachment_host_matches(host: &str, suffix: &str) -> bool {
    host.eq_ignore_ascii_case(suffix)
        || host
            .to_ascii_lowercase()
            .ends_with(&format!(".{}", suffix.to_ascii_lowercase()))
}

fn configured_attachment_hosts(access: &BotAttachmentAccess) -> Vec<String> {
    access
        .config
        .as_ref()
        .and_then(|value| {
            value
                .get("attachmentHostAllowlist")
                .or_else(|| value.get("attachment_host_allowlist"))
        })
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

fn bot_attachment_host_allowed(access: &BotAttachmentAccess, host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if configured_attachment_hosts(access)
        .iter()
        .any(|allowed| attachment_host_matches(&host, allowed))
    {
        return true;
    }
    match access.platform.as_str() {
        "slack" => ["slack.com", "slack-files.com"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        "discord" => ["discordapp.com", "discordapp.net"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        "telegram" => attachment_host_matches(&host, "api.telegram.org"),
        "whatsapp" => ["facebook.com", "fbsbx.com", "fbcdn.net"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        "feishu" | "lark" => ["feishu.cn", "larksuite.com"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        "dingtalk" => ["dingtalk.com", "alicdn.com"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        "wecom" => ["qq.com", "weixin.qq.com"]
            .iter()
            .any(|suffix| attachment_host_matches(&host, suffix)),
        _ => false,
    }
}

fn attachment_ip_is_public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
                || octets[0] >= 240)
        }
        std::net::IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => attachment_ip_is_public(std::net::IpAddr::V4(ip)),
            None => {
                !(ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local())
            }
        },
    }
}

async fn validate_bot_attachment_url(
    access: &BotAttachmentAccess,
    raw_url: &str,
) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|_| AppError::ValidationError("invalid Bot attachment URL".to_string()))?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::ValidationError(
            "Bot attachments require an HTTPS URL without embedded credentials".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::ValidationError("Bot attachment URL has no host".to_string()))?;
    if !bot_attachment_host_allowed(access, host) {
        return Err(AppError::Forbidden);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| AppError::Internal(format!("attachment DNS lookup failed: {error}")))?
        .map(|address| address.ip())
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|ip| !attachment_ip_is_public(*ip)) {
        return Err(AppError::Forbidden);
    }
    Ok(url)
}

async fn bot_attachment_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("AOS-Bot-Attachment/1.0")
        .build()
        .map_err(|error| AppError::Internal(format!("failed to build attachment client: {error}")))
}

async fn fetch_json_with_bearer(
    client: &reqwest::Client,
    access: &BotAttachmentAccess,
    url: &str,
    token: Option<&str>,
) -> Result<Value> {
    let url = validate_bot_attachment_url(access, url).await?;
    let mut request = client.get(url);
    if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|_| AppError::Internal("Bot attachment metadata request failed".to_string()))?;
    if !response.status().is_success() {
        return Err(AppError::Internal(format!(
            "Bot attachment metadata provider returned HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| AppError::Internal("Bot attachment metadata was not valid JSON".to_string()))
}

async fn feishu_attachment_token(
    client: &reqwest::Client,
    access: &BotAttachmentAccess,
) -> Result<String> {
    let config = access.config.as_ref();
    let app_id = config_string(config, &["appId", "app_id", "appKey", "app_key"])
        .ok_or_else(|| AppError::ValidationError("Feishu App ID is missing".to_string()))?;
    let app_secret = config_string(
        config,
        &["appSecret", "app_secret", "clientSecret", "client_secret"],
    )
    .ok_or_else(|| AppError::ValidationError("Feishu App Secret is missing".to_string()))?;
    let api_base = config_string(config, &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://open.feishu.cn/open-apis".to_string());
    let url = format!(
        "{}/auth/v3/tenant_access_token/internal",
        api_base.trim_end_matches('/')
    );
    let url = validate_bot_attachment_url(access, &url).await?;
    let response = client
        .post(url)
        .json(&json!({ "app_id": app_id, "app_secret": app_secret }))
        .send()
        .await
        .map_err(|_| AppError::Internal("Feishu attachment authentication failed".to_string()))?;
    let status = response.status();
    let payload = response.json::<Value>().await.map_err(|_| {
        AppError::Internal("Feishu attachment authentication returned invalid JSON".to_string())
    })?;
    if !status.is_success()
        || payload
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            != 0
    {
        return Err(AppError::Internal(format!(
            "Feishu attachment authentication failed with HTTP {status}"
        )));
    }
    payload
        .get("tenant_access_token")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| AppError::Internal("Feishu attachment token is missing".to_string()))
}

async fn resolve_bot_attachment_download(
    client: &reqwest::Client,
    access: &BotAttachmentAccess,
    attachment: &BotInboundAttachment,
) -> Result<(String, Option<String>)> {
    if let Some(url) = attachment.download_url.as_deref() {
        let bearer = match access.platform.as_str() {
            "slack" | "whatsapp" => access.outbound_token.clone(),
            _ => config_string(
                access.config.as_ref(),
                &["attachmentBearerToken", "attachment_bearer_token"],
            ),
        };
        return Ok((url.to_string(), bearer));
    }
    let file_id = attachment
        .provider_file_id
        .as_deref()
        .ok_or_else(|| AppError::ValidationError("Bot attachment has no file id".to_string()))?;
    match access.platform.as_str() {
        "telegram" => {
            let token = access.outbound_token.as_deref().ok_or_else(|| {
                AppError::ValidationError("Telegram Bot token is missing".to_string())
            })?;
            let metadata = fetch_json_with_bearer(
                client,
                access,
                &format!(
                    "https://api.telegram.org/bot{token}/getFile?file_id={}",
                    urlencoding::encode(file_id)
                ),
                None,
            )
            .await?;
            let file_path = metadata
                .pointer("/result/file_path")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::Internal("Telegram file path is missing".to_string()))?;
            Ok((
                format!("https://api.telegram.org/file/bot{token}/{file_path}"),
                None,
            ))
        }
        "whatsapp" => {
            let token = access.outbound_token.as_deref().ok_or_else(|| {
                AppError::ValidationError("WhatsApp access token is missing".to_string())
            })?;
            let api_base = config_string(access.config.as_ref(), &["apiBase", "api_base"])
                .unwrap_or_else(|| "https://graph.facebook.com/v21.0".to_string());
            let metadata = fetch_json_with_bearer(
                client,
                access,
                &format!(
                    "{}/{}",
                    api_base.trim_end_matches('/'),
                    urlencoding::encode(file_id)
                ),
                Some(token),
            )
            .await?;
            let url = metadata
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::Internal("WhatsApp media URL is missing".to_string()))?;
            Ok((url.to_string(), Some(token.to_string())))
        }
        "feishu" | "lark" => {
            let message_id = attachment.provider_message_id.as_deref().ok_or_else(|| {
                AppError::ValidationError("Feishu attachment message id is missing".to_string())
            })?;
            let token = feishu_attachment_token(client, access).await?;
            let api_base = config_string(access.config.as_ref(), &["apiBase", "api_base"])
                .unwrap_or_else(|| "https://open.feishu.cn/open-apis".to_string());
            Ok((
                format!(
                    "{}/im/v1/messages/{}/resources/{}?type={}",
                    api_base.trim_end_matches('/'),
                    urlencoding::encode(message_id),
                    urlencoding::encode(file_id),
                    if attachment.kind == "image" {
                        "image"
                    } else {
                        "file"
                    }
                ),
                Some(token),
            ))
        }
        _ => Err(AppError::ValidationError(format!(
            "{} attachments require a provider download URL or adapter credentials",
            access.platform
        ))),
    }
}

async fn download_bot_attachment(
    client: &reqwest::Client,
    access: &BotAttachmentAccess,
    attachment: &BotInboundAttachment,
) -> Result<Vec<u8>> {
    let (url, bearer) = resolve_bot_attachment_download(client, access, attachment).await?;
    let url = validate_bot_attachment_url(access, &url).await?;
    let mut request = client.get(url);
    if let Some(token) = bearer.as_deref().filter(|value| !value.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| AppError::Internal("Bot attachment download failed".to_string()))?;
    if response.status().is_redirection() {
        return Err(AppError::ValidationError(
            "Bot attachment provider returned an unverified redirect".to_string(),
        ));
    }
    if !response.status().is_success() {
        return Err(AppError::Internal(format!(
            "Bot attachment provider returned HTTP {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size > BOT_ATTACHMENT_MAX_DOWNLOAD_BYTES as u64)
    {
        return Err(AppError::PayloadTooLarge(
            "Bot attachment exceeds the 50 MiB download limit".to_string(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::Internal("Bot attachment stream failed".to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > BOT_ATTACHMENT_MAX_DOWNLOAD_BYTES {
            return Err(AppError::PayloadTooLarge(
                "Bot attachment exceeds the 50 MiB download limit".to_string(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn persist_staged_bot_attachments(
    state: &AppState,
    job: &InboundExecutionJob,
    staged: &[StagedBotAttachment],
) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "UPDATE bot_message_logs
         SET content_json = JSON_SET(COALESCE(content_json, JSON_OBJECT()),
             '$.stagedAttachments', json(?)), updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ?
           AND status NOT IN ('cancelling','cancelled')
           AND queue_status NOT IN ('cancelling','cancelled')",
    )
    .bind(serde_json::to_string(staged)?)
    .bind(&job.tenant_id)
    .bind(&job.inbound_log_id)
    .execute(state.control_db())
    .await?;
    Ok(())
}

fn inbound_attachments_from_content(content: &Value) -> Vec<BotInboundAttachment> {
    content
        .get("attachments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn staged_attachments_from_content(content: &Value) -> Vec<StagedBotAttachment> {
    content
        .get("stagedAttachments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

async fn stage_bot_attachments(
    state: &AppState,
    job: &InboundExecutionJob,
) -> Result<Vec<StagedBotAttachment>> {
    if !job.staged_attachments.is_empty() {
        return Ok(job.staged_attachments.clone());
    }
    if job.attachments.is_empty() {
        return Ok(Vec::new());
    }
    let access = load_bot_attachment_access(state, &job.tenant_id, &job.channel_id).await?;
    let client = bot_attachment_http_client().await?;
    let mut staged = Vec::new();
    for attachment in &job.attachments {
        if bot_inbound_is_cancelled(state, &job.tenant_id, &job.inbound_log_id).await? {
            break;
        }
        let bytes = download_bot_attachment(&client, &access, attachment).await?;
        if bot_inbound_is_cancelled(state, &job.tenant_id, &job.inbound_log_id).await? {
            break;
        }
        let uploaded = crate::routes::upload::store_upload_bytes(
            state,
            &job.actor_user_id,
            &attachment.filename,
            &attachment.media_type,
            &bytes,
        )
        .await?;
        crate::routes::chat_intelligence::register_chat_file_internal(
            state,
            &job.tenant_id,
            &job.actor_user_id,
            crate::routes::chat_intelligence::ChatFileRegisterRequest {
                file_id: uploaded.file_id.clone(),
                filename: uploaded.filename.clone(),
                media_type: uploaded.media_type.clone(),
                size: Some(uploaded.size),
                url: uploaded.url.clone(),
                session_id: None,
            },
        )
        .await?;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT OR IGNORE INTO agent_task_artifacts
               (id, tenant_id, task_id, owner_user_id, artifact_type, name,
                artifact_ref, mime_type, size_bytes, sensitivity_label, metadata_json)
             VALUES (?, ?, ?, ?, 'bot_attachment', ?, ?, ?, ?, 'internal', ?)",
        )
        .bind(format!("agtart-{}", uuid::Uuid::new_v4()))
        .bind(&job.tenant_id)
        .bind(&job.agent_task_id)
        .bind(&job.actor_user_id)
        .bind(&uploaded.filename)
        .bind(&uploaded.url)
        .bind(&uploaded.media_type)
        .bind(crate::sqlite_i64(uploaded.size))
        .bind(
            json!({
                "fileId": uploaded.file_id,
                "source": "bot",
                "platform": access.platform,
            })
            .to_string(),
        )
        .execute(state.control_db())
        .await?;
        staged.push(StagedBotAttachment {
            file_id: uploaded.file_id,
            filename: uploaded.filename,
            media_type: uploaded.media_type,
            size_bytes: uploaded.size,
            url: uploaded.url,
            kind: attachment.kind.clone(),
        });
        persist_staged_bot_attachments(state, job, &staged).await?;
    }
    Ok(staged)
}

#[derive(Debug, Clone)]
enum InboundExecutionOutcome {
    Succeeded,
    Ignored { reason: String },
    Cancelled { reason: String },
    Failed { error_code: String, message: String },
}

impl InboundExecutionOutcome {
    fn queue_success(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Ignored { .. })
    }

    fn error_message(&self) -> Option<&str> {
        match self {
            Self::Succeeded => None,
            Self::Ignored { reason } => Some(reason.as_str()),
            Self::Cancelled { reason } => Some(reason.as_str()),
            Self::Failed { message, .. } => Some(message.as_str()),
        }
    }

    fn agent_event(&self) -> (&'static str, &'static str, &'static str, String) {
        match self {
            Self::Succeeded => (
                "bot.queue.succeeded",
                agent_ops::STATUS_COMPLETED,
                "info",
                "Bot 入站队列执行完成".to_string(),
            ),
            Self::Ignored { reason } => (
                "bot.queue.ignored",
                agent_ops::STATUS_COMPLETED,
                "info",
                format!("Bot 入站队列已忽略：{reason}"),
            ),
            Self::Cancelled { reason } => (
                "bot.queue.cancelled",
                agent_ops::STATUS_CANCELLED,
                "warn",
                format!("Bot 入站队列已取消：{reason}"),
            ),
            Self::Failed {
                error_code,
                message,
            } => (
                "bot.queue.dead",
                agent_ops::STATUS_FAILED,
                "error",
                format!("Bot 入站队列执行失败：{error_code}: {message}"),
            ),
        }
    }
}

async fn bot_inbound_is_cancelled(
    state: &AppState,
    tenant_id: &str,
    inbound_log_id: &str,
) -> Result<bool> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT queue_status, status FROM bot_message_logs
         WHERE tenant_id = ? AND id = ? AND direction = 'inbound' LIMIT 1",
    )
    .bind(tenant_id)
    .bind(inbound_log_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound("Bot inbound message not found".to_string()))?;
    let queue_status: String = row.get("queue_status");
    let status: String = row.get("status");
    Ok(matches!(queue_status.as_str(), "cancelling" | "cancelled")
        || matches!(status.as_str(), "cancelling" | "cancelled"))
}

async fn bot_inbound_stop_outcome(
    state: &AppState,
    job: &InboundExecutionJob,
    stage: &str,
) -> Option<InboundExecutionOutcome> {
    match bot_inbound_is_cancelled(state, &job.tenant_id, &job.inbound_log_id).await {
        Ok(false) => None,
        Ok(true) => {
            tracing::info!(
                tenant_id = %job.tenant_id,
                inbound_log_id = %job.inbound_log_id,
                agent_task_id = %job.agent_task_id,
                stage,
                "bot inbound execution observed cancellation"
            );
            Some(InboundExecutionOutcome::Cancelled {
                reason: format!("cancelled during {stage}"),
            })
        }
        Err(error) => {
            let message =
                format!("failed to verify Bot cancellation state during {stage}: {error}");
            tracing::error!(
                tenant_id = %job.tenant_id,
                inbound_log_id = %job.inbound_log_id,
                agent_task_id = %job.agent_task_id,
                stage,
                error = %error,
                error_debug = ?error,
                "Bot execution stopped because cancellation state could not be verified"
            );
            let _ = agent_ops::fail_task(
                state,
                &job.tenant_id,
                &job.agent_task_id,
                "bot_cancel_state_check_failed",
                &message,
            )
            .await;
            Some(InboundExecutionOutcome::Failed {
                error_code: "bot_cancel_state_check_failed".to_string(),
                message,
            })
        }
    }
}

async fn link_bot_capability_output(
    state: &AppState,
    job: &InboundExecutionJob,
    output: &BotCapabilityOutput,
) -> Result<()> {
    let (Some(resource_type), Some(resource_id)) = (
        output.linked_resource_type.as_deref(),
        output.linked_resource_id.as_deref(),
    ) else {
        return Ok(());
    };
    agent_ops::link_task_resource(
        state,
        &job.tenant_id,
        &job.agent_task_id,
        resource_type,
        resource_id,
    )
    .await?;
    if resource_type == "super_assistant_turn" {
        let origin_session_id = output
            .output_json
            .as_ref()
            .and_then(|value| value.get("sessionId"))
            .and_then(Value::as_str);
        if let Some(origin_session_id) = origin_session_id {
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE agent_tasks
                 SET origin_session_id = ?, origin_turn_id = ?, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND id = ?",
            )
            .bind(origin_session_id)
            .bind(resource_id)
            .bind(&job.tenant_id)
            .bind(&job.agent_task_id)
            .execute(state.control_db())
            .await?;
        }
    }
    Ok(())
}

fn bot_hook_blocking_error(stage: &str, result: &runtime::HookRunResult) -> Option<AppError> {
    if result.is_denied() {
        Some(AppError::ValidationError(format!(
            "{stage} blocked by hook: {}",
            result.messages().join("\n")
        )))
    } else if result.is_cancelled() {
        Some(AppError::Internal(format!(
            "{stage} hook cancelled: {}",
            result.messages().join("\n")
        )))
    } else if result.is_failed() {
        Some(AppError::Internal(format!(
            "{stage} hook failed: {}",
            result.messages().join("\n")
        )))
    } else {
        None
    }
}

async fn process_inbound_execution(
    state: AppState,
    job: InboundExecutionJob,
) -> InboundExecutionOutcome {
    let execution_started = Instant::now();
    tracing::info!(
        tenant_id = %job.tenant_id,
        agent_id = %job.agent_id,
        channel_id = %job.channel_id,
        inbound_log_id = %job.inbound_log_id,
        agent_task_id = %job.agent_task_id,
        capability = %job.selected_capability,
        external_conversation_id = ?job.external_conversation_id,
        inbound_text_chars = job.user_text.chars().count(),
        "bot inbound execution started"
    );
    if let Some(outcome) = bot_inbound_stop_outcome(&state, &job, "worker_start").await {
        return outcome;
    }
    let _ = agent_ops::mark_task_running(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        agent_ops::PHASE_INTAKE,
        "Bot 入站任务开始执行",
        5,
    )
    .await;
    update_inbound_log_status(
        &state,
        &job.tenant_id,
        &job.inbound_log_id,
        "processing",
        None,
    )
    .await;

    let staged_attachments = if job.attachments.is_empty() && job.staged_attachments.is_empty() {
        Vec::new()
    } else {
        let _ = agent_ops::mark_task_running(
            &state,
            &job.tenant_id,
            &job.agent_task_id,
            agent_ops::PHASE_CONTEXT_LOADING,
            "正在安全接收并索引 Bot 附件",
            12,
        )
        .await;
        match stage_bot_attachments(&state, &job).await {
            Ok(staged) => {
                let _ = agent_ops::add_event(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "bot.attachments_staged",
                    Some(agent_ops::PHASE_CONTEXT_LOADING),
                    Some(agent_ops::STATUS_RUNNING),
                    "info",
                    "Bot 附件已进入用户隔离 Workspace",
                    Some(json!({ "count": staged.len() })),
                )
                .await;
                staged
            }
            Err(error) => {
                let error_message = error.to_string();
                tracing::warn!(
                    tenant_id = %job.tenant_id,
                    agent_task_id = %job.agent_task_id,
                    channel_id = %job.channel_id,
                    error = %error,
                    error_debug = ?error,
                    "Bot attachment staging failed"
                );
                update_inbound_log_status(
                    &state,
                    &job.tenant_id,
                    &job.inbound_log_id,
                    "failed",
                    Some(&error_message),
                )
                .await;
                let _ = agent_ops::fail_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "bot_attachment_staging_failed",
                    &error_message,
                )
                .await;
                if let Err(delivery_error) = send_channel_message(
                    &state,
                    &job.tenant_id,
                    &job.channel_id,
                    BotOutboundMessage {
                        title: Some("AOS Bot 附件接收失败".to_string()),
                        text: format!(
                            "附件未能安全接收，本次任务没有继续执行。请检查文件权限或重新上传。任务编号 #{}。",
                            crate::routes::task_control::short_code_for_task_id(
                                &job.agent_task_id
                            )
                        ),
                        external_conversation_id: job.external_conversation_id.clone(),
                    },
                )
                .await
                {
                    tracing::warn!(
                        tenant_id = %job.tenant_id,
                        agent_task_id = %job.agent_task_id,
                        channel_id = %job.channel_id,
                        error = %delivery_error,
                        "failed to send Bot attachment staging failure reply"
                    );
                }
                return InboundExecutionOutcome::Failed {
                    error_code: "bot_attachment_staging_failed".to_string(),
                    message: error_message,
                };
            }
        }
    };
    if let Some(outcome) = bot_inbound_stop_outcome(&state, &job, "attachment_staging").await {
        return outcome;
    }
    let effective_user_text = if job.user_text.trim().is_empty() && !staged_attachments.is_empty() {
        format!(
            "请分析我上传的附件：{}",
            staged_attachments
                .iter()
                .map(|attachment| attachment.filename.as_str())
                .collect::<Vec<_>>()
                .join("、")
        )
    } else {
        job.user_text.clone()
    };

    if effective_user_text.trim().is_empty() {
        tracing::info!(
            tenant_id = %job.tenant_id,
            agent_id = %job.agent_id,
            channel_id = %job.channel_id,
            inbound_log_id = %job.inbound_log_id,
            capability = %job.selected_capability,
            "bot inbound execution ignored empty message"
        );
        update_inbound_log_status(
            &state,
            &job.tenant_id,
            &job.inbound_log_id,
            "ignored",
            Some("empty inbound message text"),
        )
        .await;
        let _ = agent_ops::complete_task(
            &state,
            &job.tenant_id,
            &job.agent_task_id,
            "空消息已忽略",
            Some(json!({ "ignored": true, "reason": "empty inbound message text" })),
        )
        .await;
        return InboundExecutionOutcome::Ignored {
            reason: "empty inbound message text".to_string(),
        };
    }

    let _ = agent_ops::mark_task_running(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        agent_ops::PHASE_CONTEXT_LOADING,
        "正在加载 Bot 会话上下文",
        20,
    )
    .await;
    let context = match build_bot_conversation_context(&state, &job).await {
        Ok(context) => {
            tracing::info!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                capability = %job.selected_capability,
                external_conversation_id = ?job.external_conversation_id,
                context_messages_loaded = context.loaded_messages,
                context_messages_summarized = context.summarized_messages,
                context_recent_messages = context.recent_messages.len(),
                context_total_chars = context.total_chars,
                has_context_summary = context.summary.is_some(),
                "bot conversation context loaded"
            );
            context
        }
        Err(error) => {
            let error_message = error.to_string();
            tracing::warn!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                capability = %job.selected_capability,
                external_conversation_id = ?job.external_conversation_id,
                elapsed_ms = execution_started.elapsed().as_millis() as u64,
                "bot conversation context load failed: {}",
                error_message
            );
            update_inbound_log_status(
                &state,
                &job.tenant_id,
                &job.inbound_log_id,
                "failed",
                Some(&error_message),
            )
            .await;
            let _ = agent_ops::fail_task(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "context_load_failed",
                &error_message,
            )
            .await;
            return InboundExecutionOutcome::Failed {
                error_code: "context_load_failed".to_string(),
                message: error_message,
            };
        }
    };
    if let Some(outcome) = bot_inbound_stop_outcome(&state, &job, "context_loading").await {
        return outcome;
    }

    let capability_phase = match job.selected_capability.as_str() {
        "super_adversarial" => agent_ops::PHASE_DEBATING,
        "rd_agent" => agent_ops::PHASE_EXECUTING,
        "nl2sql" => agent_ops::PHASE_RETRIEVING,
        _ => agent_ops::PHASE_MODEL_CALLING,
    };
    let execution_policy = BotExecutionPolicy::for_capability(
        &job.selected_capability,
        job.capability_config.as_ref(),
    );
    let _ = agent_ops::mark_task_running(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        capability_phase,
        "正在执行绑定菜单能力",
        45,
    )
    .await;
    let _ = agent_ops::add_event(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        "execution_policy",
        Some(capability_phase),
        Some(agent_ops::STATUS_RUNNING),
        "info",
        "Bot 已应用 Hybrid 执行策略",
        Some(json!({
            "capability": &job.selected_capability,
            "mode": execution_policy.mode.as_str(),
            "syncTimeoutMs": execution_policy.sync_timeout_ms,
            "ackTimeoutMs": execution_policy.ack_timeout_ms,
            "firstAck": execution_policy.first_ack,
            "finalPush": execution_policy.final_push,
        })),
    )
    .await;
    if let Some(outcome) = bot_inbound_stop_outcome(&state, &job, "capability_start").await {
        return outcome;
    }
    let output = match run_text_capability(
        &state,
        &job.tenant_id,
        &job.actor_user_id,
        &job.agent_task_id,
        &job.selected_capability,
        &job.agent_id,
        &job.agent_name,
        job.persona_prompt.as_deref(),
        job.sender_name.as_deref(),
        &context,
        job.capability_config.as_ref(),
        &job.capabilities,
        job.external_conversation_id.as_deref(),
        &job.channel_id,
        &effective_user_text,
        &staged_attachments,
    )
    .await
    {
        Ok(answer) => answer,
        Err(error) => {
            let error_message = error.to_string();
            tracing::warn!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                capability = %job.selected_capability,
                elapsed_ms = execution_started.elapsed().as_millis() as u64,
                "bot inbound capability execution failed: {}",
                error_message
            );
            update_inbound_log_status(
                &state,
                &job.tenant_id,
                &job.inbound_log_id,
                "failed",
                Some(&error_message),
            )
            .await;
            let _ = agent_ops::fail_task(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "capability_execution_failed",
                &error_message,
            )
            .await;
            return InboundExecutionOutcome::Failed {
                error_code: "capability_execution_failed".to_string(),
                message: error_message,
            };
        }
    };
    if let Some(cancelled) = bot_inbound_stop_outcome(&state, &job, "capability_completion").await {
        if let Err(error) = link_bot_capability_output(&state, &job, &output).await {
            let message = format!(
                "Bot cancellation was requested, but the newly created capability resource could not be linked: {error}"
            );
            let _ = agent_ops::fail_task(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "cancel_resource_link_failed",
                &message,
            )
            .await;
            return InboundExecutionOutcome::Failed {
                error_code: "cancel_resource_link_failed".to_string(),
                message,
            };
        }
        if let (Some(resource_type), Some(resource_id)) = (
            output.linked_resource_type.as_deref(),
            output.linked_resource_id.as_deref(),
        ) {
            if let Err(error) = agent_ops::cancel_linked_resource(
                &state,
                &job.tenant_id,
                Some(&job.actor_user_id),
                resource_type,
                resource_id,
            )
            .await
            {
                let message = format!(
                    "Bot cancellation could not be delivered to {resource_type}:{resource_id}: {error}"
                );
                let _ = agent_ops::fail_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "cancel_dispatch_failed",
                    &message,
                )
                .await;
                return InboundExecutionOutcome::Failed {
                    error_code: "cancel_dispatch_failed".to_string(),
                    message,
                };
            }
            let _ =
                agent_ops::sync_linked_resource_status(&state, &job.tenant_id, &job.agent_task_id)
                    .await;
        }
        return cancelled;
    }
    let requested_async_without_resource = execution_policy.mode == BotExecutionMode::Async
        && !output.keep_agent_task_open
        && output.monitor.is_none()
        && output.linked_resource_id.is_none();
    if requested_async_without_resource {
        let _ = agent_ops::add_event(
            &state,
            &job.tenant_id,
            &job.agent_task_id,
            "execution_policy_downgraded",
            Some(capability_phase),
            Some(agent_ops::STATUS_RUNNING),
            "warn",
            "能力未创建可观测异步资源，已按同步回复完成",
            Some(json!({
                "capability": &job.selected_capability,
                "requestedMode": execution_policy.mode.as_str(),
                "reason": "adapter_returned_sync_output_without_linked_resource",
            })),
        )
        .await;
    }
    if let Err(error) = link_bot_capability_output(&state, &job, &output).await {
        let error_message = format!("failed to link Bot capability resource: {error}");
        tracing::error!(
            tenant_id = %job.tenant_id,
            agent_task_id = %job.agent_task_id,
            error = %error,
            error_debug = ?error,
            "failed to link Bot task to its execution resource"
        );
        let _ = agent_ops::fail_task(
            &state,
            &job.tenant_id,
            &job.agent_task_id,
            "capability_resource_link_failed",
            &error_message,
        )
        .await;
        return InboundExecutionOutcome::Failed {
            error_code: "capability_resource_link_failed".to_string(),
            message: error_message,
        };
    }
    let output = output.with_execution_policy(&execution_policy);

    if let Some(outcome) = bot_inbound_stop_outcome(&state, &job, "reply_delivery").await {
        return outcome;
    }

    tracing::info!(
        tenant_id = %job.tenant_id,
        agent_id = %job.agent_id,
        channel_id = %job.channel_id,
        inbound_log_id = %job.inbound_log_id,
        capability = %job.selected_capability,
        reply_chars = output.answer.chars().count(),
        elapsed_ms = execution_started.elapsed().as_millis() as u64,
        "bot inbound capability execution completed"
    );

    let _ = agent_ops::mark_task_running(
        &state,
        &job.tenant_id,
        &job.agent_task_id,
        agent_ops::PHASE_REPLYING,
        "正在发送 Bot 回复",
        85,
    )
    .await;
    tracing::info!(
        tenant_id = %job.tenant_id,
        agent_id = %job.agent_id,
        channel_id = %job.channel_id,
        inbound_log_id = %job.inbound_log_id,
        capability = %job.selected_capability,
        external_conversation_id = ?job.external_conversation_id,
        reply_chars = output.answer.chars().count(),
        "bot inbound reply delivery started"
    );
    let send_result = send_channel_message(
        &state,
        &job.tenant_id,
        &job.channel_id,
        BotOutboundMessage {
            title: Some("AOS Bot 回复".to_string()),
            text: output.answer.clone(),
            external_conversation_id: job.external_conversation_id.clone(),
        },
    )
    .await;
    match send_result {
        Ok(result) => {
            if let Some(outcome) =
                bot_inbound_stop_outcome(&state, &job, "reply_delivery_completion").await
            {
                return outcome;
            }
            let outbound_log_id = result.log_id.clone();
            tracing::info!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                outbound_log_id = %outbound_log_id,
                capability = %job.selected_capability,
                elapsed_ms = execution_started.elapsed().as_millis() as u64,
                "bot inbound reply delivery completed"
            );
            let _ = agent_ops::add_event(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "bot.outbound",
                Some(agent_ops::PHASE_REPLYING),
                Some(agent_ops::STATUS_RUNNING),
                "info",
                "Bot 出站回复已发送",
                Some(json!({
                    "inboundLogId": &job.inbound_log_id,
                    "outboundLogId": &outbound_log_id,
                    "agentId": &job.agent_id,
                    "channelId": &job.channel_id,
                    "capability": &job.selected_capability,
                    "replyChars": output.answer.chars().count(),
                })),
            )
            .await;
            update_inbound_log_status(&state, &job.tenant_id, &job.inbound_log_id, "replied", None)
                .await;
            if output.keep_agent_task_open {
                let _ = agent_ops::add_event(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "first_ack_sent",
                    Some(agent_ops::PHASE_REPLYING),
                    Some(agent_ops::STATUS_RUNNING),
                    "info",
                    "Bot 已发送首条回执，真实能力任务继续执行",
                    Some(json!({
                        "capability": &job.selected_capability,
                        "mode": execution_policy.mode.as_str(),
                        "finalPush": execution_policy.final_push,
                        "outboundLogId": &outbound_log_id,
                    })),
                )
                .await;
                if let Some(state_value) = output.conversation_state.as_ref() {
                    if let Err(error) = attach_outbound_conversation_state(
                        &state,
                        &job.tenant_id,
                        &outbound_log_id,
                        state_value,
                    )
                    .await
                    {
                        tracing::warn!(
                            tenant_id = %job.tenant_id,
                            outbound_log_id = %outbound_log_id,
                            agent_task_id = %job.agent_task_id,
                            "failed to attach bot outbound conversation state: {}",
                            error
                        );
                    }
                }
                if output.waiting_for_input {
                    let _ = agent_ops::mark_task_waiting_input(
                        &state,
                        &job.tenant_id,
                        &job.agent_task_id,
                        agent_ops::PHASE_REPLYING,
                        "Bot 已发送澄清问题，等待用户补充输入",
                        65,
                        output.output_json.clone(),
                    )
                    .await;
                } else {
                    let _ = agent_ops::mark_task_running(
                        &state,
                        &job.tenant_id,
                        &job.agent_task_id,
                        match job.selected_capability.as_str() {
                            "super_adversarial" => agent_ops::PHASE_DEBATING,
                            "rd_agent" => agent_ops::PHASE_EXECUTING,
                            _ => agent_ops::PHASE_REPLYING,
                        },
                        "Bot 回执已发送，真实能力任务继续执行",
                        60,
                    )
                    .await;
                }
                if let Some(monitor) = output.monitor.clone() {
                    tokio::spawn(monitor_bot_async_capability(
                        state.clone(),
                        BotMonitorJob {
                            tenant_id: job.tenant_id.clone(),
                            agent_id: job.agent_id.clone(),
                            channel_id: job.channel_id.clone(),
                            agent_task_id: job.agent_task_id.clone(),
                            external_conversation_id: job.external_conversation_id.clone(),
                            inbound_log_id: job.inbound_log_id.clone(),
                            monitor,
                        },
                    ));
                }
            } else {
                let _ = agent_ops::complete_task(
                    &state,
                    &job.tenant_id,
                    &job.agent_task_id,
                    "Bot 回复已发送",
                    output.output_json.or_else(|| {
                        Some(json!({
                            "replyChars": output.answer.chars().count(),
                            "outboundLogId": &outbound_log_id,
                        }))
                    }),
                )
                .await;
            }
            InboundExecutionOutcome::Succeeded
        }
        Err(error) => {
            let error_message = error.to_string();
            tracing::warn!(
                tenant_id = %job.tenant_id,
                agent_id = %job.agent_id,
                channel_id = %job.channel_id,
                inbound_log_id = %job.inbound_log_id,
                capability = %job.selected_capability,
                elapsed_ms = execution_started.elapsed().as_millis() as u64,
                "bot inbound reply delivery failed: {}",
                error_message
            );
            update_inbound_log_status(
                &state,
                &job.tenant_id,
                &job.inbound_log_id,
                "failed",
                Some(&error_message),
            )
            .await;
            let _ = agent_ops::add_event(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "bot.outbound",
                Some(agent_ops::PHASE_REPLYING),
                Some(agent_ops::STATUS_FAILED),
                "error",
                "Bot 出站回复发送失败",
                Some(json!({
                    "inboundLogId": &job.inbound_log_id,
                    "agentId": &job.agent_id,
                    "channelId": &job.channel_id,
                    "capability": &job.selected_capability,
                    "error": &error_message,
                })),
            )
            .await;
            let _ = agent_ops::fail_task(
                &state,
                &job.tenant_id,
                &job.agent_task_id,
                "reply_delivery_failed",
                &error_message,
            )
            .await;
            InboundExecutionOutcome::Failed {
                error_code: "reply_delivery_failed".to_string(),
                message: error_message,
            }
        }
    }
}

fn bot_queue_worker_id() -> String {
    format!("bot-queue:{}:{}", std::process::id(), uuid::Uuid::new_v4())
}

fn bot_queue_execution_slots() -> &'static Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let limit = std::env::var("AOS_BOT_QUEUE_MAX_CONCURRENT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, 128);
        Arc::new(Semaphore::new(limit))
    })
}

fn bot_queue_max_concurrent_per_tenant() -> usize {
    std::env::var("AOS_BOT_QUEUE_MAX_CONCURRENT_PER_TENANT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(1, 32)
}

fn bot_queue_tenant_slots(tenant_id: &str) -> Arc<Semaphore> {
    static SLOTS: OnceLock<StdMutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slots
        .entry(tenant_id.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(bot_queue_max_concurrent_per_tenant())))
        .clone()
}

#[derive(Debug, Clone)]
struct BotQueueClaim {
    id: String,
    tenant_id: String,
}

fn bot_queue_batch_size() -> i64 {
    std::env::var("AOS_BOT_QUEUE_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(BOT_QUEUE_DEFAULT_BATCH_SIZE)
        .clamp(1, 50)
}

fn bot_queue_poll_interval() -> Duration {
    let millis = std::env::var("AOS_BOT_QUEUE_POLL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(BOT_QUEUE_DEFAULT_POLL_MS)
        .clamp(100, 30_000);
    Duration::from_millis(millis)
}

fn bot_queue_claim_timeout_secs() -> u64 {
    std::env::var("AOS_BOT_QUEUE_CLAIM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(BOT_QUEUE_DEFAULT_CLAIM_TIMEOUT_SECS)
        .clamp(30, 86_400)
}

fn bot_queue_recovery_interval() -> Duration {
    let seconds = std::env::var("AOS_BOT_QUEUE_RECOVERY_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(BOT_QUEUE_DEFAULT_RECOVERY_INTERVAL_SECS)
        .clamp(10, 3_600);
    Duration::from_secs(seconds)
}

async fn recover_stale_bot_queue_claims(state: &AppState, timeout_secs: u64) -> Result<u64> {
    let stale_rows = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(format!(
        r"
        SELECT tenant_id, id, CAST(content_json AS TEXT) AS content_json,
               attempt_count, max_attempts
        FROM bot_message_logs
        WHERE direction = 'inbound'
          AND queue_status = 'claimed'
          AND claimed_at IS NOT NULL
          AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds')
        ",
        timeout_secs
    )))
    .fetch_all(state.control_db())
    .await?;
    if stale_rows.is_empty() {
        return Ok(0);
    }
    let has_dead = stale_rows
        .iter()
        .any(|row| row.get::<u32, _>("attempt_count") >= row.get::<u32, _>("max_attempts"));
    let has_retryable = stale_rows
        .iter()
        .any(|row| row.get::<u32, _>("attempt_count") < row.get::<u32, _>("max_attempts"));
    let dead_sql = format!(
        r"
        UPDATE bot_message_logs
        SET queue_status = 'dead',
            status = 'failed',
            claimed_by = NULL,
            claimed_at = NULL,
            finished_at = CURRENT_TIMESTAMP,
            error_message = 'worker claim timed out and max attempts were exhausted',
            last_error = 'worker claim timed out and max attempts were exhausted',
            updated_at = CURRENT_TIMESTAMP
        WHERE direction = 'inbound'
          AND queue_status = 'claimed'
          AND claimed_at IS NOT NULL
          AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds')
          AND attempt_count >= max_attempts
        ",
        timeout_secs
    );
    let dead_count = if has_dead {
        sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(dead_sql))
            .execute(state.control_db())
            .await?
            .rows_affected()
    } else {
        0
    };
    if dead_count > 0 {
        tracing::warn!(
            dead = dead_count,
            timeout_secs,
            "bot gateway durable queue stale claims exhausted max attempts"
        );
        for row in stale_rows
            .iter()
            .filter(|row| row.get::<u32, _>("attempt_count") >= row.get::<u32, _>("max_attempts"))
        {
            let tenant_id: String = row.get("tenant_id");
            let log_id: String = row.get("id");
            let task_id = parse_json_opt(row.get("content_json")).and_then(|value| {
                value
                    .get("agentTaskId")
                    .or_else(|| value.get("agent_task_id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            });
            if let Some(task_id) = task_id {
                let message = "Bot 入站队列任务超过最大恢复次数，已转入 dead 状态";
                let _ = agent_ops::fail_task(
                    state,
                    &tenant_id,
                    &task_id,
                    "bot_queue_max_attempts_exhausted",
                    message,
                )
                .await;
                let _ = agent_ops::add_event(
                    state,
                    &tenant_id,
                    &task_id,
                    "queue_dead",
                    Some(agent_ops::PHASE_FINALIZING),
                    Some(agent_ops::STATUS_FAILED),
                    "error",
                    message,
                    Some(json!({ "inboundLogId": log_id })),
                )
                .await;
            }
        }
    }
    let sql = format!(
        r"
        UPDATE bot_message_logs
        SET queue_status = 'queued',
            status = 'queued',
            claimed_by = NULL,
            claimed_at = NULL,
            available_at = CURRENT_TIMESTAMP,
            last_error = 'worker claim timed out',
            updated_at = CURRENT_TIMESTAMP
        WHERE direction = 'inbound'
          AND queue_status = 'claimed'
          AND claimed_at IS NOT NULL
          AND claimed_at < datetime(CURRENT_TIMESTAMP, '-{} seconds')
          AND attempt_count < max_attempts
        ",
        timeout_secs
    );
    let recovered_count = if has_retryable {
        sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(sql))
            .execute(state.control_db())
            .await?
            .rows_affected()
    } else {
        0
    };
    Ok(dead_count + recovered_count)
}

async fn claim_bot_queue_jobs(
    state: &AppState,
    worker_id: &str,
    batch_size: i64,
) -> Result<Vec<BotQueueClaim>> {
    let per_tenant_limit = bot_queue_max_concurrent_per_tenant();
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        WITH active_by_tenant AS (
          SELECT tenant_id, COUNT(*) AS active_count
          FROM bot_message_logs
          WHERE direction = 'inbound'
            AND queue_status = 'claimed'
          GROUP BY tenant_id
        )
        SELECT id, tenant_id
        FROM (
          SELECT queued.id, queued.tenant_id, queued.created_at,
                 ROW_NUMBER() OVER (
                   PARTITION BY queued.tenant_id
                   ORDER BY queued.created_at ASC, queued.id ASC
                 ) AS tenant_rank
          FROM bot_message_logs queued
          WHERE queued.direction = 'inbound'
            AND queued.queue_status = 'queued'
            AND queued.available_at <= CURRENT_TIMESTAMP
        ) ranked
        LEFT JOIN active_by_tenant active USING (tenant_id)
        WHERE tenant_rank + COALESCE(active.active_count, 0) <= ?
        ORDER BY created_at ASC, id ASC
        LIMIT ?
        ",
    )
    .bind(i64::try_from(per_tenant_limit).unwrap_or(i64::MAX))
    .bind(batch_size)
    .fetch_all(state.control_db())
    .await?;

    let candidates = rows
        .into_iter()
        .map(|row| BotQueueClaim {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let mut tx = state.control_db.begin().await?;
    let mut update = QueryBuilder::<Sqlite>::new(
        "UPDATE bot_message_logs SET queue_status = 'claimed', status = 'processing', claimed_by = ",
    );
    update
        .push_bind(worker_id)
        .push(", claimed_at = CURRENT_TIMESTAMP, attempt_count = attempt_count + 1, last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id IN (");
    let mut ids = update.separated(", ");
    for candidate in &candidates {
        ids.push_bind(&candidate.id);
    }
    ids.push_unseparated(
        ") AND direction = 'inbound' AND queue_status = 'queued' AND available_at <= CURRENT_TIMESTAMP",
    );
    update.build().execute(&mut *tx).await?;

    let mut selected = QueryBuilder::<Sqlite>::new(
        "SELECT id, tenant_id FROM bot_message_logs WHERE claimed_by = ",
    );
    selected.push_bind(worker_id).push(" AND id IN (");
    let mut ids = selected.separated(", ");
    for candidate in &candidates {
        ids.push_bind(&candidate.id);
    }
    ids.push_unseparated(")");
    let claimed = selected
        .build()
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|row| BotQueueClaim {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
        })
        .collect();
    tx.commit().await?;
    Ok(claimed)
}

async fn load_inbound_execution_job_from_log(
    state: &AppState,
    log_id: &str,
) -> Result<Option<InboundExecutionJob>> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT l.id, l.tenant_id, l.agent_id, l.channel_id, l.platform,
               l.external_conversation_id, CAST(l.content_json AS TEXT) AS content_json,
               c.enabled AS channel_enabled,
               a.name AS agent_name, a.enabled AS agent_enabled, a.default_capability,
               a.persona_prompt, a.created_by
        FROM bot_message_logs l
        INNER JOIN bot_agent_channels c ON c.tenant_id = l.tenant_id AND c.id = l.channel_id
        INNER JOIN bot_agents a ON a.tenant_id = l.tenant_id AND a.id = l.agent_id
        WHERE l.id = ?
          AND l.direction = 'inbound'
        ",
    )
    .bind(log_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(None);
    };

    let tenant_id: String = row.get("tenant_id");
    let agent_id: String = row.get("agent_id");
    let channel_id: String = row.get("channel_id");
    let channel_enabled: bool = row.get("channel_enabled");
    let agent_enabled: bool = row.get("agent_enabled");
    if !channel_enabled || !agent_enabled {
        return Err(AppError::ValidationError(
            "bot agent or channel is disabled".to_string(),
        ));
    }

    let content = parse_json_opt(row.get("content_json")).unwrap_or_else(|| json!({}));
    let capabilities = load_capabilities(state, &tenant_id, &agent_id).await?;
    let agent_name: String = row.get("agent_name");
    let default_capability: String = row.get("default_capability");
    let selected_capability = content
        .get("selectedCapability")
        .or_else(|| content.get("selected_capability"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            select_capability_for_payload(&capabilities, &content, &agent_name, &default_capability)
        })
        .ok_or_else(|| {
            AppError::ValidationError(
                "queued bot inbound message has no selected capability".to_string(),
            )
        })?;
    let agent_task_id = content
        .get("agentTaskId")
        .or_else(|| content.get("agent_task_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::ValidationError("queued bot inbound message has no agent task id".to_string())
        })?;
    let raw_text = payload_text(&content);
    let attachments = inbound_attachments_from_content(&content);
    let staged_attachments = staged_attachments_from_content(&content);
    let user_text =
        strip_selected_trigger_prefix(&capabilities, &selected_capability, &raw_text, &agent_name);
    let actor_user_id: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT COALESCE(initiator_user_id, owner_user_id)
         FROM agent_tasks WHERE tenant_id = ? AND id = ? LIMIT 1",
    )
    .bind(&tenant_id)
    .bind(&agent_task_id)
    .fetch_optional(state.control_db())
    .await?
    .flatten()
    .ok_or_else(|| AppError::Forbidden)?;
    Ok(Some(InboundExecutionJob {
        tenant_id,
        agent_id,
        channel_id,
        external_conversation_id: row.get("external_conversation_id"),
        selected_capability: selected_capability.clone(),
        inbound_log_id: row.get("id"),
        agent_task_id,
        agent_name,
        persona_prompt: row.get("persona_prompt"),
        actor_user_id,
        sender_name: payload_sender_name(&content),
        capability_config: capability_config(&capabilities, &selected_capability).cloned(),
        capabilities,
        user_text,
        attachments,
        staged_attachments,
    }))
}

async fn mark_bot_queue_succeeded(
    state: &AppState,
    tenant_id: &str,
    log_id: &str,
    outcome: &InboundExecutionOutcome,
) {
    if let Err(error) = sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE bot_message_logs
        SET queue_status = 'succeeded',
            claimed_by = NULL,
            claimed_at = NULL,
            finished_at = CURRENT_TIMESTAMP,
            last_error = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
          AND queue_status NOT IN ('cancelling','cancelled')
        ",
    )
    .bind(outcome.error_message())
    .bind(tenant_id)
    .bind(log_id)
    .execute(state.control_db())
    .await
    {
        tracing::warn!(
            tenant_id = %tenant_id,
            log_id = %log_id,
            "failed to mark bot queue job succeeded: {}",
            error
        );
    } else {
        tracing::info!(
            tenant_id = %tenant_id,
            log_id = %log_id,
            outcome = ?outcome,
            "bot gateway durable queue job finalized as succeeded"
        );
    }
}

async fn mark_bot_queue_dead(
    state: &AppState,
    tenant_id: Option<&str>,
    log_id: &str,
    error_code: &str,
    error_message: &str,
) {
    let result = if let Some(tenant_id) = tenant_id {
        sqlx::query::<sqlx::Sqlite>(
            r"
            UPDATE bot_message_logs
            SET queue_status = 'dead',
                status = 'failed',
                error_message = ?,
                last_error = ?,
                claimed_by = NULL,
                claimed_at = NULL,
                finished_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND id = ?
              AND queue_status <> 'cancelled'
            ",
        )
        .bind(error_message)
        .bind(error_message)
        .bind(tenant_id)
        .bind(log_id)
        .execute(state.control_db())
        .await
    } else {
        sqlx::query::<sqlx::Sqlite>(
            r"
            UPDATE bot_message_logs
            SET queue_status = 'dead',
                status = 'failed',
                error_message = ?,
                last_error = ?,
                claimed_by = NULL,
                claimed_at = NULL,
                finished_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
              AND queue_status <> 'cancelled'
            ",
        )
        .bind(error_message)
        .bind(error_message)
        .bind(log_id)
        .execute(state.control_db())
        .await
    };
    if let Err(error) = result {
        tracing::warn!(
            tenant_id = ?tenant_id,
            log_id = %log_id,
            error_code = %error_code,
            "failed to mark bot queue job dead: {}",
            error
        );
    } else {
        tracing::warn!(
            tenant_id = ?tenant_id,
            log_id = %log_id,
            error_code = %error_code,
            error_message = %error_message,
            "bot gateway durable queue job finalized as dead"
        );
    }
}

async fn mark_bot_queue_cancelled(state: &AppState, tenant_id: &str, log_id: &str) {
    if let Err(error) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE bot_message_logs
         SET queue_status = 'cancelled', status = 'cancelled',
             claimed_by = NULL, claimed_at = NULL,
             finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP),
             last_error = 'cancelled by user', error_message = NULL,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND id = ? AND direction = 'inbound'
           AND queue_status IN ('queued','claimed','cancelling','cancelled')",
    )
    .bind(tenant_id)
    .bind(log_id)
    .execute(state.control_db())
    .await
    {
        tracing::error!(
            tenant_id,
            log_id,
            error = %error,
            error_debug = ?error,
            "failed to confirm Bot queue cancellation"
        );
    }
}

async fn load_bot_queue_task_ref(
    state: &AppState,
    log_id: &str,
) -> Result<Option<(String, String)>> {
    let Some(row) = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT tenant_id, CAST(content_json AS TEXT) AS content_json
        FROM bot_message_logs
        WHERE id = ? AND direction = 'inbound'
        ",
    )
    .bind(log_id)
    .fetch_optional(state.control_db())
    .await?
    else {
        return Ok(None);
    };
    let tenant_id: String = row.get("tenant_id");
    let task_id = parse_json_opt(row.get("content_json"))
        .and_then(|value| {
            value
                .get("agentTaskId")
                .or_else(|| value.get("agent_task_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.trim().is_empty());
    Ok(task_id.map(|task_id| (tenant_id, task_id)))
}

async fn finish_bot_queue_job(state: AppState, log_id: String, outcome: InboundExecutionOutcome) {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT tenant_id FROM bot_message_logs WHERE id = ? AND direction = 'inbound'",
    )
    .bind(&log_id)
    .fetch_optional(state.control_db())
    .await;
    let tenant_id = match row {
        Ok(Some(row)) => row.get::<String, _>("tenant_id"),
        Ok(None) => {
            tracing::warn!(log_id = %log_id, "bot queue job disappeared before finalization");
            return;
        }
        Err(error) => {
            tracing::warn!(log_id = %log_id, "failed to load bot queue job for finalization: {}", error);
            return;
        }
    };
    let outcome = match bot_inbound_is_cancelled(&state, &tenant_id, &log_id).await {
        Ok(true) => InboundExecutionOutcome::Cancelled {
            reason: "cancellation took precedence during queue finalization".to_string(),
        },
        Ok(false) => outcome,
        Err(error) => {
            tracing::error!(
                tenant_id,
                log_id,
                error = %error,
                error_debug = ?error,
                "Bot queue finalization stopped because cancellation state is unavailable"
            );
            return;
        }
    };
    if outcome.queue_success() {
        mark_bot_queue_succeeded(&state, &tenant_id, &log_id, &outcome).await;
    } else if matches!(outcome, InboundExecutionOutcome::Cancelled { .. }) {
        mark_bot_queue_cancelled(&state, &tenant_id, &log_id).await;
    } else if let InboundExecutionOutcome::Failed {
        error_code,
        message,
    } = &outcome
    {
        mark_bot_queue_dead(&state, Some(&tenant_id), &log_id, error_code, message).await;
    }
    if let Ok(Some((event_tenant_id, task_id))) = load_bot_queue_task_ref(&state, &log_id).await {
        if let Err(error) =
            agent_ops::sync_linked_resource_status(&state, &event_tenant_id, &task_id).await
        {
            tracing::error!(
                tenant_id = %event_tenant_id,
                task_id,
                log_id,
                error = %error,
                error_debug = ?error,
                "failed to project finalized Bot queue state into AgentOps"
            );
        }
        let (event_type, status, severity, message) = outcome.agent_event();
        let _ = agent_ops::add_event(
            &state,
            &event_tenant_id,
            &task_id,
            event_type,
            Some(agent_ops::PHASE_REPLYING),
            Some(status),
            severity,
            &message,
            Some(json!({
                "inboundLogId": log_id,
                "queueSuccess": outcome.queue_success(),
            })),
        )
        .await;
    }
}

async fn execute_bot_queue_job(state: AppState, log_id: String) {
    if let Ok(Some((tenant_id, _))) = load_bot_queue_task_ref(&state, &log_id).await {
        match bot_inbound_is_cancelled(&state, &tenant_id, &log_id).await {
            Ok(true) => {
                finish_bot_queue_job(
                    state,
                    log_id,
                    InboundExecutionOutcome::Cancelled {
                        reason: "cancelled before queue execution".to_string(),
                    },
                )
                .await;
                return;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::error!(
                    tenant_id,
                    log_id,
                    error = %error,
                    error_debug = ?error,
                    "failed to verify Bot queue cancellation before loading the job"
                );
                return;
            }
        }
    }
    let job = match load_inbound_execution_job_from_log(&state, &log_id).await {
        Ok(Some(job)) => job,
        Ok(None) => {
            mark_bot_queue_dead(
                &state,
                None,
                &log_id,
                "queue_log_not_found",
                "queued bot inbound message was not found",
            )
            .await;
            return;
        }
        Err(error) => {
            let error_message = error.to_string();
            tracing::warn!(
                log_id = %log_id,
                "failed to load bot queue job: {}",
                error_message
            );
            if let Ok(Some((tenant_id, task_id))) = load_bot_queue_task_ref(&state, &log_id).await {
                let _ = agent_ops::fail_task(
                    &state,
                    &tenant_id,
                    &task_id,
                    "queue_job_load_failed",
                    &error_message,
                )
                .await;
            }
            mark_bot_queue_dead(
                &state,
                None,
                &log_id,
                "queue_job_load_failed",
                &error_message,
            )
            .await;
            return;
        }
    };
    let tenant_id = job.tenant_id.clone();
    let agent_task_id = job.agent_task_id.clone();
    let execution = tokio::spawn(process_inbound_execution(state.clone(), job)).await;
    let outcome = match execution {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = format!("bot inbound execution panicked or was cancelled: {error}");
            let _ = agent_ops::fail_task(
                &state,
                &tenant_id,
                &agent_task_id,
                "bot_execution_join_failed",
                &message,
            )
            .await;
            InboundExecutionOutcome::Failed {
                error_code: "bot_execution_join_failed".to_string(),
                message,
            }
        }
    };
    finish_bot_queue_job(state, log_id, outcome).await;
}

fn supplemental_input_text(input: &Value) -> Option<String> {
    input
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            ["text", "input", "message", "content"]
                .into_iter()
                .find_map(|key| {
                    input
                        .get(key)
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bot_resume_log_id(tenant_id: &str, task_id: &str, command_id: &str) -> String {
    let digest = Sha256::digest(format!("{tenant_id}:{task_id}:{command_id}").as_bytes());
    // bot_message_logs.id is VARCHAR(36). Keep this deterministic ID inside
    // that contract so command retries reuse the same queue row.
    format!("br-{}", hex::encode(&digest[..16]))
}

pub(crate) async fn resume_waiting_bot_task_from_agent_ops(
    state: &AppState,
    tenant_id: &str,
    task_id: &str,
    command_id: &str,
    input: &Value,
) -> Result<Value> {
    let text = supplemental_input_text(input).ok_or_else(|| {
        AppError::ValidationError("supplemental input must contain non-empty text".to_string())
    })?;
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT tasks.source_ref, tasks.external_channel_id,
                tasks.external_conversation_id, tasks.external_platform,
                COALESCE(tasks.initiator_user_id, tasks.owner_user_id) AS actor_user_id,
                logs.agent_id, logs.external_user_id,
                CAST(logs.content_json AS TEXT) AS content_json
         FROM agent_tasks tasks
         INNER JOIN bot_message_logs logs
           ON logs.tenant_id = tasks.tenant_id AND logs.id = tasks.source_ref
         WHERE tasks.tenant_id = ? AND tasks.id = ? AND tasks.source = 'bot'
           AND tasks.status = 'waiting_input' AND logs.direction = 'inbound'
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(task_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| {
        AppError::ValidationError(
            "the task is not a resumable Bot task waiting for input".to_string(),
        )
    })?;
    let channel_id = row
        .get::<Option<String>, _>("external_channel_id")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::ValidationError("Bot task channel is missing".to_string()))?;
    let platform = row
        .get::<Option<String>, _>("external_platform")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::ValidationError("Bot task platform is missing".to_string()))?;
    let actor_user_id: String = row.get("actor_user_id");
    let agent_id: String = row.get("agent_id");
    let mut content = parse_json_opt(row.get("content_json")).unwrap_or_else(|| json!({}));
    let object = content.as_object_mut().ok_or_else(|| {
        AppError::ValidationError("Bot task payload is not a JSON object".to_string())
    })?;
    for key in ["text", "message", "content", "body"] {
        object.insert(key.to_string(), Value::String(text.clone()));
    }
    object.insert(
        "agentTaskId".to_string(),
        Value::String(task_id.to_string()),
    );
    object.insert(
        "aos_actor_user_id".to_string(),
        Value::String(actor_user_id),
    );
    object.insert(
        "resumeCommandId".to_string(),
        Value::String(command_id.to_string()),
    );
    object.insert("supplementalInput".to_string(), Value::Bool(true));

    let resume_log_id = bot_resume_log_id(tenant_id, task_id, command_id);
    let inserted = sqlx::query::<sqlx::Sqlite>(
        "INSERT OR IGNORE INTO bot_message_logs
           (id, tenant_id, agent_id, channel_id, direction, platform, external_user_id,
            external_conversation_id, message_type, content_json, status, queue_status,
            available_at, max_attempts)
         VALUES (?, ?, ?, ?, 'inbound', ?, ?, ?, 'text', ?, 'queued', 'queued',
                 CURRENT_TIMESTAMP, ?)",
    )
    .bind(&resume_log_id)
    .bind(tenant_id)
    .bind(agent_id)
    .bind(channel_id)
    .bind(platform)
    .bind(row.get::<Option<String>, _>("external_user_id"))
    .bind(row.get::<Option<String>, _>("external_conversation_id"))
    .bind(content)
    .bind(BOT_QUEUE_MAX_ATTEMPTS)
    .execute(state.control_db())
    .await?;
    agent_ops::mark_task_running(
        state,
        tenant_id,
        task_id,
        agent_ops::PHASE_INTAKE,
        "已收到补充信息，原任务重新进入执行队列",
        66,
    )
    .await?;
    Ok(json!({
        "status": "resume_queued",
        "taskId": task_id,
        "queueLogId": resume_log_id,
        "reused": inserted.rows_affected() == 0,
    }))
}

pub fn start_bot_gateway_queue_worker(state: AppState) {
    let worker_id = bot_queue_worker_id();
    let batch_size = bot_queue_batch_size();
    let poll_interval = bot_queue_poll_interval();
    let claim_timeout_secs = bot_queue_claim_timeout_secs();
    let recovery_interval = bot_queue_recovery_interval();
    tracing::info!(
        worker_id = %worker_id,
        batch_size,
        poll_interval_ms = poll_interval.as_millis() as u64,
        claim_timeout_secs,
        recovery_interval_secs = recovery_interval.as_secs(),
        max_attempts = BOT_QUEUE_MAX_ATTEMPTS,
        "bot gateway durable queue worker started"
    );
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut next_recovery_at =
            tokio::time::Instant::now() + recovery_interval.min(Duration::from_secs(10));
        let mut failure_delay = poll_interval;
        loop {
            ticker.tick().await;
            let now = tokio::time::Instant::now();
            if now >= next_recovery_at {
                next_recovery_at = now + recovery_interval;
                match recover_stale_bot_queue_claims(&state, claim_timeout_secs).await {
                    Ok(count) if count > 0 => {
                        tracing::warn!(
                            worker_id = %worker_id,
                            recovered = count,
                            "bot gateway durable queue recovered stale claims"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            worker_id = %worker_id,
                            error = %error,
                            error_debug = ?error,
                            "bot gateway durable queue stale claim recovery failed"
                        );
                    }
                }
            }
            let available = bot_queue_execution_slots().available_permits();
            if available == 0 {
                continue;
            }
            let claim_limit = batch_size.min(i64::try_from(available).unwrap_or(i64::MAX));
            let claimed = match claim_bot_queue_jobs(&state, &worker_id, claim_limit).await {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::warn!(
                        worker_id = %worker_id,
                        error = %error,
                        error_debug = ?error,
                        "bot gateway durable queue claim failed"
                    );
                    tokio::time::sleep(failure_delay).await;
                    failure_delay = (failure_delay * 2)
                        .max(Duration::from_secs(2))
                        .min(Duration::from_secs(15));
                    continue;
                }
            };
            failure_delay = poll_interval;
            for claim in claimed {
                let tenant_permit = match bot_queue_tenant_slots(&claim.tenant_id)
                    .acquire_owned()
                    .await
                {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::error!("bot queue tenant execution semaphore closed");
                        break;
                    }
                };
                let permit = match bot_queue_execution_slots().clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::error!("bot queue execution semaphore closed");
                        break;
                    }
                };
                tracing::info!(
                    worker_id = %worker_id,
                    tenant_id = %claim.tenant_id,
                    log_id = %claim.id,
                    "bot gateway durable queue job claimed"
                );
                let job_state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _tenant_permit = tenant_permit;
                    execute_bot_queue_job(job_state, claim.id).await;
                });
            }
        }
    });
}

pub async fn enqueue_inbound_payload(
    state: AppState,
    channel_id: String,
    payload: Value,
) -> Result<Value> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT c.tenant_id, c.agent_id, c.platform, c.enabled,
               a.name AS agent_name, a.enabled AS agent_enabled, a.default_capability,
               a.persona_prompt, a.created_by
        FROM bot_agent_channels c
        INNER JOIN bot_agents a ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
        WHERE c.id = ?
        ",
    )
    .bind(&channel_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{channel_id}' not found")))?;

    let tenant_id: String = row.get("tenant_id");
    let agent_id: String = row.get("agent_id");
    let platform: String = row.get("platform");
    let channel_enabled: bool = row.get("enabled");
    let agent_enabled: bool = row.get("agent_enabled");
    let default_capability: String = row.get("default_capability");
    let agent_name: String = row.get("agent_name");
    if !channel_enabled || !agent_enabled {
        return Err(AppError::Forbidden);
    }

    let mut payload = normalize_platform_inbound_payload(&platform, payload);

    let external_user_id = external_user_id_from_payload(&payload);
    let external_conversation_id = external_conversation_id_from_payload(&payload);
    let external_conversation_id_for_task = external_conversation_id.clone();
    let raw_text = payload_text(&payload);
    let sender_name = payload_sender_name(&payload);
    let external_user_id = external_user_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::ValidationError(
                "Bot platform did not provide a stable external user id".to_string(),
            )
        })?;
    if let Some(code) = external_pairing_code(&raw_text) {
        if !pairing_allowed_in_private_conversation(&platform, &payload) {
            let text = "为保护私人任务，身份绑定只能在可确认的 Bot 私聊中完成。请打开与该 Bot 的私聊后重新发送绑定码。";
            let _ = send_channel_message(
                &state,
                &tenant_id,
                &channel_id,
                BotOutboundMessage {
                    title: Some("AOS 身份绑定".to_string()),
                    text: text.to_string(),
                    external_conversation_id,
                },
            )
            .await;
            return Ok(json!({
                "ok": false,
                "status": "pairing_private_conversation_required",
                "message": text,
            }));
        }
        let paired = crate::routes::task_control::claim_external_identity_pairing(
            &state,
            &tenant_id,
            &code,
            &platform,
            &external_user_id,
            Some(&channel_id),
            external_conversation_id.as_deref(),
            sender_name.as_deref(),
        )
        .await?;
        let text = if paired.is_some() {
            "身份绑定成功。现在可以在此查看、控制并接收你自己的 AOS 任务。"
        } else {
            "绑定码无效、已使用或已过期，请在 AOS WebUI 的「Bot 网关 → 身份绑定」中重新生成。"
        };
        let _ = send_channel_message(
            &state,
            &tenant_id,
            &channel_id,
            BotOutboundMessage {
                title: Some("AOS 身份绑定".to_string()),
                text: text.to_string(),
                external_conversation_id,
            },
        )
        .await;
        return Ok(json!({
            "ok": paired.is_some(),
            "status": if paired.is_some() { "paired" } else { "pairing_failed" },
            "message": text,
        }));
    }
    let actor_user_id = crate::routes::task_control::resolve_external_identity_user(
        &state,
        &tenant_id,
        &platform,
        &external_user_id,
    )
    .await?;
    let Some(actor_user_id) = actor_user_id else {
        let text = "此社交账号尚未绑定 AOS 用户。请在 AOS WebUI 的「Bot 网关 → 身份绑定」中生成绑定码，然后发送“绑定 ABCD1234”或“bind ABCD1234”。";
        let _ = send_channel_message(
            &state,
            &tenant_id,
            &channel_id,
            BotOutboundMessage {
                title: Some("需要绑定身份".to_string()),
                text: text.to_string(),
                external_conversation_id,
            },
        )
        .await;
        return Ok(json!({
            "ok": true,
            "status": "identity_required",
            "message": text,
        }));
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "aos_actor_user_id".to_string(),
            Value::String(actor_user_id.clone()),
        );
    }
    let external_message_id = external_message_id_from_payload(&payload);
    let task_command_idempotency = external_message_id
        .as_deref()
        .map(|message_id| {
            bot_message_idempotency_key("task-command", &platform, &channel_id, message_id)
        })
        .unwrap_or_else(|| format!("bot-task-command:{}", uuid::Uuid::new_v4()));
    let capabilities = load_capabilities(&state, &tenant_id, &agent_id).await?;

    // Read-only task control is a high-priority fast lane. It must run before
    // capability/prefix selection so a progress or cancellation question can
    // interrupt a long research session instead of becoming another queued
    // model turn. The handler is idempotent, so the legacy post-selection call
    // below remains safe for prefixed commands.
    if let Some(answer) = crate::routes::task_control::handle_external_task_request(
        &state,
        &tenant_id,
        &actor_user_id,
        &raw_text,
        &task_command_idempotency,
        external_conversation_id.as_deref(),
    )
    .await?
    {
        let _ = send_channel_message(
            &state,
            &tenant_id,
            &channel_id,
            BotOutboundMessage {
                title: Some("AOS 任务指挥".to_string()),
                text: answer.clone(),
                external_conversation_id,
            },
        )
        .await;
        return Ok(json!({
            "ok": true,
            "status": "task_command_handled",
            "message": answer,
        }));
    }
    let Some(selected_capability) =
        select_capability_for_payload(&capabilities, &payload, &agent_name, &default_capability)
    else {
        let mentioned = payload_mentions_bot(&payload, &agent_name);
        let default_binding = capabilities
            .iter()
            .find(|item| item.enabled && item.capability_key == default_capability);
        tracing::info!(
            tenant_id = %tenant_id,
            agent_id = %agent_id,
            channel_id = %channel_id,
            platform = %platform,
            default_capability = %default_capability,
            default_capability_bound = default_binding.is_some(),
            mentioned,
            "bot gateway inbound message ignored because no prefix or mention matched"
        );
        return Ok(json!({
            "ok": true,
            "status": "ignored",
            "message": "No enabled bound capability matched this inbound message."
        }));
    };
    let user_text =
        strip_selected_trigger_prefix(&capabilities, &selected_capability, &raw_text, &agent_name);

    if let Some(answer) = crate::routes::task_control::handle_external_task_request(
        &state,
        &tenant_id,
        &actor_user_id,
        &user_text,
        &task_command_idempotency,
        external_conversation_id.as_deref(),
    )
    .await?
    {
        let _ = send_channel_message(
            &state,
            &tenant_id,
            &channel_id,
            BotOutboundMessage {
                title: Some("AOS 任务指挥".to_string()),
                text: answer.clone(),
                external_conversation_id,
            },
        )
        .await;
        return Ok(json!({
            "ok": true,
            "status": "task_command_handled",
            "message": answer,
        }));
    }

    let inbound_log_id = uuid::Uuid::new_v4().to_string();
    let agent_task_idempotency_key = external_message_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|message_id| bot_message_idempotency_key("task", &platform, &channel_id, message_id))
        .unwrap_or_else(|| format!("bot:{inbound_log_id}"));

    let hook_result = run_lifecycle_hooks(
        &state,
        &tenant_id,
        "bot",
        HookEventType::BotMessageReceived,
        "bot.inbound_message",
        serde_json::json!({
            "agentId": &agent_id,
            "channelId": &channel_id,
            "platform": &platform,
            "capability": &selected_capability,
            "externalUserId": &external_user_id,
            "externalConversationId": &external_conversation_id,
            "rawText": &raw_text,
            "userText": &user_text,
            "senderName": &sender_name,
            "payload": &payload,
        }),
        None,
        false,
    )
    .await?;
    if let Some(error) = bot_hook_blocking_error("bot_message_received", &hook_result) {
        tracing::warn!(
            tenant_id = %tenant_id,
            agent_id = %agent_id,
            channel_id = %channel_id,
            platform = %platform,
            capability = %selected_capability,
            "bot gateway inbound message blocked by lifecycle hook: {}",
            error
        );
        return Err(error);
    }

    tracing::info!(
        tenant_id = %tenant_id,
        agent_id = %agent_id,
        channel_id = %channel_id,
        platform = %platform,
        capability = %selected_capability,
        external_conversation_id = ?external_conversation_id,
        inbound_text_chars = raw_text.chars().count(),
        "bot gateway inbound message queued"
    );

    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO bot_message_logs
            (id, tenant_id, agent_id, channel_id, direction, platform, external_user_id,
             external_conversation_id, message_type, content_json, status)
        VALUES (?, ?, ?, ?, 'inbound', ?, ?, ?, 'text', ?, 'queued')
        ",
    )
    .bind(&inbound_log_id)
    .bind(&tenant_id)
    .bind(&agent_id)
    .bind(&channel_id)
    .bind(&platform)
    .bind(Some(external_user_id))
    .bind(external_conversation_id)
    .bind(&payload)
    .execute(state.control_db())
    .await?;

    let agent_task_outcome = agent_ops::create_task_with_outcome(
        &state,
        CreateAgentTaskInput {
            tenant_id: tenant_id.clone(),
            source: "bot".to_string(),
            source_ref: Some(inbound_log_id.clone()),
            source_label: Some(format!("{platform} / {agent_name}")),
            capability_key: selected_capability.clone(),
            agent_id: Some(agent_id.clone()),
            agent_name: Some(agent_name.clone()),
            title: if user_text.trim().is_empty() {
                format!("{} Bot 入站消息", selected_capability)
            } else {
                user_text.chars().take(80).collect()
            },
            summary: Some(format!(
                "来自 {platform} 的 Bot 入站任务，能力：{selected_capability}"
            )),
            owner_user_id: Some(actor_user_id.clone()),
            correlation_id: Some(inbound_log_id.clone()),
            parent_task_id: None,
            external_platform: Some(platform.clone()),
            external_channel_id: Some(channel_id.clone()),
            external_conversation_id: external_conversation_id_for_task.clone(),
            external_message_id,
            idempotency_key: Some(agent_task_idempotency_key),
            input_json: Some(json!({
                "rawTextChars": raw_text.chars().count(),
                "userTextChars": user_text.chars().count(),
                "senderName": sender_name.clone(),
                "inboundLogId": inbound_log_id.clone(),
            })),
        },
    )
    .await?;
    let agent_task_id = agent_task_outcome.id;
    if !agent_task_outcome.reused {
        if let Err(error) = agent_ops::link_task_resource(
            &state,
            &tenant_id,
            &agent_task_id,
            "bot_inbound_message",
            &inbound_log_id,
        )
        .await
        {
            let error_message = format!("failed to link durable Bot inbound resource: {error}");
            sqlx::query::<sqlx::Sqlite>(
                "UPDATE bot_message_logs
                 SET status = 'failed', queue_status = 'dead', error_message = ?,
                     last_error = ?, finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE tenant_id = ? AND id = ?",
            )
            .bind(&error_message)
            .bind(&error_message)
            .bind(&tenant_id)
            .bind(&inbound_log_id)
            .execute(state.control_db())
            .await?;
            let _ = agent_ops::fail_task(
                &state,
                &tenant_id,
                &agent_task_id,
                "bot_inbound_resource_link_failed",
                &error_message,
            )
            .await;
            return Err(AppError::Internal(error_message));
        }
    }
    let _ = agent_ops::add_event(
        &state,
        &tenant_id,
        &agent_task_id,
        "bot.inbound",
        Some(agent_ops::PHASE_INTAKE),
        Some(agent_ops::STATUS_CREATED),
        "info",
        "Bot 入站消息已进入 AgentOps",
        Some(json!({
            "inboundLogId": &inbound_log_id,
            "agentId": &agent_id,
            "channelId": &channel_id,
            "platform": &platform,
            "capability": &selected_capability,
            "externalConversationId": &external_conversation_id_for_task,
            "rawTextChars": raw_text.chars().count(),
            "userTextChars": user_text.chars().count(),
            "duplicateDelivery": agent_task_outcome.reused,
        })),
    )
    .await;

    let mut log_payload = payload.clone();
    if let Some(object) = log_payload.as_object_mut() {
        object.insert(
            "agentTaskId".to_string(),
            Value::String(agent_task_id.clone()),
        );
        object.insert(
            "selectedCapability".to_string(),
            Value::String(selected_capability.clone()),
        );
        if agent_task_outcome.reused {
            object.insert("duplicateDelivery".to_string(), Value::Bool(true));
            object.insert(
                "duplicateReason".to_string(),
                Value::String("reused existing agent task".to_string()),
            );
        }
    }
    if agent_task_outcome.reused {
        sqlx::query::<sqlx::Sqlite>(
            r"
            UPDATE bot_message_logs
            SET content_json = ?,
                status = 'duplicate',
                queue_status = 'succeeded',
                finished_at = CURRENT_TIMESTAMP,
                last_error = NULL,
                error_message = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND id = ?
            ",
        )
        .bind(&log_payload)
        .bind(&tenant_id)
        .bind(&inbound_log_id)
        .execute(state.control_db())
        .await?;
    } else {
        sqlx::query::<sqlx::Sqlite>(
            r"
            UPDATE bot_message_logs
            SET content_json = ?,
                queue_status = 'queued',
                available_at = CURRENT_TIMESTAMP,
                max_attempts = ?,
                updated_at = CURRENT_TIMESTAMP
            WHERE tenant_id = ? AND id = ?
            ",
        )
        .bind(&log_payload)
        .bind(BOT_QUEUE_MAX_ATTEMPTS)
        .bind(&tenant_id)
        .bind(&inbound_log_id)
        .execute(state.control_db())
        .await?;
    }

    tracing::info!(
        tenant_id = %tenant_id,
        agent_id = %agent_id,
        channel_id = %channel_id,
        platform = %platform,
        capability = %selected_capability,
        inbound_log_id = %inbound_log_id,
        agent_task_id = %agent_task_id,
        duplicate_delivery = agent_task_outcome.reused,
        "bot gateway inbound execution accepted durably"
    );

    Ok(json!({
        "ok": true,
        "status": if agent_task_outcome.reused { "duplicate" } else { "queued" },
        "capability": selected_capability,
        "log_id": inbound_log_id,
        "agent_task_id": agent_task_id,
        "duplicate": agent_task_outcome.reused,
        "message": if agent_task_outcome.reused {
            "Bot Gateway received a duplicate delivery and reused the existing Agent task."
        } else {
            "Bot Gateway has received the message and queued capability execution."
        }
    }))
}

async fn replace_capabilities(
    state: &AppState,
    tenant_id: &str,
    agent_id: &str,
    capabilities: &[CapabilityBindingInput],
) -> Result<()> {
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM bot_agent_capabilities WHERE tenant_id = ? AND agent_id = ?",
    )
    .bind(tenant_id)
    .bind(agent_id)
    .execute(state.control_db())
    .await?;

    let mut items = capabilities
        .iter()
        .filter_map(capability_binding_parts)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items.dedup_by(|a, b| a.0 == b.0);

    let fallback_key = items
        .iter()
        .find(|(key, _)| key == "aos_router")
        .or_else(|| items.first())
        .map(|(key, _)| key.clone());
    for (capability_key, config_json) in &mut items {
        let config = config_json.get_or_insert_with(|| json!({}));
        if let Some(object) = config.as_object_mut() {
            let fallback_is_explicit = object.contains_key("fallback_when_no_prefix");
            if !fallback_is_explicit {
                let has_prefix = object
                    .get("trigger_prefixes")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item.as_str().is_some_and(|value| !value.trim().is_empty()))
                    });
                object.insert(
                    "fallback_when_no_prefix".to_string(),
                    Value::Bool(
                        !has_prefix && fallback_key.as_deref() == Some(capability_key.as_str()),
                    ),
                );
            }
        }
    }

    for (capability_key, config_json) in items {
        sqlx::query::<sqlx::Sqlite>(
            r"
            INSERT INTO bot_agent_capabilities
                (id, tenant_id, agent_id, capability_key, enabled, config_json)
            VALUES (?, ?, ?, ?, 1, ?)
            ",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(agent_id)
        .bind(capability_key)
        .bind(config_json)
        .execute(state.control_db())
        .await?;
    }
    Ok(())
}

async fn list_agents(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<ListResponse<BotAgentInfo>>> {
    let rows = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, tenant_id, name, description, enabled, default_capability,
               persona_prompt, created_by, created_at, updated_at
        FROM bot_agents
        WHERE tenant_id = ?
        ORDER BY updated_at DESC, created_at DESC
        LIMIT ? OFFSET ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(params.limit())
    .bind(params.offset())
    .fetch_all(state.control_db())
    .await?;
    let total: (i64,) = sqlx::query_as::<sqlx::Sqlite, _>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_agents WHERE tenant_id = ?",
    )
    .bind(&claims.tenant_id)
    .fetch_one(state.control_db())
    .await?;

    let mut items = Vec::new();
    for row in rows {
        items.push(row_to_agent(&state, &claims.tenant_id, row).await?);
    }
    Ok(Json(ListResponse {
        items,
        total: total.0,
    }))
}

async fn create_agent(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<BotAgentInfo>> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(AppError::ValidationError(
            "agent name is required".to_string(),
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let capabilities = if req.capabilities.is_empty() {
        default_capability_bindings()
    } else {
        req.capabilities
    };
    let preferred_capability = first_capability_key(&capabilities);
    let default_capability = preferred_capability
        .filter(|key| key == "aos_router")
        .or_else(|| clean_opt(req.default_capability))
        .or_else(|| first_capability_key(&capabilities))
        .unwrap_or_else(|| "aos_router".to_string());

    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO bot_agents
            (id, tenant_id, name, description, enabled, default_capability, persona_prompt, created_by)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(name)
    .bind(clean_opt(req.description))
    .bind(req.enabled.unwrap_or(true))
    .bind(default_capability)
    .bind(clean_opt(req.persona_prompt))
    .bind(&claims.sub)
    .execute(state.control_db())
    .await?;
    replace_capabilities(&state, &claims.tenant_id, &id, &capabilities).await?;

    get_agent(State(state), Extension(claims), Path(id)).await
}

async fn get_agent(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<BotAgentInfo>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, tenant_id, name, description, enabled, default_capability,
               persona_prompt, created_by, created_at, updated_at
        FROM bot_agents WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot agent '{id}' not found")))?;
    Ok(Json(row_to_agent(&state, &claims.tenant_id, row).await?))
}

async fn update_agent(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<BotAgentInfo>> {
    let exists =
        sqlx::query::<sqlx::Sqlite>("SELECT id FROM bot_agents WHERE tenant_id = ? AND id = ?")
            .bind(&claims.tenant_id)
            .bind(&id)
            .fetch_optional(state.control_db())
            .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!("bot agent '{id}' not found")));
    }

    let name = clean_opt(req.name);
    let preferred_capability = req
        .capabilities
        .as_ref()
        .and_then(|items| first_capability_key(items));
    let preferred_router = preferred_capability
        .as_ref()
        .filter(|key| key.as_str() == "aos_router")
        .cloned();
    let default_capability = preferred_router
        .or_else(|| clean_opt(req.default_capability))
        .or(preferred_capability);

    sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE bot_agents
        SET name = COALESCE(?, name),
            description = COALESCE(?, description),
            enabled = COALESCE(?, enabled),
            default_capability = COALESCE(?, default_capability),
            persona_prompt = COALESCE(?, persona_prompt)
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(name)
    .bind(clean_opt(req.description))
    .bind(req.enabled)
    .bind(default_capability)
    .bind(clean_opt(req.persona_prompt))
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(state.control_db())
    .await?;

    if let Some(capabilities) = req.capabilities {
        replace_capabilities(&state, &claims.tenant_id, &id, &capabilities).await?;
    }

    get_agent(State(state), Extension(claims), Path(id)).await
}

async fn delete_agent(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM bot_agent_channels WHERE tenant_id = ? AND agent_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(state.control_db())
    .await?;
    sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM bot_agent_capabilities WHERE tenant_id = ? AND agent_id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(state.control_db())
    .await?;
    let affected =
        sqlx::query::<sqlx::Sqlite>("DELETE FROM bot_agents WHERE tenant_id = ? AND id = ?")
            .bind(&claims.tenant_id)
            .bind(&id)
            .execute(state.control_db())
            .await?
            .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound(format!("bot agent '{id}' not found")));
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn list_channels(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ChannelListQuery>,
) -> Result<Json<ListResponse<BotAgentChannelInfo>>> {
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let mut sql = String::from(
        "SELECT id, agent_id, platform, name, enabled, inbound_mode, inbound_secret, inbound_status, inbound_error, inbound_last_seen_at, inbound_last_message_at, outbound_webhook_url, outbound_token, signing_secret, outbound_signing_secret, CAST(config_json AS TEXT) AS config_json, created_at, updated_at FROM bot_agent_channels WHERE tenant_id = ?",
    );
    if params.agent_id.is_some() {
        sql.push_str(" AND agent_id = ?");
    }
    sql.push_str(" ORDER BY updated_at DESC LIMIT ? OFFSET ?");

    let mut query = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(sql)).bind(&claims.tenant_id);
    if let Some(agent_id) = &params.agent_id {
        query = query.bind(agent_id);
    }
    let rows = query
        .bind(pagination.limit())
        .bind(pagination.offset())
        .fetch_all(state.control_db())
        .await?;

    let total = if let Some(agent_id) = &params.agent_id {
        sqlx::query_as::<sqlx::Sqlite, (i64,)>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_agent_channels WHERE tenant_id = ? AND agent_id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(agent_id)
        .fetch_one(state.control_db())
        .await?
        .0
    } else {
        sqlx::query_as::<sqlx::Sqlite, (i64,)>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_agent_channels WHERE tenant_id = ?",
        )
        .bind(&claims.tenant_id)
        .fetch_one(state.control_db())
        .await?
        .0
    };

    Ok(Json(ListResponse {
        items: rows.into_iter().map(row_to_channel).collect(),
        total,
    }))
}

async fn create_channel(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateChannelRequest>,
) -> Result<Json<BotAgentChannelInfo>> {
    let exists =
        sqlx::query::<sqlx::Sqlite>("SELECT id FROM bot_agents WHERE tenant_id = ? AND id = ?")
            .bind(&claims.tenant_id)
            .bind(&req.agent_id)
            .fetch_optional(state.control_db())
            .await?;
    if exists.is_none() {
        return Err(AppError::NotFound(format!(
            "bot agent '{}' not found",
            req.agent_id
        )));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let platform = normalize_platform(&req.platform);
    let inbound_mode = normalize_inbound_mode(req.inbound_mode)?;
    validate_platform_inbound_mode(&platform, &inbound_mode)?;
    let inbound_secret = encrypt_bot_channel_secret(
        req.inbound_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::InboundSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let outbound_token = encrypt_bot_channel_secret(
        req.outbound_token,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::OutboundToken,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let outbound_signing_secret = encrypt_bot_channel_secret(
        req.outbound_signing_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::OutboundSigningSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let signing_secret = encrypt_bot_channel_secret(
        req.signing_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::SigningSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    sqlx::query::<sqlx::Sqlite>(
        r"
        INSERT INTO bot_agent_channels
            (id, tenant_id, agent_id, platform, name, enabled, inbound_mode, inbound_secret,
             outbound_webhook_url, outbound_token, outbound_signing_secret, signing_secret, config_json)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&req.agent_id)
    .bind(platform)
    .bind(req.name.trim())
    .bind(req.enabled.unwrap_or(true))
    .bind(inbound_mode)
    .bind(inbound_secret)
    .bind(clean_opt(req.outbound_webhook_url))
    .bind(outbound_token)
    .bind(outbound_signing_secret)
    .bind(signing_secret)
    .bind(json_opt(req.config_json))
    .execute(state.control_db())
    .await?;
    get_channel(State(state), Extension(claims), Path(id)).await
}

async fn get_channel(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<BotAgentChannelInfo>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT id, agent_id, platform, name, enabled, inbound_mode, inbound_secret,
               inbound_status, inbound_error, inbound_last_seen_at, inbound_last_message_at,
               outbound_webhook_url, outbound_token, signing_secret, outbound_signing_secret, CAST(config_json AS TEXT) AS config_json,
               created_at, updated_at
        FROM bot_agent_channels WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{id}' not found")))?;
    Ok(Json(row_to_channel(row)))
}

async fn update_channel(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateChannelRequest>,
) -> Result<Json<BotAgentChannelInfo>> {
    let current = sqlx::query_as::<sqlx::Sqlite, (String, String)>(
        "SELECT platform, inbound_mode FROM bot_agent_channels WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{id}' not found")))?;

    let platform = clean_opt(req.platform).map(|value| normalize_platform(&value));
    let name = clean_opt(req.name);
    let inbound_mode = normalize_inbound_mode_update(req.inbound_mode)?;
    let effective_platform = platform.as_deref().unwrap_or(&current.0);
    let effective_inbound_mode = inbound_mode.as_deref().unwrap_or(&current.1);
    validate_platform_inbound_mode(effective_platform, effective_inbound_mode)?;

    let inbound_secret = encrypt_bot_channel_secret(
        req.inbound_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::InboundSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let outbound_token = encrypt_bot_channel_secret(
        req.outbound_token,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::OutboundToken,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let outbound_signing_secret = encrypt_bot_channel_secret(
        req.outbound_signing_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::OutboundSigningSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;
    let signing_secret = encrypt_bot_channel_secret(
        req.signing_secret,
        &claims.tenant_id,
        &id,
        BotChannelSecretKind::SigningSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret encrypt failed: {error}")))?;

    sqlx::query::<sqlx::Sqlite>(
        r"
        UPDATE bot_agent_channels
        SET platform = COALESCE(?, platform),
            name = COALESCE(?, name),
            enabled = COALESCE(?, enabled),
            inbound_mode = COALESCE(?, inbound_mode),
            inbound_secret = COALESCE(?, inbound_secret),
            outbound_webhook_url = COALESCE(?, outbound_webhook_url),
            outbound_token = COALESCE(?, outbound_token),
            outbound_signing_secret = COALESCE(?, outbound_signing_secret),
            signing_secret = COALESCE(?, signing_secret),
            config_json = COALESCE(?, config_json)
        WHERE tenant_id = ? AND id = ?
        ",
    )
    .bind(platform)
    .bind(name)
    .bind(req.enabled)
    .bind(inbound_mode)
    .bind(inbound_secret)
    .bind(clean_opt(req.outbound_webhook_url))
    .bind(outbound_token)
    .bind(outbound_signing_secret)
    .bind(signing_secret)
    .bind(json_opt(req.config_json))
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(state.control_db())
    .await?;
    get_channel(State(state), Extension(claims), Path(id)).await
}

async fn delete_channel(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let affected = sqlx::query::<sqlx::Sqlite>(
        "DELETE FROM bot_agent_channels WHERE tenant_id = ? AND id = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .execute(state.control_db())
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(AppError::NotFound(format!("bot channel '{id}' not found")));
    }
    Ok(Json(json!({ "deleted": true })))
}

fn test_send_error_requires_identity_binding(has_active_identity: bool, error: &str) -> bool {
    if has_active_identity {
        return false;
    }
    let detail = error.to_ascii_lowercase();
    detail.contains("default recipient")
        || detail.contains("default recipient/chat_id")
        || detail.contains("inbound chat_id")
}

async fn test_channel(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<TestChannelRequest>,
) -> Result<Json<BotOutboundResult>> {
    let mut external_conversation_id = clean_opt(req.external_conversation_id);
    let identity = sqlx::query_as::<sqlx::Sqlite, (Option<String>,)>(
        "SELECT external_conversation_id
         FROM agent_external_identity_links
         WHERE tenant_id = ? AND user_id = ? AND channel_id = ?
           AND status = 'active' AND revoked_at IS NULL
         ORDER BY COALESCE(last_seen_at, verified_at) DESC, updated_at DESC
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&id)
    .fetch_optional(state.control_db())
    .await?;
    let has_active_identity = identity.is_some();
    if external_conversation_id.is_none() {
        external_conversation_id = identity
            .as_ref()
            .and_then(|(conversation_id,)| clean_opt(conversation_id.clone()));
    }
    if external_conversation_id.is_none() && has_active_identity {
        external_conversation_id = sqlx::query_scalar::<sqlx::Sqlite, String>(
            r#"
            SELECT external_conversation_id
            FROM bot_message_logs
            WHERE tenant_id = ? AND channel_id = ? AND direction = 'inbound'
              AND external_conversation_id IS NOT NULL AND external_conversation_id <> ''
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(&claims.tenant_id)
        .bind(&id)
        .fetch_optional(state.control_db())
        .await?;
    }
    let message = BotOutboundMessage {
        title: req.title.or_else(|| Some("AOS Bot 网关测试".to_string())),
        text: req
            .text
            .and_then(|value| {
                let trimmed = value.trim().to_string();
                (!trimmed.is_empty()).then_some(trimmed)
            })
            .unwrap_or_else(|| "这是一条来自 AOS Bot 网关的测试通知。".to_string()),
        external_conversation_id,
    };
    let result = match send_channel_message(&state, &claims.tenant_id, &id, message).await {
        Ok(result) => result,
        Err(error) => {
            if test_send_error_requires_identity_binding(has_active_identity, &error.to_string()) {
                return Err(AppError::ValidationError(
                    "bot_identity_binding_required".to_string(),
                ));
            }
            return Err(error);
        }
    };
    Ok(Json(result))
}

async fn list_logs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<LogListQuery>,
) -> Result<Json<ListResponse<BotMessageLogInfo>>> {
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let mut sql = String::from(
        "SELECT id, agent_id, channel_id, direction, platform, external_user_id, external_conversation_id, message_type, CAST(content_json AS TEXT) AS content_json, status, COALESCE(queue_status, 'none') AS queue_status, COALESCE(attempt_count, 0) AS attempt_count, COALESCE(max_attempts, 3) AS max_attempts, claimed_by, claimed_at, last_error, finished_at, error_message, created_at FROM bot_message_logs WHERE tenant_id = ?",
    );
    if params.agent_id.is_some() {
        sql.push_str(" AND agent_id = ?");
    }
    if params.channel_id.is_some() {
        sql.push_str(" AND channel_id = ?");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
    let mut query = sqlx::query::<sqlx::Sqlite>(sqlx::AssertSqlSafe(sql)).bind(&claims.tenant_id);
    if let Some(agent_id) = &params.agent_id {
        query = query.bind(agent_id);
    }
    if let Some(channel_id) = &params.channel_id {
        query = query.bind(channel_id);
    }
    let rows = query
        .bind(pagination.limit())
        .bind(pagination.offset())
        .fetch_all(state.control_db())
        .await?;
    Ok(Json(ListResponse {
        items: rows.into_iter().map(row_to_log).collect(),
        total: -1,
    }))
}

async fn inbound_webhook(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    Query(query): Query<WebhookQuery>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT c.tenant_id, c.enabled, c.platform, c.inbound_mode, c.inbound_secret,
               a.enabled AS agent_enabled
        FROM bot_agent_channels c
        INNER JOIN bot_agents a ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
        WHERE c.id = ?
        ",
    )
    .bind(&channel_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{channel_id}' not found")))?;

    let channel_enabled: bool = row.get("enabled");
    let agent_enabled: bool = row.get("agent_enabled");
    let platform: String = row.get("platform");
    let inbound_mode: String = row.get("inbound_mode");
    let tenant_id: String = row.get("tenant_id");
    let inbound_secret = decrypt_bot_channel_secret(
        row.get("inbound_secret"),
        &tenant_id,
        &channel_id,
        BotChannelSecretKind::InboundSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret decrypt failed: {error}")))?;

    if !channel_enabled || !agent_enabled {
        return Err(AppError::Forbidden);
    }
    if !platform_accepts_webhook_ingress(&platform, &inbound_mode) {
        return Err(AppError::ValidationError(format!(
            "channel uses inbound_mode '{inbound_mode}'; Webhook ingress is disabled"
        )));
    }
    if let Some(secret) = inbound_secret.filter(|s| !s.is_empty()) {
        if query.secret.as_deref() != Some(secret.as_str()) {
            return Err(AppError::Forbidden);
        }
    }

    if let Some(challenge) = webhook_challenge_response(&payload) {
        return Ok(Json(challenge));
    }

    let result = enqueue_inbound_payload(state, channel_id, payload).await?;
    Ok(Json(result))
}

async fn inbound_webhook_verify(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    Query(query): Query<WebhookQuery>,
) -> Result<String> {
    let row = sqlx::query::<sqlx::Sqlite>(
        r"
        SELECT c.tenant_id, c.enabled, c.platform, c.inbound_mode, c.inbound_secret,
               a.enabled AS agent_enabled
        FROM bot_agent_channels c
        INNER JOIN bot_agents a ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
        WHERE c.id = ?
        ",
    )
    .bind(&channel_id)
    .fetch_optional(state.control_db())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("bot channel '{channel_id}' not found")))?;

    let channel_enabled: bool = row.get("enabled");
    let agent_enabled: bool = row.get("agent_enabled");
    let platform: String = row.get("platform");
    let inbound_mode: String = row.get("inbound_mode");
    let tenant_id: String = row.get("tenant_id");
    let inbound_secret = decrypt_bot_channel_secret(
        row.get("inbound_secret"),
        &tenant_id,
        &channel_id,
        BotChannelSecretKind::InboundSecret,
    )
    .map_err(|error| AppError::Internal(format!("Bot channel secret decrypt failed: {error}")))?;
    if !channel_enabled || !agent_enabled {
        return Err(AppError::Forbidden);
    }
    if !platform_accepts_webhook_ingress(&platform, &inbound_mode) {
        return Err(AppError::ValidationError(format!(
            "channel uses inbound_mode '{inbound_mode}'; Webhook verification is disabled"
        )));
    }
    if let Some(secret) = inbound_secret.filter(|s| !s.is_empty()) {
        let provided = query
            .secret
            .as_deref()
            .or(query.hub_verify_token.as_deref());
        if provided != Some(secret.as_str()) {
            return Err(AppError::Forbidden);
        }
    }
    if let Some(mode) = query.hub_mode.as_deref() {
        if mode != "subscribe" {
            return Err(AppError::ValidationError(
                "unsupported webhook verify mode".to_string(),
            ));
        }
    }
    let challenge = query
        .hub_challenge
        .or(query.challenge)
        .ok_or_else(|| AppError::ValidationError("webhook challenge is required".to_string()))?;
    Ok(challenge)
}

pub fn routes(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/", routing_get(list_agents).post(create_agent))
        .route(
            "/{id}",
            routing_get(get_agent)
                .patch(update_agent)
                .delete(delete_agent),
        )
        .route("/channels", routing_get(list_channels).post(create_channel))
        .route(
            "/channels/{id}",
            routing_get(get_channel)
                .patch(update_channel)
                .delete(delete_channel),
        )
        .route("/channels/{id}/test", routing_post(test_channel))
        .route("/message-logs", routing_get(list_logs))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth_middleware::require_auth,
        ));

    Router::new().merge(protected).route(
        "/webhooks/{channel_id}",
        routing_get(inbound_webhook_verify).post(inbound_webhook),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::Row;

    use super::{
        bot_message_idempotency_key, build_bot_prompt, external_conversation_id_from_payload,
        external_message_id_from_payload, external_user_id_from_payload,
        normalize_platform_inbound_payload, pairing_allowed_in_private_conversation,
        payload_mentions_bot, payload_string_field, payload_text, platform_accepts_webhook_ingress,
        select_capability_for_payload, test_send_error_requires_identity_binding,
        validate_platform_inbound_mode, webhook_challenge_response, BotAgentCapabilityInfo,
        BotConversationContext, BotConversationMessage, BotExecutionMode, BotExecutionPolicy,
        InboundExecutionOutcome,
    };

    #[test]
    fn test_send_identity_error_is_only_rewritten_before_binding() {
        let recipient_error =
            "Feishu default recipient/chat_id is required when no webhook is configured";
        assert!(test_send_error_requires_identity_binding(
            false,
            recipient_error
        ));
        assert!(!test_send_error_requires_identity_binding(
            true,
            recipient_error
        ));
        assert!(!test_send_error_requires_identity_binding(
            false,
            "Feishu API returned 403 forbidden"
        ));
    }

    #[test]
    fn bot_attachment_filenames_are_reduced_to_safe_basenames() {
        assert_eq!(
            super::sanitized_bot_attachment_filename(
                Some("../private/quarterly\0report.xlsx"),
                "fallback.txt"
            ),
            "quarterlyreport.xlsx"
        );
        assert_eq!(
            super::sanitized_bot_attachment_filename(
                Some("..\\private\\report.csv"),
                "fallback.txt"
            ),
            "report.csv"
        );
        assert_eq!(
            super::sanitized_bot_attachment_filename(Some("\n\t"), "fallback.txt"),
            "fallback.txt"
        );
    }

    #[test]
    fn bot_attachment_extraction_covers_supported_platform_shapes() {
        let cases = [
            (
                "slack",
                json!({
                    "event": {
                        "client_msg_id": "slack-message-1",
                        "files": [{
                            "id": "slack-file-1",
                            "name": "report.xlsx",
                            "mimetype": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                            "url_private_download": "https://files.slack.com/files-pri/report.xlsx"
                        }]
                    }
                }),
                "slack-file-1",
                "report.xlsx",
                "document",
            ),
            (
                "discord",
                json!({
                    "id": "discord-message-1",
                    "attachments": [{
                        "id": "discord-file-1",
                        "filename": "screen.png",
                        "content_type": "image/png",
                        "url": "https://cdn.discordapp.com/attachments/screen.png"
                    }]
                }),
                "discord-file-1",
                "screen.png",
                "image",
            ),
            (
                "telegram",
                json!({
                    "message": {
                        "message_id": 17,
                        "document": {
                            "file_id": "telegram-file-1",
                            "file_name": "brief.pdf",
                            "mime_type": "application/pdf"
                        }
                    }
                }),
                "telegram-file-1",
                "brief.pdf",
                "document",
            ),
            (
                "whatsapp",
                json!({
                    "entry": [{ "changes": [{ "value": {
                        "messages": [{
                            "id": "wamid.1",
                            "document": {
                                "id": "whatsapp-file-1",
                                "filename": "metrics.csv",
                                "mime_type": "text/csv"
                            }
                        }]
                    } }] }]
                }),
                "whatsapp-file-1",
                "metrics.csv",
                "document",
            ),
            (
                "feishu",
                json!({
                    "event": { "message": {
                        "message_id": "feishu-message-1",
                        "message_type": "file",
                        "content": "{\"file_key\":\"feishu-file-1\",\"file_name\":\"scope.docx\"}"
                    } }
                }),
                "feishu-file-1",
                "scope.docx",
                "document",
            ),
            (
                "wecom",
                json!({
                    "MsgId": "wecom-message-1",
                    "MediaId": "wecom-file-1",
                    "FileName": "evidence.txt"
                }),
                "wecom-file-1",
                "evidence.txt",
                "document",
            ),
        ];

        for (platform, payload, file_id, filename, kind) in cases {
            let attachments = super::bot_inbound_attachments(platform, &payload);
            assert_eq!(attachments.len(), 1, "platform={platform}");
            assert_eq!(
                attachments[0].provider_file_id.as_deref(),
                Some(file_id),
                "platform={platform}"
            );
            assert_eq!(attachments[0].filename, filename, "platform={platform}");
            assert_eq!(attachments[0].kind, kind, "platform={platform}");
        }
    }

    #[test]
    fn attachment_only_payloads_receive_a_user_facing_fallback_prompt() {
        for (platform, payload) in [
            (
                "slack",
                json!({ "event": {
                    "type": "message",
                    "user": "U1",
                    "channel": "D1",
                    "files": [{
                        "id": "F1",
                        "name": "report.csv",
                        "url_private_download": "https://files.slack.com/report.csv"
                    }]
                } }),
            ),
            (
                "discord",
                json!({
                    "id": "M1",
                    "channel_id": "D1",
                    "author": { "id": "U1" },
                    "attachments": [{
                        "id": "F1",
                        "filename": "screen.png",
                        "content_type": "image/png",
                        "url": "https://cdn.discordapp.com/screen.png"
                    }]
                }),
            ),
        ] {
            let normalized = super::normalize_platform_inbound_payload(platform, payload);
            assert!(
                !super::payload_text(&normalized).is_empty(),
                "platform={platform}"
            );
            assert_eq!(
                normalized
                    .get("attachments")
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::len),
                Some(1),
                "platform={platform}"
            );
        }
    }

    #[test]
    fn attachment_host_and_ip_guards_reject_boundary_bypasses() {
        let slack = super::BotAttachmentAccess {
            platform: "slack".to_string(),
            outbound_token: None,
            config: None,
        };
        assert!(super::bot_attachment_host_allowed(
            &slack,
            "files.slack.com"
        ));
        assert!(!super::bot_attachment_host_allowed(
            &slack,
            "slack.com.attacker.example"
        ));
        assert!(!super::bot_attachment_host_allowed(&slack, "evilslack.com"));

        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !super::attachment_ip_is_public(address.parse().expect("valid test IP")),
                "address={address}"
            );
        }
        assert!(super::attachment_ip_is_public(
            "1.1.1.1".parse().expect("valid public IP")
        ));
    }

    #[test]
    fn staged_attachment_metadata_round_trips_without_shape_drift() {
        let staged = vec![super::StagedBotAttachment {
            file_id: "file-1".to_string(),
            filename: "report.pdf".to_string(),
            media_type: "application/pdf".to_string(),
            size_bytes: 42,
            url: "/api/v1/uploads/user-1/file-1.pdf".to_string(),
            kind: "document".to_string(),
        }];
        let content = json!({ "stagedAttachments": &staged });
        assert_eq!(super::staged_attachments_from_content(&content), staged);
    }

    #[test]
    fn supplemental_input_accepts_text_without_serializing_control_json() {
        assert_eq!(
            super::supplemental_input_text(&json!({"text": "  使用华东区口径  "})).as_deref(),
            Some("使用华东区口径")
        );
        assert_eq!(
            super::supplemental_input_text(&json!("继续执行")).as_deref(),
            Some("继续执行")
        );
        assert!(super::supplemental_input_text(&json!({"approved": true})).is_none());
        assert!(super::supplemental_input_text(&json!({"text": "  "})).is_none());
    }

    #[test]
    fn supplemental_input_resume_ids_are_stable_and_fit_the_database_column() {
        let first = super::bot_resume_log_id("tenant-a", "task-a", "command-a");
        assert_eq!(
            first,
            super::bot_resume_log_id("tenant-a", "task-a", "command-a")
        );
        assert_ne!(
            first,
            super::bot_resume_log_id("tenant-a", "task-a", "command-b")
        );
        assert!(first.len() <= 36);
    }

    #[tokio::test]
    async fn sqlite_supplemental_input_reuses_the_original_bot_task() {
        let db = crate::test_sqlite_pool().await;
        let state = crate::state::AppState {
            data_dir: std::env::temp_dir(),
            platform_lifecycle: None,
            control_db: db.clone(),
            telemetry_db: db.clone(),
            #[cfg(feature = "pm")]
            pm_telemetry: crate::routes::agent::PmTelemetrySink::for_test(),
            db,
            jwt_secret: std::sync::Arc::new(tokio::sync::RwLock::new("test".repeat(8))),
            base_url: "http://localhost".to_string(),
            default_model: "test-model".to_string(),
            setup_initialized_cache: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            usage_writer: None,
            agent_manager: None,
            #[cfg(feature = "projects")]
            gitlab_manager: None,
            config_registry: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_embedding_store: None,
            #[cfg(feature = "rd")]
            rd_embedding_store: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_routing_engine: None,
            #[cfg(feature = "nl2sql")]
            nl2sql_pool_cache: std::sync::Arc::new(crate::nl2sql::datasource_pool::PoolCache::new()),
            #[cfg(feature = "nl2sql")]
            nl2sql_rate_limiter: std::sync::Arc::new(
                crate::nl2sql::rate_limiter::TenantRateLimiter::default(),
            ),
        };
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let source_log_id = uuid::Uuid::new_v4().to_string();
        let agent_id = uuid::Uuid::new_v4().to_string();
        let channel_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = format!("conversation-{}", uuid::Uuid::new_v4());
        let command_id = format!("agtcmd-{}", uuid::Uuid::new_v4());

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO bot_message_logs
               (id, tenant_id, agent_id, channel_id, direction, platform,
                external_user_id, external_conversation_id, message_type, content_json,
                status, queue_status)
             VALUES (?, ?, ?, ?, 'inbound', 'slack', 'external-user', ?, 'text', ?,
                     'completed', 'succeeded')",
        )
        .bind(&source_log_id)
        .bind(&tenant_id)
        .bind(&agent_id)
        .bind(&channel_id)
        .bind(&conversation_id)
        .bind(json!({ "text": "original question" }))
        .execute(state.control_db())
        .await
        .expect("insert original Bot message");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_tasks
               (id, short_code, tenant_id, source, source_ref, capability_key, title,
                status, phase, state_version, owner_user_id, initiator_user_id,
                root_task_id, external_platform, external_channel_id,
                external_conversation_id, last_heartbeat_at)
             VALUES (?, ?, ?, 'bot', ?, 'generic_ai', 'Bot waiting task',
                     'waiting_input', 'executing', 1, ?, ?, ?, 'slack', ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&task_id)
        .bind(crate::routes::task_control::short_code_for_task_id(
            &task_id,
        ))
        .bind(&tenant_id)
        .bind(&source_log_id)
        .bind(&user_id)
        .bind(&user_id)
        .bind(&task_id)
        .bind(&channel_id)
        .bind(&conversation_id)
        .execute(state.control_db())
        .await
        .expect("insert waiting Bot task");

        let first = super::resume_waiting_bot_task_from_agent_ops(
            &state,
            &tenant_id,
            &task_id,
            &command_id,
            &json!({ "text": "use the east-region definition" }),
        )
        .await
        .expect("queue supplemental input");
        let resume_log_id = first
            .get("queueLogId")
            .and_then(serde_json::Value::as_str)
            .expect("resume queue log id")
            .to_string();
        assert!(resume_log_id.len() <= 36);
        let queued = sqlx::query::<sqlx::Sqlite>(
            "SELECT CAST(content_json AS TEXT) AS content_json, queue_status
             FROM bot_message_logs WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&resume_log_id)
        .fetch_one(state.control_db())
        .await
        .expect("load resume queue row");
        let queued_content: serde_json::Value =
            serde_json::from_str(&queued.get::<String, _>("content_json"))
                .expect("valid resume payload");
        assert_eq!(queued.get::<String, _>("queue_status"), "queued");
        assert_eq!(
            queued_content
                .get("agentTaskId")
                .and_then(serde_json::Value::as_str),
            Some(task_id.as_str())
        );
        assert_eq!(
            queued_content
                .get("aos_actor_user_id")
                .and_then(serde_json::Value::as_str),
            Some(user_id.as_str())
        );
        let task_status: String = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT status FROM agent_tasks WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .fetch_one(state.control_db())
        .await
        .expect("load resumed task");
        assert_eq!(task_status, "running");

        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks SET status = 'waiting_input' WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&task_id)
        .execute(state.control_db())
        .await
        .expect("restore waiting state for idempotency replay");
        let replay = super::resume_waiting_bot_task_from_agent_ops(
            &state,
            &tenant_id,
            &task_id,
            &command_id,
            &json!({ "text": "use the east-region definition" }),
        )
        .await
        .expect("replay supplemental input");
        assert_eq!(
            replay.get("reused").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let resume_count: i64 = sqlx::query_scalar::<sqlx::Sqlite, _>(
            "SELECT CAST(COUNT(*) AS INTEGER) FROM bot_message_logs
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&resume_log_id)
        .fetch_one(state.control_db())
        .await
        .expect("count resume rows");
        assert_eq!(resume_count, 1);

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&task_id)
            .execute(state.control_db())
            .await
            .expect("delete Bot task fixture");
        sqlx::query::<sqlx::Sqlite>(
            "DELETE FROM bot_message_logs WHERE tenant_id = ? AND id IN (?, ?)",
        )
        .bind(&tenant_id)
        .bind(&source_log_id)
        .bind(&resume_log_id)
        .execute(state.control_db())
        .await
        .expect("delete Bot log fixtures");
        state.db.close().await;
    }

    #[tokio::test]
    async fn sqlite_bot_session_reuse_requires_the_same_bound_conversation() {
        let db = crate::test_sqlite_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        let session_id = uuid::Uuid::new_v4().to_string();
        let previous_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let current_task_id = format!("agt-{}", uuid::Uuid::new_v4());
        let channel_id = uuid::Uuid::new_v4().to_string();

        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO tenants (id, name, slug) VALUES (?, 'Bot session tenant', ?)",
        )
        .bind(&tenant_id)
        .bind(format!("bot-session-{}", uuid::Uuid::new_v4()))
        .execute(&db)
        .await
        .expect("insert Bot session tenant fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO users
               (id, email, name, password_hash, role, tenant_id, is_active)
             VALUES (?, ?, 'Bot session user', 'not-used', 'developer', ?, 1)",
        )
        .bind(&user_id)
        .bind(format!(
            "bot-session-{}@example.invalid",
            uuid::Uuid::new_v4()
        ))
        .bind(&tenant_id)
        .execute(&db)
        .await
        .expect("insert Bot session user fixture");
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO agent_sessions
               (id, tenant_id, user_id, session_id, name, model, workspace_path, state,
                source, provider)
             VALUES (?, ?, ?, ?, 'Bot shared session', 'test-model', ?, 'idle',
                     'super_assistant', 'test')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&tenant_id)
        .bind(&user_id)
        .bind(&session_id)
        .bind(
            std::env::temp_dir()
                .join(&session_id)
                .to_string_lossy()
                .to_string(),
        )
        .execute(&db)
        .await
        .expect("insert shared session fixture");
        for (task_id, origin_session_id) in [
            (&previous_task_id, Some(session_id.as_str())),
            (&current_task_id, None),
        ] {
            sqlx::query::<sqlx::Sqlite>(
                "INSERT INTO agent_tasks
                   (id, short_code, tenant_id, source, capability_key, title, owner_user_id,
                    initiator_user_id, root_task_id, external_platform, external_channel_id,
                    external_conversation_id, origin_session_id, last_heartbeat_at)
                 VALUES (?, ?, ?, 'bot', 'ai_chat', 'Bot shared conversation', ?, ?, ?,
                         'slack', ?, 'conversation-a', ?, CURRENT_TIMESTAMP)",
            )
            .bind(task_id)
            .bind(crate::routes::task_control::short_code_for_task_id(task_id))
            .bind(&tenant_id)
            .bind(&user_id)
            .bind(&user_id)
            .bind(task_id)
            .bind(&channel_id)
            .bind(origin_session_id)
            .execute(&db)
            .await
            .expect("insert Bot conversation task fixture");
        }

        assert_eq!(
            super::reusable_bot_super_assistant_session_id(
                &db,
                &tenant_id,
                &user_id,
                &current_task_id,
            )
            .await
            .expect("resolve reusable session"),
            Some(session_id.clone())
        );
        sqlx::query::<sqlx::Sqlite>(
            "UPDATE agent_tasks SET external_conversation_id = 'conversation-b'
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&tenant_id)
        .bind(&current_task_id)
        .execute(&db)
        .await
        .expect("change current conversation");
        assert!(super::reusable_bot_super_assistant_session_id(
            &db,
            &tenant_id,
            &user_id,
            &current_task_id,
        )
        .await
        .expect("reject cross-conversation reuse")
        .is_none());

        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_tasks WHERE tenant_id = ?")
            .bind(&tenant_id)
            .execute(&db)
            .await
            .expect("delete Bot session task fixtures");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM agent_sessions WHERE session_id = ?")
            .bind(&session_id)
            .execute(&db)
            .await
            .expect("delete shared session fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM users WHERE tenant_id = ? AND id = ?")
            .bind(&tenant_id)
            .bind(&user_id)
            .execute(&db)
            .await
            .expect("delete Bot session user fixture");
        sqlx::query::<sqlx::Sqlite>("DELETE FROM tenants WHERE id = ?")
            .bind(&tenant_id)
            .execute(&db)
            .await
            .expect("delete Bot session tenant fixture");
        db.close().await;
    }

    #[test]
    fn bot_session_scope_locks_serialize_the_same_bound_conversation() {
        let scope = format!("scope-{}", uuid::Uuid::new_v4());
        let first = super::bot_session_scope_lock(&scope);
        let second = super::bot_session_scope_lock(&scope);
        let other = super::bot_session_scope_lock(&format!("{scope}-other"));

        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn identity_pairing_requires_a_confirmed_private_conversation() {
        assert!(pairing_allowed_in_private_conversation(
            "dingtalk",
            &json!({ "conversationType": "1" }),
        ));
        assert!(!pairing_allowed_in_private_conversation(
            "dingtalk",
            &json!({ "conversationType": "2" }),
        ));
        assert!(pairing_allowed_in_private_conversation(
            "slack",
            &json!({ "event": { "channel": "D12345", "channel_type": "im" } }),
        ));
        assert!(!pairing_allowed_in_private_conversation(
            "slack",
            &json!({ "event": { "channel": "C12345", "channel_type": "channel" } }),
        ));
        assert!(pairing_allowed_in_private_conversation(
            "telegram",
            &json!({ "message": { "chat": { "type": "private" } } }),
        ));
        assert!(!pairing_allowed_in_private_conversation(
            "generic_webhook",
            &json!({ "text": "bind ABCD1234" }),
        ));
    }

    #[test]
    fn provider_message_ids_survive_payload_normalization() {
        let feishu = normalize_platform_inbound_payload(
            "feishu",
            json!({
                "event_id": "evt-feishu-1",
                "event": {
                    "sender": { "sender_id": { "open_id": "ou-1" } },
                    "message": {
                        "message_id": "om-feishu-1",
                        "chat_type": "p2p",
                        "chat_id": "oc-1",
                        "content": "{\"text\":\"hello\"}"
                    }
                }
            }),
        );
        assert_eq!(
            external_message_id_from_payload(&feishu).as_deref(),
            Some("om-feishu-1")
        );

        let whatsapp = normalize_platform_inbound_payload(
            "whatsapp",
            json!({
                "entry": [{ "changes": [{ "value": {
                    "messages": [{ "id": "wamid.1", "from": "15551234567", "text": { "body": "hello" } }]
                } }] }]
            }),
        );
        assert_eq!(
            external_message_id_from_payload(&whatsapp).as_deref(),
            Some("wamid.1")
        );

        let long_id = "external-message/".repeat(100);
        let normalized = external_message_id_from_payload(&json!({ "message_id": long_id }))
            .expect("hashed external id");
        assert!(normalized.starts_with("sha256:"));
        assert!(normalized.len() <= 255);

        let key = bot_message_idempotency_key(
            "task",
            &"platform/".repeat(20),
            &"channel/".repeat(30),
            &normalized,
        );
        assert_eq!(
            key,
            bot_message_idempotency_key(
                "task",
                &"platform/".repeat(20),
                &"channel/".repeat(30),
                &normalized,
            )
        );
        assert!(key.len() <= 128);

        let telegram = json!({
            "message": {
                "message_id": 42,
                "from": { "id": 10001, "username": "tester" },
                "chat": { "id": 20002, "type": "private" },
                "text": "bind ABCD1234"
            }
        });
        assert_eq!(
            external_message_id_from_payload(&telegram).as_deref(),
            Some("42")
        );
        assert_eq!(
            external_user_id_from_payload(&telegram).as_deref(),
            Some("10001")
        );
        assert_eq!(
            external_conversation_id_from_payload(&telegram).as_deref(),
            Some("20002")
        );
    }

    #[test]
    fn feishu_event_payload_is_normalized() {
        let payload = json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_user_1",
                        "user_id": "user_1"
                    }
                },
                "message": {
                    "chat_id": "oc_chat_1",
                    "message_type": "text",
                    "content": "{\"text\":\"cy 看一下昨天留存\"}"
                }
            }
        });
        let normalized = normalize_platform_inbound_payload("feishu", payload);
        assert_eq!(payload_text(&normalized), "cy 看一下昨天留存");
        assert_eq!(
            payload_string_field(&normalized, &["user_id"]),
            Some("ou_user_1".to_string())
        );
        assert_eq!(
            payload_string_field(&normalized, &["conversation_id"]),
            Some("oc_chat_1".to_string())
        );
        assert_eq!(
            normalized.get("platform_adapter").and_then(|v| v.as_str()),
            Some("feishu_event")
        );
    }

    #[test]
    fn feishu_private_chat_counts_as_direct_bot_mention() {
        let payload = json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user_1" } },
                "message": {
                    "chat_id": "oc_private_1",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"帮我看一下\"}"
                }
            }
        });
        let normalized = normalize_platform_inbound_payload("feishu", payload);
        assert!(payload_mentions_bot(&normalized, "aos"));
    }

    #[test]
    fn feishu_private_chat_without_chat_type_still_counts_as_direct_mention() {
        let payload = json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user_1" } },
                "message": {
                    "chat_id": "oc_private_1",
                    "message_type": "text",
                    "content": "{\"text\":\"帮我看一下\"}"
                }
            }
        });
        let normalized = normalize_platform_inbound_payload("feishu", payload);
        assert!(payload_mentions_bot(&normalized, "aos"));
    }

    #[test]
    fn feishu_stream_private_payload_counts_as_direct_mention() {
        let payload = json!({
            "text": "帮我看一下",
            "message": "帮我看一下",
            "content": "帮我看一下",
            "conversation_id": "oc_private_1",
            "chat_id": "oc_private_1",
            "chat_type": "p2p",
            "platform_adapter": "feishu_stream"
        });
        let normalized = normalize_platform_inbound_payload("feishu", payload);
        assert!(payload_mentions_bot(&normalized, "aos"));
    }

    #[test]
    fn feishu_group_chat_without_mentions_does_not_count_as_mention() {
        let payload = json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user_1" } },
                "message": {
                    "chat_id": "oc_group_1",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"帮我看一下\"}"
                }
            }
        });
        let normalized = normalize_platform_inbound_payload("feishu", payload);
        assert!(!payload_mentions_bot(&normalized, "aos"));
    }

    #[test]
    fn feishu_private_chat_hits_default_capability_when_mention_required() {
        let capabilities = vec![BotAgentCapabilityInfo {
            id: "cap-1".to_string(),
            agent_id: "agent-1".to_string(),
            capability_key: "pm_assistant".to_string(),
            enabled: true,
            config_json: Some(json!({
                "require_mention": true,
                "fallback_when_no_prefix": false
            })),
        }];
        let payload = normalize_platform_inbound_payload(
            "feishu",
            json!({
                "schema": "2.0",
                "header": { "event_type": "im.message.receive_v1" },
                "event": {
                    "sender": { "sender_id": { "open_id": "ou_user_1" } },
                    "message": {
                        "chat_id": "oc_private_1",
                        "message_type": "text",
                        "content": "{\"text\":\"帮我看一下\"}"
                    }
                }
            }),
        );
        assert_eq!(
            select_capability_for_payload(&capabilities, &payload, "aos", "pm_assistant"),
            Some("pm_assistant".to_string())
        );
    }

    #[test]
    fn slack_event_payload_is_normalized_and_bot_echo_ignored() {
        let payload = json!({
            "type": "event_callback",
            "event": {
                "type": "app_mention",
                "user": "U123",
                "channel": "C456",
                "text": "<@BOT> cy ROI 怎么看"
            }
        });
        let normalized = normalize_platform_inbound_payload("slack", payload);
        assert_eq!(payload_text(&normalized), "<@BOT> cy ROI 怎么看");
        assert_eq!(
            payload_string_field(&normalized, &["conversation_id"]),
            Some("C456".to_string())
        );
        assert_eq!(
            normalized.get("mentioned").and_then(|v| v.as_bool()),
            Some(true)
        );

        let bot_payload = json!({
            "event": {
                "type": "message",
                "bot_id": "B999",
                "channel": "C456",
                "text": "bot echo"
            }
        });
        let bot_normalized = normalize_platform_inbound_payload("slack", bot_payload);
        assert_eq!(payload_text(&bot_normalized), "");
    }

    #[test]
    fn whatsapp_cloud_payload_is_normalized() {
        let payload = json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "changes": [{
                    "value": {
                        "metadata": { "phone_number_id": "12345" },
                        "contacts": [{
                            "wa_id": "628123456789",
                            "profile": { "name": "Nadia" }
                        }],
                        "messages": [{
                            "from": "628123456789",
                            "type": "text",
                            "text": { "body": "帮我生成一张海报" }
                        }]
                    }
                }]
            }]
        });
        let normalized = normalize_platform_inbound_payload("whatsapp", payload);
        assert_eq!(payload_text(&normalized), "帮我生成一张海报");
        assert_eq!(
            payload_string_field(&normalized, &["conversation_id"]),
            Some("628123456789".to_string())
        );
        assert_eq!(
            normalized.get("sender_name").and_then(|v| v.as_str()),
            Some("Nadia")
        );
    }

    #[test]
    fn webhook_challenge_keeps_feishu_and_slack_verification_working() {
        let feishu = json!({
            "type": "url_verification",
            "challenge": "feishu-challenge"
        });
        assert_eq!(
            webhook_challenge_response(&feishu),
            Some(json!({ "challenge": "feishu-challenge" }))
        );

        let slack = json!({
            "type": "url_verification",
            "challenge": "slack-challenge"
        });
        assert_eq!(
            webhook_challenge_response(&slack),
            Some(json!({ "challenge": "slack-challenge" }))
        );
    }

    #[test]
    fn configured_prefix_is_required_even_for_the_default_capability() {
        let payload = json!({ "text": "随便问一句" });
        let capabilities = vec![BotAgentCapabilityInfo {
            id: "cap-1".to_string(),
            agent_id: "agent-1".to_string(),
            capability_key: "pm_assistant".to_string(),
            enabled: true,
            config_json: Some(json!({
                "trigger_prefixes": ["pm"],
                "require_mention": false,
                "fallback_when_no_prefix": true
            })),
        }];

        assert_eq!(
            select_capability_for_payload(&capabilities, &payload, "aos", "generic_ai"),
            None
        );
        assert_eq!(
            select_capability_for_payload(&capabilities, &payload, "aos", "pm_assistant"),
            None
        );

        let private_mention = json!({
            "text": "随便问一句",
            "chat_type": "p2p",
            "mentioned": true
        });
        assert_eq!(
            select_capability_for_payload(&capabilities, &private_mention, "aos", "pm_assistant"),
            None
        );

        let prefixed = json!({ "text": "pm 看一下留存" });
        assert_eq!(
            select_capability_for_payload(&capabilities, &prefixed, "aos", "generic_ai"),
            Some("pm_assistant".to_string())
        );
    }

    #[test]
    fn default_capability_respects_require_mention() {
        let payload = json!({ "text": "没有艾特" });
        let capabilities = vec![BotAgentCapabilityInfo {
            id: "cap-1".to_string(),
            agent_id: "agent-1".to_string(),
            capability_key: "rd_agent".to_string(),
            enabled: true,
            config_json: Some(json!({
                "require_mention": true,
                "fallback_when_no_prefix": true
            })),
        }];

        assert_eq!(
            select_capability_for_payload(&capabilities, &payload, "aos", "rd_agent"),
            None
        );

        let mentioned = json!({ "text": "@aos 没有艾特", "mentioned": true });
        assert_eq!(
            select_capability_for_payload(&capabilities, &mentioned, "aos", "rd_agent"),
            Some("rd_agent".to_string())
        );
    }

    #[test]
    fn task_control_command_still_obeys_the_normal_capability_gate() {
        let payload = json!({ "text": "取消 1", "chat_type": "p2p" });
        let capabilities = vec![
            BotAgentCapabilityInfo {
                id: "cap-pm".to_string(),
                agent_id: "agent-1".to_string(),
                capability_key: "pm_assistant".to_string(),
                enabled: true,
                config_json: Some(json!({
                    "require_mention": false,
                    "fallback_when_no_prefix": true
                })),
            },
            BotAgentCapabilityInfo {
                id: "cap-watchdog".to_string(),
                agent_id: "agent-1".to_string(),
                capability_key: "watchdog".to_string(),
                enabled: true,
                config_json: Some(json!({
                    "require_mention": true,
                    "fallback_when_no_prefix": false,
                    "allowActions": ["open_task", "cancel_task", "retry_task"]
                })),
            },
        ];

        assert_eq!(
            select_capability_for_payload(&capabilities, &payload, "aos", "pm_assistant"),
            Some("pm_assistant".to_string())
        );
    }

    #[test]
    fn bot_prompt_includes_conversation_summary_and_recent_messages() {
        let context = BotConversationContext {
            summary: Some("用户在追问昨日留存，上一轮已说明需要看 cohort。".to_string()),
            recent_messages: vec![
                BotConversationMessage {
                    direction: "inbound".to_string(),
                    status: "replied".to_string(),
                    text: "昨天留存怎么看？".to_string(),
                    created_at: "2026-06-12T07:00:00+00:00".to_string(),
                },
                BotConversationMessage {
                    direction: "outbound".to_string(),
                    status: "sent".to_string(),
                    text: "建议先按渠道拆 cohort。".to_string(),
                    created_at: "2026-06-12T07:01:00+00:00".to_string(),
                },
            ],
            loaded_messages: 2,
            summarized_messages: 4,
            total_chars: 128,
        };
        let prompt = build_bot_prompt("pm_assistant", "aos", None, Some("Alex"), &context, "继续");
        assert!(prompt.contains("[压缩摘要]"));
        assert!(prompt.contains("[最近消息]"));
        assert!(prompt.contains("[user]"));
        assert!(prompt.contains("[assistant]"));
        assert!(prompt.contains("继续"));
    }

    #[cfg(feature = "nl2sql")]
    #[test]
    fn nl2sql_pending_state_requires_explicit_pending_status() {
        let ctx = crate::nl2sql::ClarificationContext {
            original_question: "查一下收入".to_string(),
            clarification_question: "按哪个时间范围？".to_string(),
            options: Vec::new(),
            confirmed_requirements: Vec::new(),
            missing_requirements: vec!["时间范围".to_string()],
            missing_requirement_reasons: Vec::new(),
            clarification_history: Vec::new(),
            turn: 1,
            conversation_id: "bot:oc_1".to_string(),
        };
        let pending = super::nl2sql_pending_state("bot:oc_1", "bot-nl2sql:bot:oc_1", &ctx);
        assert!(super::parse_nl2sql_pending_state(Some(pending)).is_some());

        let completed = json!({
            "capability": "nl2sql",
            "status": "completed",
            "conversationId": "bot:oc_1",
            "sessionId": "bot-nl2sql:bot:oc_1",
            "clarificationContext": ctx,
        });
        assert!(super::parse_nl2sql_pending_state(Some(completed)).is_none());
        assert!(super::parse_nl2sql_pending_state(Some(json!({
            "capability": "nl2sql",
            "conversationId": "bot:oc_1"
        })))
        .is_none());
    }

    #[test]
    fn bot_execution_policy_defaults_and_overrides_are_stable() {
        let rd = BotExecutionPolicy::for_capability("rd_agent", None);
        assert_eq!(rd.mode, BotExecutionMode::Async);
        assert!(rd.first_ack);
        assert!(rd.final_push);

        let nl2sql = BotExecutionPolicy::for_capability("nl2sql", None);
        assert_eq!(nl2sql.mode, BotExecutionMode::Clarification);
        assert!(!nl2sql.first_ack);

        let overridden = BotExecutionPolicy::for_capability(
            "ai_chat",
            Some(&json!({
                "executionMode": "sync",
                "syncTimeoutMs": 500,
                "ackTimeoutMs": 60000
            })),
        );
        assert_eq!(overridden.mode, BotExecutionMode::Sync);
        assert_eq!(overridden.sync_timeout_ms, 1000);
        assert_eq!(overridden.ack_timeout_ms, 30000);
        assert!(!overridden.first_ack);
    }

    #[test]
    fn bot_queue_execution_outcomes_map_to_terminal_queue_state() {
        let succeeded = InboundExecutionOutcome::Succeeded;
        assert!(succeeded.queue_success());
        assert_eq!(succeeded.error_message(), None);

        let ignored = InboundExecutionOutcome::Ignored {
            reason: "empty inbound message text".to_string(),
        };
        assert!(ignored.queue_success());
        assert_eq!(ignored.error_message(), Some("empty inbound message text"));

        let cancelled = InboundExecutionOutcome::Cancelled {
            reason: "cancelled before queue execution".to_string(),
        };
        assert!(!cancelled.queue_success());
        assert_eq!(
            cancelled.error_message(),
            Some("cancelled before queue execution")
        );
        assert_eq!(
            cancelled.agent_event().1,
            crate::routes::agent_ops::STATUS_CANCELLED
        );

        let failed = InboundExecutionOutcome::Failed {
            error_code: "capability_execution_failed".to_string(),
            message: "missing api key".to_string(),
        };
        assert!(!failed.queue_success());
        assert_eq!(failed.error_message(), Some("missing api key"));
    }

    #[test]
    fn pm_assistant_short_answer_declares_llm_only_path() {
        let answer = super::format_pm_assistant_short_answer("这是一个短答。");
        assert!(answer.starts_with("产运助手 · 短答（LLM）"));
        assert!(answer.contains("未执行深度检索"));
        assert!(answer.contains("开启“深度分析”"));
        assert!(answer.ends_with("这是一个短答。"));
    }

    #[test]
    fn pm_research_progress_push_includes_stage_result_and_next_step() {
        let event = super::PmResearchProgressEvent {
            seq: 3,
            status: "completed".to_string(),
            stage: Some("planner".to_string()),
            message: Some("已完成问题理解和研究计划".to_string()),
            detail: Some(json!({ "summary": "拆成用户画像、渠道、提现信任三个方向" })),
            response: None,
            error_message: None,
        };
        let text = super::build_pm_research_progress_push("pm-task-1", &event);
        assert!(text.contains("产运深度分析进度 · 理解与规划 已完成"));
        assert!(text.contains("阶段输出：拆成用户画像、渠道、提现信任三个方向"));
        assert!(text.contains("准备下一阶段：检索与采集"));
    }

    #[test]
    fn inbound_mode_validation_is_platform_aware() {
        assert!(validate_platform_inbound_mode("dingtalk", "stream").is_ok());
        assert!(validate_platform_inbound_mode("dingtalk", "socket").is_err());
        assert!(validate_platform_inbound_mode("feishu", "stream").is_ok());
        assert!(validate_platform_inbound_mode("lark", "stream").is_ok());
        assert!(validate_platform_inbound_mode("wecom", "stream").is_ok());
        assert!(validate_platform_inbound_mode("slack", "socket").is_ok());
        assert!(validate_platform_inbound_mode("discord", "socket").is_ok());
        assert!(validate_platform_inbound_mode("telegram", "polling").is_ok());
        assert!(validate_platform_inbound_mode("telegram", "webhook").is_ok());
        assert!(validate_platform_inbound_mode("slack", "webhook").is_err());
        assert!(validate_platform_inbound_mode("slack", "auto").is_ok());
        assert!(validate_platform_inbound_mode("whatsapp", "webhook").is_ok());
        assert!(validate_platform_inbound_mode("generic_webhook", "webhook").is_ok());
        assert!(validate_platform_inbound_mode("whatsapp", "stream").is_err());
        assert!(validate_platform_inbound_mode("whatsapp", "polling").is_err());
        assert!(validate_platform_inbound_mode("unknown", "auto").is_err());
    }

    #[test]
    fn webhook_ingress_matches_the_configured_transport() {
        assert!(platform_accepts_webhook_ingress("whatsapp", "auto"));
        assert!(platform_accepts_webhook_ingress("generic_webhook", "auto"));
        assert!(platform_accepts_webhook_ingress("telegram", "webhook"));
        assert!(!platform_accepts_webhook_ingress("telegram", "auto"));
        assert!(!platform_accepts_webhook_ingress("feishu", "stream"));
        assert!(!platform_accepts_webhook_ingress("slack", "socket"));
    }
}
