//! Inbound Bot Gateway runtime.
//!
//! Webhooks remain supported for compatibility, but channel `inbound_mode=auto`
//! prefers provider-side stream/socket/polling adapters so local/private AOS
//! installs do not need a public callback URL.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use feishu_sdk::core::{
    Config as FeishuConfig, LogLevel as FeishuLogLevel, Logger, LoggerRef, FEISHU_BASE_URL,
    LARK_BASE_URL,
};
use feishu_sdk::event::{
    Event as FeishuEvent, EventDispatcher, EventDispatcherConfig, EventHandler, EventResp,
};
use feishu_sdk::ws::{StreamClient, StreamConfig};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::Row;
use tokio_tungstenite::tungstenite::Message;

use crate::error::{AppError, Result};
use crate::state::AppState;

#[derive(Debug)]
struct FeishuTracingLogger {
    level: FeishuLogLevel,
}

impl Logger for FeishuTracingLogger {
    fn log(&self, level: FeishuLogLevel, message: &str) {
        if !self.is_enabled(level) {
            return;
        }
        let message = redact_feishu_log_message(message);
        match level {
            FeishuLogLevel::Debug => tracing::debug!(target: "feishu_sdk", "{}", message),
            FeishuLogLevel::Info => tracing::info!(target: "feishu_sdk", "{}", message),
            FeishuLogLevel::Warn => tracing::warn!(target: "feishu_sdk", "{}", message),
            FeishuLogLevel::Error => tracing::error!(target: "feishu_sdk", "{}", message),
        }
    }

    fn is_enabled(&self, level: FeishuLogLevel) -> bool {
        level >= self.level
    }
}

fn feishu_log_level() -> FeishuLogLevel {
    match std::env::var("BOT_GATEWAY_FEISHU_LOG")
        .unwrap_or_else(|_| "warn".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "debug" => FeishuLogLevel::Debug,
        "warn" => FeishuLogLevel::Warn,
        "error" => FeishuLogLevel::Error,
        "info" => FeishuLogLevel::Info,
        _ => FeishuLogLevel::Warn,
    }
}

fn redact_feishu_log_message(message: &str) -> String {
    const SENSITIVE_KEYS: &[&str] = &[
        "access_key",
        "ticket",
        "token",
        "app_secret",
        "secret",
        "authorization",
    ];
    let mut redacted = message.to_string();
    for key in SENSITIVE_KEYS {
        let needle = format!("{key}=");
        let mut search_from = 0;
        loop {
            let lower = redacted.to_ascii_lowercase();
            let Some(relative_start) = lower[search_from..].find(&needle) else {
                break;
            };
            let value_start = search_from + relative_start + needle.len();
            const PLACEHOLDER: &str = "[REDACTED]";
            if redacted[value_start..].starts_with(PLACEHOLDER) {
                search_from = value_start + PLACEHOLDER.len();
                continue;
            }
            let value_end = redacted[value_start..]
                .find(|ch: char| ch == '&' || ch.is_whitespace() || matches!(ch, '\'' | '"' | ']'))
                .map_or(redacted.len(), |offset| value_start + offset);
            redacted.replace_range(value_start..value_end, PLACEHOLDER);
            search_from = value_start + PLACEHOLDER.len();
        }
    }
    runtime::protect_sensitive_text(&redacted, runtime::configured_data_protection_mode()).value
}

fn feishu_logger() -> LoggerRef {
    Arc::new(FeishuTracingLogger {
        level: feishu_log_level(),
    })
}

#[derive(Debug, Clone)]
struct InboundChannelRuntimeConfig {
    tenant_id: String,
    channel_id: String,
    platform: String,
    inbound_mode: String,
    inbound_cursor: Option<String>,
    outbound_token: Option<String>,
    signing_secret: Option<String>,
    config_json: Option<Value>,
}

#[derive(Debug)]
struct DingTalkOpenConnectionResponse {
    endpoint: String,
    ticket: String,
}

fn active_inbound_channels() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn inbound_retry_after() -> &'static Mutex<HashMap<String, Instant>> {
    static RETRY_AFTER: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    RETRY_AFTER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_retry_after<T>(fallback: T, f: impl FnOnce(&mut HashMap<String, Instant>) -> T) -> T {
    match inbound_retry_after().lock() {
        Ok(mut guard) => f(&mut guard),
        Err(error) => {
            tracing::error!(error = %error, "bot inbound retry map mutex poisoned");
            fallback
        }
    }
}

fn with_active_channels<T>(fallback: T, f: impl FnOnce(&mut HashSet<String>) -> T) -> T {
    match active_inbound_channels().lock() {
        Ok(mut guard) => f(&mut guard),
        Err(error) => {
            tracing::error!(error = %error, "bot inbound active channel mutex poisoned");
            fallback
        }
    }
}

fn stream_retry_cooldown() -> Duration {
    let secs = std::env::var("BOT_GATEWAY_STREAM_RETRY_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(5, 3_600);
    Duration::from_secs(secs)
}

fn should_skip_for_retry_cooldown(channel_id: &str) -> bool {
    let now = Instant::now();
    with_retry_after(true, |retry_after| {
        retry_after.retain(|_, next| *next > now);
        retry_after
            .get(channel_id)
            .is_some_and(|next_allowed| *next_allowed > now)
    })
}

fn clear_retry_cooldown(channel_id: &str) {
    with_retry_after((), |retry_after| {
        retry_after.remove(channel_id);
    });
}

fn mark_retry_cooldown(channel_id: &str) {
    with_retry_after((), |retry_after| {
        retry_after.insert(
            channel_id.to_string(),
            Instant::now() + stream_retry_cooldown(),
        );
    });
}

fn clean_opt(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn parse_json_opt(raw: Option<String>) -> Option<Value> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
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

fn json_u64(config: Option<&Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        config
            .and_then(|value| value.get(*key))
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
    })
}

fn resolve_inbound_mode(platform: &str, mode: &str) -> String {
    let mode = mode.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "webhook" | "stream" | "socket" | "polling") {
        return mode;
    }
    match platform.trim().to_ascii_lowercase().as_str() {
        "dingtalk" => "stream".to_string(),
        "feishu" | "lark" | "wecom" | "slack" | "discord" => "stream".to_string(),
        "telegram" => "polling".to_string(),
        _ => "webhook".to_string(),
    }
}

fn telegram_token(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.outbound_token.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["botToken", "bot_token", "telegramToken", "token"],
        )
    })
}

fn dingtalk_client_id(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    json_string(
        channel.config_json.as_ref(),
        &["clientId", "client_id", "appKey", "app_key"],
    )
}

fn dingtalk_client_secret(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.signing_secret.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["clientSecret", "client_secret", "appSecret", "app_secret"],
        )
    })
}

fn dingtalk_api_base(channel: &InboundChannelRuntimeConfig) -> String {
    json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://api.dingtalk.com".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn slack_app_token(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.signing_secret.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["appToken", "app_token", "socketToken", "socket_token"],
        )
    })
}

fn slack_api_base(channel: &InboundChannelRuntimeConfig) -> String {
    json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://slack.com/api".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn discord_bot_token(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.outbound_token.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &[
                "botToken",
                "bot_token",
                "discordToken",
                "discord_token",
                "token",
            ],
        )
    })
}

fn discord_gateway_identify_token(raw: &str) -> &str {
    raw.strip_prefix("Bot ")
        .or_else(|| raw.strip_prefix("Bearer "))
        .unwrap_or(raw)
        .trim()
}

fn discord_gateway_url(channel: &InboundChannelRuntimeConfig) -> String {
    json_string(
        channel.config_json.as_ref(),
        &["gatewayUrl", "gateway_url", "websocketUrl", "websocket_url"],
    )
    .unwrap_or_else(|| "wss://gateway.discord.gg/?v=10&encoding=json".to_string())
}

fn discord_intents(channel: &InboundChannelRuntimeConfig) -> u64 {
    json_u64(
        channel.config_json.as_ref(),
        &["intents", "gatewayIntents", "gateway_intents"],
    )
    .unwrap_or(1 | 512 | 4096 | 32768)
}

fn wecom_bot_id(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    json_string(channel.config_json.as_ref(), &["botId", "bot_id"])
}

fn wecom_bot_secret(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.signing_secret.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["botSecret", "bot_secret", "secret"],
        )
    })
}

fn wecom_ws_base(channel: &InboundChannelRuntimeConfig) -> String {
    json_string(
        channel.config_json.as_ref(),
        &["wsBase", "ws_base", "websocketBase"],
    )
    .unwrap_or_else(|| "wss://openws.work.weixin.qq.com".to_string())
    .trim_end_matches('/')
    .to_string()
}

fn feishu_app_id(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    json_string(
        channel.config_json.as_ref(),
        &["appId", "app_id", "appKey", "app_key"],
    )
}

fn feishu_app_secret(channel: &InboundChannelRuntimeConfig) -> Option<String> {
    clean_opt(channel.signing_secret.clone()).or_else(|| {
        json_string(
            channel.config_json.as_ref(),
            &["appSecret", "app_secret", "clientSecret", "client_secret"],
        )
    })
}

fn feishu_base_url(channel: &InboundChannelRuntimeConfig) -> String {
    json_string(
        channel.config_json.as_ref(),
        &["baseUrl", "base_url", "apiBase", "api_base"],
    )
    .unwrap_or_else(|| {
        if channel.platform.eq_ignore_ascii_case("lark") {
            LARK_BASE_URL.to_string()
        } else {
            FEISHU_BASE_URL.to_string()
        }
    })
    .trim_end_matches('/')
    .to_string()
}

fn append_query_param(url: &str, key: &str, value: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{key}={}", urlencoding::encode(value))
}

async fn load_enabled_channels(state: &AppState) -> Result<Vec<InboundChannelRuntimeConfig>> {
    let rows = sqlx::query(
        r#"
        SELECT c.tenant_id, c.id, c.platform,
               c.inbound_mode,
               c.inbound_cursor, c.outbound_token, c.signing_secret,
               CAST(c.config_json AS TEXT) AS config_json
        FROM bot_agent_channels c
        INNER JOIN bot_agents a ON a.tenant_id = c.tenant_id AND a.id = c.agent_id
        WHERE c.enabled = 1 AND a.enabled = 1
        ORDER BY c.updated_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(state.control_db())
    .await?;

    let mut channels = Vec::with_capacity(rows.len());
    for row in rows {
        let inbound_mode = clean_opt(row.get::<Option<String>, _>("inbound_mode"))
            .unwrap_or_else(|| "auto".to_string());
        let tenant_id: String = row.get("tenant_id");
        let channel_id: String = row.get("id");
        let outbound_token = crate::routes::bot_agents_types::decrypt_bot_channel_secret(
            row.get("outbound_token"),
            &tenant_id,
            &channel_id,
            crate::routes::bot_agents_types::BotChannelSecretKind::OutboundToken,
        )
        .map_err(|error| {
            AppError::Internal(format!("Bot channel secret decrypt failed: {error}"))
        })?;
        let signing_secret = crate::routes::bot_agents_types::decrypt_bot_channel_secret(
            row.get("signing_secret"),
            &tenant_id,
            &channel_id,
            crate::routes::bot_agents_types::BotChannelSecretKind::SigningSecret,
        )
        .map_err(|error| {
            AppError::Internal(format!("Bot channel secret decrypt failed: {error}"))
        })?;
        let channel = InboundChannelRuntimeConfig {
            tenant_id,
            channel_id,
            platform: row.get("platform"),
            inbound_mode,
            inbound_cursor: row.get("inbound_cursor"),
            outbound_token,
            signing_secret,
            config_json: parse_json_opt(row.get("config_json")),
        };
        if resolve_inbound_mode(&channel.platform, &channel.inbound_mode) != "webhook" {
            channels.push(channel);
        }
    }
    Ok(channels)
}

async fn update_channel_status(
    state: &AppState,
    channel: &InboundChannelRuntimeConfig,
    status: &str,
    error: Option<&str>,
) {
    if let Err(err) = sqlx::query(
        r#"
        UPDATE bot_agent_channels
        SET inbound_status = ?, inbound_error = ?, inbound_last_seen_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        "#,
    )
    .bind(status)
    .bind(error)
    .bind(&channel.tenant_id)
    .bind(&channel.channel_id)
    .execute(state.control_db())
    .await
    {
        tracing::warn!(
            channel_id = %channel.channel_id,
            status,
            "failed to update bot inbound channel status: {}",
            err
        );
    }
}

async fn update_channel_cursor(
    state: &AppState,
    channel: &InboundChannelRuntimeConfig,
    cursor: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE bot_agent_channels
        SET inbound_cursor = ?, inbound_status = 'connected', inbound_error = NULL,
            inbound_last_seen_at = CURRENT_TIMESTAMP, inbound_last_message_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND id = ?
        "#,
    )
    .bind(cursor)
    .bind(&channel.tenant_id)
    .bind(&channel.channel_id)
    .execute(state.control_db())
    .await?;
    Ok(())
}

fn telegram_payload_to_aos(update: &Value) -> Option<Value> {
    let message = update
        .get("message")
        .or_else(|| update.get("edited_message"))?;
    let text = message
        .get("text")
        .or_else(|| message.get("caption"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            ["document", "photo", "audio", "video", "voice", "animation"]
                .iter()
                .any(|key| message.get(*key).is_some())
                .then(|| "请处理我上传的附件".to_string())
        })?;
    let chat = message.get("chat");
    let from = message.get("from");
    let chat_id = chat.and_then(|value| value.get("id"));
    let user_id = from.and_then(|value| value.get("id"));
    let username = from
        .and_then(|value| value.get("username"))
        .or_else(|| from.and_then(|value| value.get("first_name")))
        .and_then(Value::as_str);
    Some(json!({
        "text": &text,
        "message": &text,
        "content": &text,
        "chat_id": chat_id,
        "conversation_id": chat_id.map(ToString::to_string),
        "user_id": user_id.map(ToString::to_string),
        "sender_name": username,
        "platform_payload": update
    }))
}

async fn run_telegram_poll_once(
    state: &AppState,
    channel: &InboundChannelRuntimeConfig,
) -> Result<()> {
    let token = telegram_token(channel).ok_or_else(|| {
        AppError::ValidationError("Telegram polling requires Bot Token on the channel".to_string())
    })?;
    let api_base = json_string(channel.config_json.as_ref(), &["apiBase", "api_base"])
        .unwrap_or_else(|| "https://api.telegram.org".to_string())
        .trim_end_matches('/')
        .to_string();
    let timeout = json_u64(
        channel.config_json.as_ref(),
        &["pollTimeoutSecs", "poll_timeout_secs"],
    )
    .unwrap_or(10)
    .min(50);
    let limit = json_u64(channel.config_json.as_ref(), &["pollLimit", "poll_limit"])
        .unwrap_or(20)
        .min(100);
    let mut url = format!("{api_base}/bot{token}/getUpdates?timeout={timeout}&limit={limit}");
    if let Some(offset) = channel
        .inbound_cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        url = append_query_param(&url, "offset", offset);
    }

    update_channel_status(state, channel, "polling", None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout + 15))
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build Telegram client: {e}")))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Telegram getUpdates failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read Telegram response: {e}")))?;
    let payload = serde_json::from_str::<Value>(&body)
        .map_err(|e| AppError::Internal(format!("Telegram returned non-JSON body: {e}; {body}")))?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::Internal(format!(
            "Telegram getUpdates returned HTTP {status}: {payload}"
        )));
    }

    let updates = payload
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut next_offset = channel.inbound_cursor.clone();
    for update in updates {
        if let Some(update_id) = update.get("update_id").and_then(Value::as_i64) {
            next_offset = Some((update_id + 1).to_string());
        }
        let Some(normalized) = telegram_payload_to_aos(&update) else {
            continue;
        };
        crate::routes::bot_agents::enqueue_inbound_payload(
            state.clone(),
            channel.channel_id.clone(),
            normalized,
        )
        .await?;
    }

    if let Some(cursor) = next_offset {
        update_channel_cursor(state, channel, &cursor).await?;
    } else {
        update_channel_status(state, channel, "connected", None).await;
    }
    Ok(())
}

fn parse_dingtalk_open_connection_response(text: &str) -> Result<DingTalkOpenConnectionResponse> {
    let payload = serde_json::from_str::<Value>(text).map_err(|e| {
        AppError::Internal(format!(
            "failed to parse DingTalk Stream open response: {e}; body={text}"
        ))
    })?;
    let endpoint = payload
        .get("endpoint")
        .or_else(|| payload.pointer("/result/endpoint"))
        .or_else(|| payload.pointer("/data/endpoint"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let ticket = payload
        .get("ticket")
        .or_else(|| payload.pointer("/result/ticket"))
        .or_else(|| payload.pointer("/data/ticket"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    match (endpoint, ticket) {
        (Some(endpoint), Some(ticket)) => Ok(DingTalkOpenConnectionResponse { endpoint, ticket }),
        _ => Err(AppError::Internal(format!(
            "DingTalk Stream open response missing endpoint/ticket: {payload}"
        ))),
    }
}

async fn open_dingtalk_stream_ticket(
    channel: &InboundChannelRuntimeConfig,
) -> Result<DingTalkOpenConnectionResponse> {
    let client_id = dingtalk_client_id(channel).ok_or_else(|| {
        AppError::ValidationError(
            "DingTalk Stream requires Client ID/AppKey on the channel".to_string(),
        )
    })?;
    let client_secret = dingtalk_client_secret(channel).ok_or_else(|| {
        AppError::ValidationError(
            "DingTalk Stream requires Client Secret/AppSecret on the channel".to_string(),
        )
    })?;
    let api_base = dingtalk_api_base(channel);
    let subscriptions = channel
        .config_json
        .as_ref()
        .and_then(|value| value.get("subscriptions"))
        .cloned()
        .unwrap_or_else(|| {
            json!([
                { "topic": "/v1.0/im/bot/messages/get", "type": "CALLBACK" }
            ])
        });
    let body = json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "subscriptions": subscriptions,
        "ua": "aos-bot-gateway-rust/0.1.0"
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build DingTalk client: {e}")))?;
    let response = client
        .post(format!("{api_base}/v1.0/gateway/connections/open"))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("DingTalk Stream open failed: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read DingTalk response: {e}")))?;
    if !status.is_success() {
        return Err(AppError::Internal(format!(
            "DingTalk Stream open returned HTTP {status}: {text}"
        )));
    }
    parse_dingtalk_open_connection_response(&text)
}

fn dingtalk_ack(frame: &Value, code: i64, message: &str, data: Value) -> Message {
    let message_id = frame
        .get("headers")
        .or_else(|| frame.get("header"))
        .and_then(|headers| headers.get("messageId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Message::Text(
        json!({
            "code": code,
            "message": message,
            "headers": {
                "messageId": message_id,
                "contentType": "application/json"
            },
            "data": data.to_string()
        })
        .to_string()
        .into(),
    )
}

fn dingtalk_stream_payload_to_aos(frame: &Value) -> Option<Value> {
    let headers = frame.get("headers").or_else(|| frame.get("header"));
    let topic = headers
        .and_then(|value| value.get("topic"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if topic != "/v1.0/im/bot/messages/get" {
        return None;
    }
    let data = frame.get("data")?.as_str()?;
    let payload = serde_json::from_str::<Value>(data).ok()?;
    Some(payload)
}

fn clean_value_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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

fn feishu_event_to_aos(event: &FeishuEvent) -> Option<Value> {
    if event.event_type() != Some("im.message.receive_v1") {
        return None;
    }
    let value = serde_json::to_value(event).ok()?;
    let event_value = value.get("event")?;
    let message = event_value.get("message").unwrap_or(event_value);
    let content = message.get("content").and_then(Value::as_str);
    let message_type = message
        .get("message_type")
        .and_then(Value::as_str)
        .unwrap_or("text");
    let text = if matches!(message_type, "file" | "image" | "audio" | "media") {
        Some("请处理我上传的附件".to_string())
    } else {
        content
            .and_then(feishu_content_to_text)
            .or_else(|| content.map(ToOwned::to_owned))
            .or_else(|| {
                message
                    .get("text")
                    .or_else(|| event_value.get("text"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
    }?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let chat_type = message
        .get("chat_type")
        .or_else(|| event_value.get("chat_type"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let has_mentions = message
        .get("mentions")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let has_group_hint = message.get("open_chat_id").is_some()
        || message.get("open_thread_id").is_some()
        || message.get("thread_id").is_some()
        || message.get("root_id").is_some()
        || message.get("parent_id").is_some();
    let is_direct_chat = chat_type
        .as_deref()
        .map(|value| {
            matches!(
                value,
                "p2p" | "private" | "single" | "direct" | "direct_message"
            )
        })
        .unwrap_or(!has_group_hint && !has_mentions);
    let mentioned = has_mentions || is_direct_chat;
    Some(json!({
        "text": text,
        "message": text,
        "content": text,
        "message_id": message.get("message_id").cloned(),
        "conversation_id": message.get("chat_id").cloned(),
        "chat_id": message.get("chat_id").cloned(),
        "chat_type": chat_type,
        "mentioned": mentioned,
        "platform_adapter": "feishu_stream",
        "user_id": event_value.pointer("/sender/sender_id/open_id").cloned(),
        "sender_name": event_value.pointer("/sender/sender_type").cloned(),
        "platform_payload": value
    }))
}

fn feishu_event_summary(event: &FeishuEvent) -> (Option<String>, Option<String>, Option<String>) {
    let value = serde_json::to_value(event).ok();
    let event_value = value.as_ref().and_then(|value| value.get("event"));
    let message = event_value.and_then(|value| value.get("message"));
    let message_id = message
        .and_then(|value| value.get("message_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let chat_id = message
        .and_then(|value| value.get("chat_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let message_type = message
        .and_then(|value| value.get("message_type"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    (message_id, chat_id, message_type)
}

fn slack_event_payload_to_aos(payload: &Value) -> Option<Value> {
    let envelope_id = payload.get("envelope_id").and_then(Value::as_str);
    let accepts_response_payload = payload
        .get("accepts_response_payload")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let event = payload
        .pointer("/payload/event")
        .or_else(|| payload.get("event"))
        .or_else(|| payload.get("payload"))?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(event_type, "message" | "app_mention") {
        return None;
    }
    if event.get("subtype").and_then(Value::as_str).is_some() {
        return None;
    }
    let text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = if text.is_empty()
        && event
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        "请处理我上传的附件".to_string()
    } else {
        text
    };
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "text": text,
        "message": text,
        "content": text,
        "message_id": event.get("client_msg_id").or_else(|| event.get("ts")).cloned(),
        "conversation_id": event.get("channel").cloned(),
        "chat_id": event.get("channel").cloned(),
        "user_id": event.get("user").cloned(),
        "sender_name": event.get("username").cloned(),
        "slack_envelope_id": envelope_id,
        "slack_accepts_response_payload": accepts_response_payload,
        "platform_payload": payload
    }))
}

async fn open_slack_socket_url(channel: &InboundChannelRuntimeConfig) -> Result<String> {
    let app_token = slack_app_token(channel).ok_or_else(|| {
        AppError::ValidationError(
            "Slack Socket Mode requires an App-Level Token (xapp-...) on the channel signing secret".to_string(),
        )
    })?;
    let api_base = slack_api_base(channel);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build Slack client: {e}")))?;
    let response = client
        .post(format!("{api_base}/apps.connections.open"))
        .bearer_auth(app_token)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Slack Socket Mode open failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read Slack response: {e}")))?;
    let payload = serde_json::from_str::<Value>(&body)
        .map_err(|e| AppError::Internal(format!("Slack returned non-JSON body: {e}; {body}")))?;
    if !status.is_success() || payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::Internal(format!(
            "Slack Socket Mode open returned HTTP {status}: {payload}"
        )));
    }
    payload
        .get("url")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::Internal(format!("Slack Socket Mode response missing url: {payload}"))
        })
}

async fn run_slack_socket(state: AppState, channel: InboundChannelRuntimeConfig) {
    update_channel_status(&state, &channel, "connecting", None).await;
    let result = async {
        let ws_url = open_slack_socket_url(&channel).await?;
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("Slack Socket Mode connect failed: {e}")))?;
        update_channel_status(&state, &channel, "connected", None).await;
        while let Some(next) = ws.next().await {
            let message = next
                .map_err(|e| AppError::Internal(format!("Slack Socket Mode read failed: {e}")))?;
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Message::Ping(bytes) => {
                    ws.send(Message::Pong(bytes)).await.map_err(|e| {
                        AppError::Internal(format!("Slack Socket Mode pong failed: {e}"))
                    })?;
                    continue;
                }
                Message::Close(_) => break,
                _ => continue,
            };
            let frame = serde_json::from_str::<Value>(&text).map_err(|e| {
                AppError::Internal(format!("invalid Slack Socket Mode frame: {e}; {text}"))
            })?;
            if let Some(envelope_id) = frame.get("envelope_id").and_then(Value::as_str) {
                ws.send(Message::Text(
                    json!({ "envelope_id": envelope_id }).to_string().into(),
                ))
                .await
                .map_err(|e| AppError::Internal(format!("Slack Socket Mode ack failed: {e}")))?;
            }
            if let Some(payload) = slack_event_payload_to_aos(&frame) {
                crate::routes::bot_agents::enqueue_inbound_payload(
                    state.clone(),
                    channel.channel_id.clone(),
                    payload,
                )
                .await?;
                sqlx::query(
                    r#"
                    UPDATE bot_agent_channels
                    SET inbound_status = 'connected', inbound_error = NULL,
                        inbound_last_seen_at = CURRENT_TIMESTAMP, inbound_last_message_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = ? AND id = ?
                    "#,
                )
                .bind(&channel.tenant_id)
                .bind(&channel.channel_id)
                .execute(state.control_db())
                .await?;
            }
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = result {
        let error_message = error.to_string();
        mark_retry_cooldown(&channel.channel_id);
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            "Slack Socket Mode adapter stopped: {}",
            error_message
        );
    } else {
        update_channel_status(&state, &channel, "idle", None).await;
    }
    with_active_channels((), |active| {
        active.remove(&channel.channel_id);
    });
}

fn discord_message_payload_to_aos(event: &Value) -> Option<Value> {
    if event
        .get("author")
        .and_then(|author| author.get("bot"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    let text = event
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = if text.is_empty()
        && event
            .get("attachments")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    {
        "请处理我上传的附件".to_string()
    } else {
        text
    };
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "text": text,
        "message": text,
        "content": text,
        "message_id": event.get("id").cloned(),
        "conversation_id": event.get("channel_id").cloned(),
        "chat_id": event.get("channel_id").cloned(),
        "user_id": event.pointer("/author/id").cloned(),
        "sender_name": event.pointer("/author/username").cloned(),
        "platform_payload": event
    }))
}

async fn run_discord_gateway(state: AppState, channel: InboundChannelRuntimeConfig) {
    update_channel_status(&state, &channel, "connecting", None).await;
    let result = async {
        let token = discord_bot_token(&channel).ok_or_else(|| {
            AppError::ValidationError(
                "Discord Gateway requires Bot Token on the channel".to_string(),
            )
        })?;
        let token = discord_gateway_identify_token(&token);
        let ws_url = discord_gateway_url(&channel);
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("Discord Gateway connect failed: {e}")))?;
        let hello_text = match ws.next().await {
            Some(Ok(Message::Text(text))) => text.to_string(),
            Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).to_string(),
            Some(Ok(other)) => {
                return Err(AppError::Internal(format!(
                    "Discord Gateway expected HELLO frame, got {other:?}"
                )));
            }
            Some(Err(e)) => {
                return Err(AppError::Internal(format!(
                    "Discord Gateway HELLO read failed: {e}"
                )));
            }
            None => return Err(AppError::Internal("Discord Gateway closed before HELLO".to_string())),
        };
        let hello = serde_json::from_str::<Value>(&hello_text).map_err(|e| {
            AppError::Internal(format!("invalid Discord Gateway HELLO frame: {e}; {hello_text}"))
        })?;
        let heartbeat_interval = hello
            .pointer("/d/heartbeat_interval")
            .and_then(Value::as_u64)
            .unwrap_or(45_000);
        let identify = json!({
            "op": 2,
            "d": {
                "token": token,
                "intents": discord_intents(&channel),
                "properties": {
                    "os": std::env::consts::OS,
                    "browser": "aos-bot-gateway",
                    "device": "aos-bot-gateway"
                }
            }
        });
        ws.send(Message::Text(identify.to_string().into()))
            .await
            .map_err(|e| AppError::Internal(format!("Discord Gateway identify failed: {e}")))?;
        update_channel_status(&state, &channel, "connected", None).await;
        let mut heartbeat = tokio::time::interval(Duration::from_millis(heartbeat_interval));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sequence: Option<i64> = None;
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    ws.send(Message::Text(json!({ "op": 1, "d": sequence }).to_string().into()))
                        .await
                        .map_err(|e| AppError::Internal(format!("Discord Gateway heartbeat failed: {e}")))?;
                }
                next = ws.next() => {
                    let Some(next) = next else {
                        break;
                    };
                    let message = next
                        .map_err(|e| AppError::Internal(format!("Discord Gateway read failed: {e}")))?;
                    let text = match message {
                        Message::Text(text) => text.to_string(),
                        Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                        Message::Ping(bytes) => {
                            ws.send(Message::Pong(bytes)).await.map_err(|e| {
                                AppError::Internal(format!("Discord Gateway pong failed: {e}"))
                            })?;
                            continue;
                        }
                        Message::Close(_) => break,
                        _ => continue,
                    };
                    let frame = serde_json::from_str::<Value>(&text).map_err(|e| {
                        AppError::Internal(format!("invalid Discord Gateway frame: {e}; {text}"))
                    })?;
                    if let Some(seq) = frame.get("s").and_then(Value::as_i64) {
                        sequence = Some(seq);
                    }
                    match frame.get("op").and_then(Value::as_i64) {
                        Some(0) => {
                            if frame.get("t").and_then(Value::as_str) == Some("MESSAGE_CREATE") {
                                if let Some(payload) = discord_message_payload_to_aos(frame.get("d").unwrap_or(&Value::Null)) {
                                    crate::routes::bot_agents::enqueue_inbound_payload(
                                        state.clone(),
                                        channel.channel_id.clone(),
                                        payload,
                                    )
                                    .await?;
                                    sqlx::query(
                                        r#"
                                        UPDATE bot_agent_channels
                                        SET inbound_status = 'connected', inbound_error = NULL,
                                            inbound_last_seen_at = CURRENT_TIMESTAMP, inbound_last_message_at = CURRENT_TIMESTAMP
                                        WHERE tenant_id = ? AND id = ?
                                        "#,
                                    )
                                    .bind(&channel.tenant_id)
                                    .bind(&channel.channel_id)
                                    .execute(state.control_db())
                                    .await?;
                                }
                            }
                        }
                        Some(1) => {
                            ws.send(Message::Text(json!({ "op": 1, "d": sequence }).to_string().into()))
                                .await
                                .map_err(|e| AppError::Internal(format!("Discord Gateway heartbeat ack failed: {e}")))?;
                        }
                        Some(7) | Some(9) => {
                            return Err(AppError::Internal(
                                "Discord Gateway requested reconnect; runtime will reopen the connection".to_string(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = result {
        let error_message = error.to_string();
        mark_retry_cooldown(&channel.channel_id);
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            "Discord Gateway adapter stopped: {}",
            error_message
        );
    } else {
        update_channel_status(&state, &channel, "idle", None).await;
    }
    with_active_channels((), |active| {
        active.remove(&channel.channel_id);
    });
}

struct FeishuAosEventHandler {
    state: AppState,
    channel_id: String,
}

impl EventHandler for FeishuAosEventHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(
        &self,
        event: FeishuEvent,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<Option<EventResp>, feishu_sdk::core::Error>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let (message_id, chat_id, message_type) = feishu_event_summary(&event);
            tracing::info!(
                channel_id = %self.channel_id,
                event_type = ?event.event_type(),
                message_id = ?message_id,
                chat_id = ?chat_id,
                message_type = ?message_type,
                "Feishu/Lark inbound event received"
            );
            if let Some(payload) = feishu_event_to_aos(&event) {
                crate::routes::bot_agents::enqueue_inbound_payload(
                    self.state.clone(),
                    self.channel_id.clone(),
                    payload,
                )
                .await
                .map_err(|e| feishu_sdk::core::Error::WebSocketError(e.to_string()))?;
                tracing::info!(
                    channel_id = %self.channel_id,
                    message_id = ?message_id,
                    chat_id = ?chat_id,
                    "Feishu/Lark inbound event queued"
                );
            } else {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    event_type = ?event.event_type(),
                    message_id = ?message_id,
                    chat_id = ?chat_id,
                    message_type = ?message_type,
                    "Feishu/Lark inbound event ignored because no text payload was extracted"
                );
            }
            Ok(None)
        })
    }
}

async fn run_feishu_stream(state: AppState, channel: InboundChannelRuntimeConfig) {
    update_channel_status(&state, &channel, "connecting", None).await;
    let result = async {
        let app_id = feishu_app_id(&channel).ok_or_else(|| {
            AppError::ValidationError(
                "Feishu/Lark stream requires App ID on the channel".to_string(),
            )
        })?;
        let app_secret = feishu_app_secret(&channel).ok_or_else(|| {
            AppError::ValidationError(
                "Feishu/Lark stream requires App Secret on the channel".to_string(),
            )
        })?;
        let mut config_builder = FeishuConfig::builder(app_id, app_secret)
            .base_url(feishu_base_url(&channel))
            .log_level(feishu_log_level())
            .request_timeout(Duration::from_secs(30));
        if let Some(app_ticket) =
            json_string(channel.config_json.as_ref(), &["appTicket", "app_ticket"])
        {
            config_builder = config_builder.marketplace_app().app_ticket(app_ticket);
        }
        let config = config_builder.build();
        let mut dispatcher_config = EventDispatcherConfig::new();
        if let Some(token) = json_string(
            channel.config_json.as_ref(),
            &["verificationToken", "verification_token"],
        ) {
            dispatcher_config = dispatcher_config.verification_token(token);
        }
        if let Some(encrypt_key) =
            json_string(channel.config_json.as_ref(), &["encryptKey", "encrypt_key"])
        {
            dispatcher_config = dispatcher_config.encrypt_key(encrypt_key);
        }
        let dispatcher = EventDispatcher::new(dispatcher_config, feishu_logger());
        dispatcher
            .register_handler(Box::new(FeishuAosEventHandler {
                state: state.clone(),
                channel_id: channel.channel_id.clone(),
            }))
            .await;
        let sdk_log_level = feishu_log_level();
        tracing::info!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            platform = %channel.platform,
            sdk_log_level = ?sdk_log_level,
            "Feishu/Lark stream handler registered; expect im.message.receive_v1 events after the app has message receive permissions and event subscription enabled"
        );
        let stream_config = StreamConfig::new()
            .auto_reconnect(true)
            .reconnect_count(-1)
            .reconnect_interval(Duration::from_secs(30))
            .ping_interval(Duration::from_secs(120));
        let client = StreamClient::builder(config)
            .stream_config(stream_config)
            .event_dispatcher(dispatcher)
            .build()
            .map_err(|e| {
                AppError::Internal(format!("failed to build Feishu stream client: {e}"))
            })?;
        update_channel_status(&state, &channel, "connected", None).await;
        client
            .start()
            .await
            .map_err(|e| AppError::Internal(format!("Feishu stream stopped: {e}")))?;
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = result {
        let error_message = error.to_string();
        mark_retry_cooldown(&channel.channel_id);
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            platform = %channel.platform,
            "Feishu/Lark stream adapter stopped: {}",
            error_message
        );
    } else {
        update_channel_status(&state, &channel, "idle", None).await;
    }
    with_active_channels((), |active| {
        active.remove(&channel.channel_id);
    });
}

fn wecom_payload_to_aos(payload: &Value) -> Option<Value> {
    let text = payload
        .pointer("/text/content")
        .or_else(|| payload.pointer("/message/text"))
        .or_else(|| payload.get("content"))
        .or_else(|| payload.get("text"))
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = if text.is_empty()
        && (payload.get("MediaId").is_some() || payload.get("media_id").is_some())
    {
        "请处理我上传的附件".to_string()
    } else {
        text
    };
    if text.is_empty() {
        return None;
    }
    let conversation_id = payload
        .get("conversation_id")
        .or_else(|| payload.get("chat_id"))
        .or_else(|| payload.get("roomid"))
        .or_else(|| payload.get("group_id"))
        .map(ToString::to_string);
    let user_id = payload
        .get("user_id")
        .or_else(|| payload.get("from_user_id"))
        .or_else(|| payload.get("userid"))
        .map(ToString::to_string);
    Some(json!({
        "text": text,
        "message": text,
        "content": text,
        "conversation_id": conversation_id,
        "user_id": user_id,
        "platform_payload": payload
    }))
}

async fn run_wecom_stream(state: AppState, channel: InboundChannelRuntimeConfig) {
    update_channel_status(&state, &channel, "connecting", None).await;
    let result = async {
        let bot_id = wecom_bot_id(&channel).ok_or_else(|| {
            AppError::ValidationError("WeCom stream requires Bot ID on the channel".to_string())
        })?;
        let bot_secret = wecom_bot_secret(&channel).ok_or_else(|| {
            AppError::ValidationError("WeCom stream requires Bot Secret on the channel".to_string())
        })?;
        let ws_base = wecom_ws_base(&channel);
        let mut ws_url = format!("{ws_base}/wwopenai/bot/ws/{bot_id}");
        ws_url = append_query_param(&ws_url, "secret", &bot_secret);
        if let Some(token) = json_string(channel.config_json.as_ref(), &["token"]) {
            ws_url = append_query_param(&ws_url, "token", &token);
        }
        if let Some(encoding_aes_key) = json_string(
            channel.config_json.as_ref(),
            &["encodingAesKey", "encoding_aes_key"],
        ) {
            ws_url = append_query_param(&ws_url, "encoding_aes_key", &encoding_aes_key);
        }

        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("WeCom WebSocket connect failed: {e}")))?;
        update_channel_status(&state, &channel, "connected", None).await;
        while let Some(next) = ws.next().await {
            let message =
                next.map_err(|e| AppError::Internal(format!("WeCom WebSocket read failed: {e}")))?;
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Message::Ping(bytes) => {
                    ws.send(Message::Pong(bytes)).await.map_err(|e| {
                        AppError::Internal(format!("WeCom WebSocket pong failed: {e}"))
                    })?;
                    continue;
                }
                Message::Close(_) => break,
                _ => continue,
            };
            let frame = serde_json::from_str::<Value>(&text).map_err(|e| {
                AppError::Internal(format!("invalid WeCom stream frame: {e}; {text}"))
            })?;
            if let Some(payload) = wecom_payload_to_aos(&frame) {
                crate::routes::bot_agents::enqueue_inbound_payload(
                    state.clone(),
                    channel.channel_id.clone(),
                    payload,
                )
                .await?;
            }
            sqlx::query(
                r#"
                UPDATE bot_agent_channels
                SET inbound_status = 'connected', inbound_error = NULL,
                    inbound_last_seen_at = CURRENT_TIMESTAMP, inbound_last_message_at = CURRENT_TIMESTAMP
                WHERE tenant_id = ? AND id = ?
                "#,
            )
            .bind(&channel.tenant_id)
            .bind(&channel.channel_id)
            .execute(state.control_db())
            .await?;
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = result {
        let error_message = error.to_string();
        mark_retry_cooldown(&channel.channel_id);
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            "WeCom stream adapter stopped: {}",
            error_message
        );
    } else {
        update_channel_status(&state, &channel, "idle", None).await;
    }
    with_active_channels((), |active| {
        active.remove(&channel.channel_id);
    });
}

async fn run_dingtalk_stream(state: AppState, channel: InboundChannelRuntimeConfig) {
    update_channel_status(&state, &channel, "connecting", None).await;
    let result = async {
        let open = open_dingtalk_stream_ticket(&channel).await?;
        let ws_url = append_query_param(&open.endpoint, "ticket", &open.ticket);
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|e| AppError::Internal(format!("DingTalk WebSocket connect failed: {e}")))?;
        update_channel_status(&state, &channel, "connected", None).await;
        while let Some(next) = ws.next().await {
            let message = next
                .map_err(|e| AppError::Internal(format!("DingTalk WebSocket read failed: {e}")))?;
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Message::Ping(bytes) => {
                    ws.send(Message::Pong(bytes)).await.map_err(|e| {
                        AppError::Internal(format!("DingTalk WebSocket pong failed: {e}"))
                    })?;
                    continue;
                }
                Message::Close(_) => break,
                _ => continue,
            };
            let frame = serde_json::from_str::<Value>(&text).map_err(|e| {
                AppError::Internal(format!("invalid DingTalk stream frame: {e}; {text}"))
            })?;
            if let Some(payload) = dingtalk_stream_payload_to_aos(&frame) {
                crate::routes::bot_agents::enqueue_inbound_payload(
                    state.clone(),
                    channel.channel_id.clone(),
                    payload,
                )
                .await?;
            }
            let ack = dingtalk_ack(&frame, 200, "OK", json!({ "response": null }));
            ws.send(ack)
                .await
                .map_err(|e| AppError::Internal(format!("DingTalk WebSocket ack failed: {e}")))?;
            sqlx::query(
                r#"
                UPDATE bot_agent_channels
                SET inbound_status = 'connected', inbound_error = NULL,
                    inbound_last_seen_at = CURRENT_TIMESTAMP, inbound_last_message_at = CURRENT_TIMESTAMP
                WHERE tenant_id = ? AND id = ?
                "#,
            )
            .bind(&channel.tenant_id)
            .bind(&channel.channel_id)
            .execute(state.control_db())
            .await?;
        }
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = result {
        let error_message = error.to_string();
        mark_retry_cooldown(&channel.channel_id);
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            tenant_id = %channel.tenant_id,
            "DingTalk stream adapter stopped: {}",
            error_message
        );
    } else {
        update_channel_status(&state, &channel, "idle", None).await;
    }
    with_active_channels((), |active| {
        active.remove(&channel.channel_id);
    });
}

fn spawn_stream_once(state: AppState, channel: InboundChannelRuntimeConfig) {
    if channel.platform.eq_ignore_ascii_case("dingtalk") {
        tokio::spawn(run_dingtalk_stream(state, channel));
    } else if channel.platform.eq_ignore_ascii_case("slack") {
        tokio::spawn(run_slack_socket(state, channel));
    } else if channel.platform.eq_ignore_ascii_case("discord") {
        tokio::spawn(run_discord_gateway(state, channel));
    } else if channel.platform.eq_ignore_ascii_case("wecom") {
        tokio::spawn(run_wecom_stream(state, channel));
    } else if matches!(
        channel.platform.to_ascii_lowercase().as_str(),
        "feishu" | "lark"
    ) {
        tokio::spawn(run_feishu_stream(state, channel));
    } else {
        let channel_id = channel.channel_id.clone();
        tokio::spawn(async move {
            update_channel_status(
                &state,
                &channel,
                "unsupported",
                Some(
                    "this platform uses Webhook/Event callback inbound; set inbound_mode to auto or webhook",
                ),
            )
            .await;
            with_active_channels((), |active| {
                active.remove(&channel_id);
            });
        });
    }
}

async fn run_channel_once(state: AppState, channel: InboundChannelRuntimeConfig) {
    let mode = resolve_inbound_mode(&channel.platform, &channel.inbound_mode);
    let channel_id = channel.channel_id.clone();
    if matches!(mode.as_str(), "stream" | "socket") && should_skip_for_retry_cooldown(&channel_id) {
        return;
    }
    if !with_active_channels(false, |active| active.insert(channel_id.clone())) {
        return;
    }

    let result = match mode.as_str() {
        "polling" if channel.platform.eq_ignore_ascii_case("telegram") => {
            run_telegram_poll_once(&state, &channel).await
        }
        "polling" => Err(AppError::ValidationError(format!(
            "platform '{}' uses Webhook/Event callback inbound; set inbound_mode to auto or webhook",
            channel.platform
        ))),
        "stream" | "socket" => {
            clear_retry_cooldown(&channel_id);
            spawn_stream_once(state.clone(), channel.clone());
            Ok(())
        }
        "webhook" => {
            update_channel_status(&state, &channel, "webhook_only", None).await;
            Ok(())
        }
        other => Err(AppError::ValidationError(format!(
            "unsupported inbound mode '{other}'"
        ))),
    };
    if let Err(error) = result {
        let error_message = error.to_string();
        if matches!(mode.as_str(), "stream" | "socket") {
            mark_retry_cooldown(&channel_id);
        }
        update_channel_status(&state, &channel, "error", Some(&error_message)).await;
        tracing::warn!(
            channel_id = %channel.channel_id,
            platform = %channel.platform,
            mode = %mode,
            "bot inbound runtime failed: {}",
            error_message
        );
    }
    if !matches!(mode.as_str(), "stream" | "socket") {
        with_active_channels((), |active| {
            active.remove(&channel_id);
        });
    }
}

/// Start background inbound runtime for stream/socket/polling channels.
pub fn start_bot_agent_inbound_runtime(state: AppState) {
    let interval_secs = std::env::var("BOT_GATEWAY_INBOUND_POLL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5)
        .max(2);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let channels = match load_enabled_channels(&state).await {
                Ok(channels) => channels,
                Err(error) => {
                    tracing::warn!("bot inbound runtime failed to load channels: {}", error);
                    continue;
                }
            };
            for channel in channels {
                let state = state.clone();
                tokio::spawn(run_channel_once(state, channel));
            }
        }
    });
    tracing::info!(
        interval_secs,
        "bot inbound runtime started; auto mode prefers stream/socket/polling over webhook"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        clear_retry_cooldown, discord_gateway_identify_token, discord_message_payload_to_aos,
        mark_retry_cooldown, redact_feishu_log_message, resolve_inbound_mode,
        should_skip_for_retry_cooldown, slack_event_payload_to_aos, telegram_payload_to_aos,
        wecom_payload_to_aos,
    };
    use serde_json::json;

    #[test]
    fn auto_prefers_provider_side_runtime_before_webhook() {
        assert_eq!(resolve_inbound_mode("dingtalk", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("feishu", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("lark", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("wecom", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("slack", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("discord", "auto"), "stream");
        assert_eq!(resolve_inbound_mode("telegram", "auto"), "polling");
        assert_eq!(resolve_inbound_mode("whatsapp", "auto"), "webhook");
    }

    #[test]
    fn explicit_inbound_mode_is_respected() {
        assert_eq!(resolve_inbound_mode("telegram", "webhook"), "webhook");
        assert_eq!(resolve_inbound_mode("generic_webhook", "stream"), "stream");
        assert_eq!(resolve_inbound_mode("dingtalk", "polling"), "polling");
        assert_eq!(resolve_inbound_mode("telegram", "socket"), "socket");
    }

    #[test]
    fn unknown_auto_platform_uses_webhook_fallback() {
        assert_eq!(resolve_inbound_mode("custom_platform", "auto"), "webhook");
        assert_eq!(resolve_inbound_mode("generic_webhook", ""), "webhook");
    }

    #[test]
    fn discord_gateway_identify_uses_raw_token() {
        assert_eq!(discord_gateway_identify_token("Bot abc"), "abc");
        assert_eq!(discord_gateway_identify_token("Bearer abc"), "abc");
        assert_eq!(discord_gateway_identify_token("abc"), "abc");
    }

    #[test]
    fn stream_retry_cooldown_can_be_marked_and_cleared() {
        let channel_id = "test-retry-channel";
        clear_retry_cooldown(channel_id);
        assert!(!should_skip_for_retry_cooldown(channel_id));
        mark_retry_cooldown(channel_id);
        assert!(should_skip_for_retry_cooldown(channel_id));
        clear_retry_cooldown(channel_id);
        assert!(!should_skip_for_retry_cooldown(channel_id));
    }

    #[test]
    fn feishu_sdk_logs_redact_connection_credentials() {
        let message = "connected wss://example.test/ws?access_key=abc123&ticket=t-456 token=xyz";
        let redacted = redact_feishu_log_message(message);

        assert_eq!(
            redacted,
            "connected wss://example.test/ws?access_key=[REDACTED]&ticket=[REDACTED] token=[REDACTED]"
        );
        assert_eq!(redact_feishu_log_message(&redacted), redacted);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("t-456"));
        assert!(!redacted.contains("xyz"));
    }

    #[test]
    fn attachment_only_stream_messages_are_not_dropped() {
        let telegram = telegram_payload_to_aos(&json!({
            "message": {
                "message_id": 1,
                "chat": { "id": 2 },
                "from": { "id": 3 },
                "document": { "file_id": "telegram-file" }
            }
        }))
        .expect("Telegram attachment-only message");
        assert_eq!(
            telegram.get("text").and_then(serde_json::Value::as_str),
            Some("请处理我上传的附件")
        );

        let slack = slack_event_payload_to_aos(&json!({
            "payload": { "event": {
                "type": "message",
                "user": "U1",
                "channel": "D1",
                "files": [{ "id": "F1" }]
            } }
        }))
        .expect("Slack attachment-only message");
        assert_eq!(
            slack.get("text").and_then(serde_json::Value::as_str),
            Some("请处理我上传的附件")
        );

        let discord = discord_message_payload_to_aos(&json!({
            "id": "M1",
            "channel_id": "D1",
            "author": { "id": "U1" },
            "attachments": [{ "id": "F1" }]
        }))
        .expect("Discord attachment-only message");
        assert_eq!(
            discord.get("text").and_then(serde_json::Value::as_str),
            Some("请处理我上传的附件")
        );

        let wecom = wecom_payload_to_aos(&json!({
            "MediaId": "wecom-file",
            "user_id": "U1",
            "conversation_id": "D1"
        }))
        .expect("WeCom attachment-only message");
        assert_eq!(
            wecom.get("text").and_then(serde_json::Value::as_str),
            Some("请处理我上传的附件")
        );
    }
}
