use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::http_client::build_http_client_or_default;
use crate::types::{
    is_reserved_extra_body_key, ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent,
    ContentBlockStopEvent, InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent,
    MessageRequest, MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock,
    StreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use super::{Provider, ProviderFuture};

pub const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_DASHSCOPE_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const REQUEST_ID_HEADER: &str = "request-id";
const ALT_REQUEST_ID_HEADER: &str = "x-request-id";
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(128);
// Two retries matches mainstream SDK behavior. Eight exponential retries made
// an unhealthy fallback candidate stall an interactive turn for minutes.
const DEFAULT_MAX_RETRIES: u32 = 2;
const RESPONSES_WEB_SEARCH_BRIEF_CHAR_LIMIT: usize = 12_000;
const RESPONSES_WEB_SEARCH_CURRENT_REQUEST_CHAR_LIMIT: usize = 4_000;
const RESPONSES_WEB_SEARCH_SYSTEM_CHAR_LIMIT: usize = 1_200;
const RESPONSES_WEB_SEARCH_RECENT_CONTEXT_CHAR_LIMIT: usize = 3_600;
const RESPONSES_WEB_SEARCH_RELEVANT_CONTEXT_CHAR_LIMIT: usize = 3_200;
const RESPONSES_WEB_SEARCH_MESSAGE_CHAR_LIMIT: usize = 1_200;
const RESPONSES_WEB_SEARCH_TOOL_RESULT_CHAR_LIMIT: usize = 600;
const RESPONSES_WEB_SEARCH_TOOL_CALL_CHAR_LIMIT: usize = 240;
const RESPONSES_WEB_SEARCH_MAX_RELEVANT_MESSAGES: usize = 5;
const RESPONSES_WEB_SEARCH_MAX_RECENT_MESSAGES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiCompatConfig {
    pub provider_name: &'static str,
    pub api_key_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

const XAI_ENV_VARS: &[&str] = &["XAI_API_KEY"];
const OPENAI_ENV_VARS: &[&str] = &["OPENAI_API_KEY"];
const DASHSCOPE_ENV_VARS: &[&str] = &["DASHSCOPE_API_KEY"];

impl OpenAiCompatConfig {
    #[must_use]
    pub const fn xai() -> Self {
        Self {
            provider_name: "xAI",
            api_key_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: DEFAULT_XAI_BASE_URL,
        }
    }

    #[must_use]
    pub const fn openai() -> Self {
        Self {
            provider_name: "OpenAI",
            api_key_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: DEFAULT_OPENAI_BASE_URL,
        }
    }

    /// Alibaba `DashScope` compatible-mode endpoint (Qwen family models).
    /// Uses the OpenAI-compatible REST shape at /compatible-mode/v1.
    /// Native Alibaba API support for
    /// higher rate limits than going through `OpenRouter`.
    #[must_use]
    pub const fn dashscope() -> Self {
        Self {
            provider_name: "DashScope",
            api_key_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: DEFAULT_DASHSCOPE_BASE_URL,
        }
    }

    #[must_use]
    pub fn credential_env_vars(self) -> &'static [&'static str] {
        match self.provider_name {
            "xAI" => XAI_ENV_VARS,
            "OpenAI" => OPENAI_ENV_VARS,
            "DashScope" => DASHSCOPE_ENV_VARS,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    http: reqwest::Client,
    api_key: String,
    config: OpenAiCompatConfig,
    base_url: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    declared_context_window_tokens: Option<u32>,
    declared_max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LearnedTokenLimitKey {
    base_url: String,
    model: String,
    api_key_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default)]
struct LearnedTokenLimits {
    context_window_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
}

fn learned_token_limits() -> &'static Mutex<HashMap<LearnedTokenLimitKey, LearnedTokenLimits>> {
    static CACHE: OnceLock<Mutex<HashMap<LearnedTokenLimitKey, LearnedTokenLimits>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Copy, Default)]
struct LearnedUnsupportedParameters {
    reasoning_effort: bool,
    max_tokens: bool,
    max_completion_tokens: bool,
    include_reasoning: bool,
    temperature: bool,
    top_p: bool,
    thinking: bool,
    thinking_config: bool,
    enable_thinking: bool,
}

fn learned_unsupported_parameters(
) -> &'static Mutex<HashMap<LearnedTokenLimitKey, LearnedUnsupportedParameters>> {
    static CACHE: OnceLock<Mutex<HashMap<LearnedTokenLimitKey, LearnedUnsupportedParameters>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn min_optional_limit(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn unsupported_parameter_error_text(error: &ApiError) -> Option<String> {
    match error {
        ApiError::Api {
            status,
            message,
            body,
            ..
        } if matches!(status.as_u16(), 400 | 422) => {
            let text =
                format!("{} {body}", message.as_deref().unwrap_or_default()).to_ascii_lowercase();
            [
                "unsupported parameter",
                "unknown parameter",
                "unrecognized parameter",
                "extra inputs are not permitted",
                "does not support",
                "not supported",
                "not allowed",
                "unexpected field",
            ]
            .iter()
            .any(|marker| text.contains(marker))
            .then_some(text)
        }
        ApiError::RetriesExhausted { last_error, .. } => {
            unsupported_parameter_error_text(last_error)
        }
        _ => None,
    }
}

fn contains_parameter_name(text: &str, parameter: &str) -> bool {
    text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|word| word == parameter)
}

impl OpenAiCompatClient {
    const fn config(&self) -> OpenAiCompatConfig {
        self.config
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub(crate) fn output_token_ceiling(&self, model: &str) -> Option<u32> {
        self.effective_max_output_tokens(model)
    }
    #[must_use]
    pub fn new(api_key: impl Into<String>, config: OpenAiCompatConfig) -> Self {
        Self {
            http: build_http_client_or_default(),
            api_key: api_key.into(),
            config,
            base_url: read_base_url(config),
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            declared_context_window_tokens: None,
            declared_max_output_tokens: None,
        }
    }

    pub fn from_env(config: OpenAiCompatConfig) -> Result<Self, ApiError> {
        let Some(api_key) = read_env_non_empty(config.api_key_env)? else {
            return Err(ApiError::missing_credentials(
                config.provider_name,
                config.credential_env_vars(),
            ));
        };
        Ok(Self::new(api_key, config))
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Attach authoritative operator or built-in token ceilings. Provider
    /// rejections may learn a lower endpoint-specific value at runtime.
    #[must_use]
    pub fn with_token_limits(
        mut self,
        context_window_tokens: Option<u32>,
        max_output_tokens: Option<u32>,
    ) -> Self {
        self.declared_context_window_tokens = context_window_tokens.filter(|value| *value > 0);
        self.declared_max_output_tokens = max_output_tokens.filter(|value| *value > 0);
        self
    }

    #[must_use]
    pub fn with_retry_policy(
        mut self,
        max_retries: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        self.max_retries = max_retries;
        self.initial_backoff = initial_backoff;
        self.max_backoff = max_backoff;
        self
    }

    fn learned_limit_key(&self, model: &str) -> LearnedTokenLimitKey {
        LearnedTokenLimitKey {
            base_url: self.base_url.trim().trim_end_matches('/').to_string(),
            model: model.trim().to_ascii_lowercase(),
            api_key_fingerprint: Sha256::digest(self.api_key.as_bytes()).into(),
        }
    }

    fn learned_limits_for(&self, model: &str) -> LearnedTokenLimits {
        let cache = learned_token_limits()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .get(&self.learned_limit_key(model))
            .copied()
            .unwrap_or_default()
    }

    fn remember_token_limits(&self, model: &str, error: &ApiError) -> LearnedTokenLimits {
        let context_window_tokens = error.context_window_tokens_limit();
        let max_output_tokens = error.max_output_tokens_limit();
        if context_window_tokens.is_none() && max_output_tokens.is_none() {
            return self.learned_limits_for(model);
        }

        let mut cache = learned_token_limits()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let limits = cache.entry(self.learned_limit_key(model)).or_default();
        if let Some(limit) = context_window_tokens.filter(|limit| *limit > 0) {
            limits.context_window_tokens = Some(
                limits
                    .context_window_tokens
                    .map_or(limit, |current| current.min(limit)),
            );
        }
        if let Some(limit) = max_output_tokens.filter(|limit| *limit > 0) {
            limits.max_output_tokens = Some(
                limits
                    .max_output_tokens
                    .map_or(limit, |current| current.min(limit)),
            );
        }
        *limits
    }

    fn effective_context_window_tokens(&self, model: &str) -> Option<u32> {
        min_optional_limit(
            self.declared_context_window_tokens.or_else(|| {
                super::model_token_limit(model).map(|limit| limit.context_window_tokens)
            }),
            self.learned_limits_for(model).context_window_tokens,
        )
    }

    fn effective_max_output_tokens(&self, model: &str) -> Option<u32> {
        min_optional_limit(
            self.declared_max_output_tokens
                .or_else(|| super::model_token_limit(model).map(|limit| limit.max_output_tokens)),
            self.learned_limits_for(model).max_output_tokens,
        )
    }

    fn prepare_request(&self, request: &MessageRequest, stream: bool) -> MessageRequest {
        let mut request = MessageRequest {
            stream,
            ..request.clone()
        };
        if let Some(limit) = self.effective_max_output_tokens(&request.model) {
            request.max_tokens = request.max_tokens.min(limit).max(1);
        }
        let learned = learned_unsupported_parameters()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.learned_limit_key(&request.model))
            .copied()
            .unwrap_or_default();
        if learned.reasoning_effort {
            request.reasoning_effort = None;
        }
        if learned.max_completion_tokens {
            request.use_max_completion_tokens = Some(false);
        } else if learned.max_tokens {
            request.use_max_completion_tokens = Some(true);
        }
        if learned.include_reasoning {
            request.include_reasoning = Some(false);
        }
        if learned.temperature {
            request.temperature = None;
        }
        if learned.top_p {
            request.top_p = None;
        }
        if let Some(extra_body) = request.extra_body.as_mut() {
            if learned.thinking {
                extra_body.remove("thinking");
            }
            if learned.thinking_config {
                extra_body.remove("thinking_config");
            }
            if learned.enable_thinking {
                extra_body.remove("enable_thinking");
            }
        }
        request
    }

    fn adjust_request_after_capability_error(
        &self,
        request: &mut MessageRequest,
        error: &ApiError,
        already_adjusted: bool,
    ) -> bool {
        if already_adjusted {
            return false;
        }
        let Some(error_text) = unsupported_parameter_error_text(error) else {
            return false;
        };
        let mut learned = learned_unsupported_parameters()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = learned
            .entry(self.learned_limit_key(&request.model))
            .or_default();
        let parameter =
            if error_text.contains("reasoning_effort") && request.reasoning_effort.is_some() {
                request.reasoning_effort = None;
                entry.reasoning_effort = true;
                "reasoning_effort"
            } else if error_text.contains("max_completion_tokens")
                && request.use_max_completion_tokens == Some(true)
            {
                request.use_max_completion_tokens = Some(false);
                entry.max_completion_tokens = true;
                "max_completion_tokens"
            } else if contains_parameter_name(&error_text, "max_tokens")
                && request.use_max_completion_tokens != Some(true)
            {
                request.use_max_completion_tokens = Some(true);
                entry.max_tokens = true;
                "max_tokens"
            } else if error_text.contains("include_reasoning")
                && request.include_reasoning == Some(true)
            {
                request.include_reasoning = Some(false);
                entry.include_reasoning = true;
                "include_reasoning"
            } else if error_text.contains("temperature") && request.temperature.is_some() {
                request.temperature = None;
                entry.temperature = true;
                "temperature"
            } else if contains_parameter_name(&error_text, "top_p") && request.top_p.is_some() {
                request.top_p = None;
                entry.top_p = true;
                "top_p"
            } else if contains_parameter_name(&error_text, "thinking_config")
                && request
                    .extra_body
                    .as_mut()
                    .is_some_and(|body| body.remove("thinking_config").is_some())
            {
                entry.thinking_config = true;
                "thinking_config"
            } else if contains_parameter_name(&error_text, "enable_thinking")
                && request
                    .extra_body
                    .as_mut()
                    .is_some_and(|body| body.remove("enable_thinking").is_some())
            {
                entry.enable_thinking = true;
                "enable_thinking"
            } else if contains_parameter_name(&error_text, "thinking")
                && request
                    .extra_body
                    .as_mut()
                    .is_some_and(|body| body.remove("thinking").is_some())
            {
                entry.thinking = true;
                "thinking"
            } else {
                return false;
            };
        tracing::warn!(
            provider = self.config.provider_name,
            base_url = %self.base_url,
            model = %request.model,
            parameter,
            "provider explicitly rejected an optional parameter; retrying once without it"
        );
        true
    }

    fn preflight_request(&self, request: &MessageRequest) -> Result<(), ApiError> {
        super::preflight_message_request_with_context_limit(
            request,
            self.effective_context_window_tokens(&request.model),
        )
    }

    fn adjust_request_after_token_error(
        &self,
        request: &mut MessageRequest,
        error: &ApiError,
        already_adjusted: bool,
    ) -> bool {
        let learned = self.remember_token_limits(&request.model, error);
        if already_adjusted {
            return false;
        }
        let Some(limit) = min_optional_limit(
            self.declared_max_output_tokens.or_else(|| {
                super::model_token_limit(&request.model).map(|limit| limit.max_output_tokens)
            }),
            learned.max_output_tokens,
        ) else {
            return false;
        };
        if limit >= request.max_tokens {
            return false;
        }
        tracing::info!(
            provider = self.config.provider_name,
            base_url = %self.base_url,
            model = %request.model,
            previous_max_tokens = request.max_tokens,
            learned_max_tokens = limit,
            "retrying request once with provider-advertised output-token ceiling"
        );
        request.max_tokens = limit.max(1);
        true
    }

    fn expand_reasoning_budget_once(&self, request: &mut MessageRequest) -> bool {
        let ceiling = self
            .effective_max_output_tokens(&request.model)
            .unwrap_or(64_000)
            .max(1);
        let expanded = request
            .max_tokens
            .saturating_mul(2)
            .min(ceiling)
            .max(request.max_tokens);
        if expanded <= request.max_tokens {
            return false;
        }
        tracing::info!(
            provider = self.config.provider_name,
            base_url = %self.base_url,
            model = %request.model,
            previous_max_tokens = request.max_tokens,
            expanded_max_tokens = expanded,
            "retrying reasoning-only truncated response once with a larger output budget"
        );
        request.max_tokens = expanded;
        true
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let mut request = self.prepare_request(request, false);
        let mut token_adjusted = false;
        let mut capability_adjusted = false;
        let mut reasoning_adjusted = false;
        loop {
            self.preflight_request(&request)?;
            let response = match self.send_with_retry(&request).await {
                Ok(response) => response,
                Err(error) => {
                    if self.adjust_request_after_token_error(&mut request, &error, token_adjusted) {
                        token_adjusted = true;
                        continue;
                    }
                    if self.adjust_request_after_capability_error(
                        &mut request,
                        &error,
                        capability_adjusted,
                    ) {
                        capability_adjusted = true;
                        continue;
                    }
                    self.remember_token_limits(&request.model, &error);
                    return Err(error);
                }
            };
            let request_id = request_id_from_headers(response.headers());
            let body = response.text().await.map_err(ApiError::from)?;
            // Some compatible backends incorrectly return an error envelope
            // with HTTP 200. Treat it exactly like a normal provider error so
            // token-limit adaptation still works.
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(err_obj) = raw.get("error") {
                    let msg = err_obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("provider returned an error")
                        .to_string();
                    let code = err_obj
                        .get("code")
                        .and_then(serde_json::Value::as_u64)
                        .map(|c| c as u16);
                    let error = ApiError::Api {
                        status: reqwest::StatusCode::from_u16(code.unwrap_or(400))
                            .unwrap_or(reqwest::StatusCode::BAD_REQUEST),
                        error_type: err_obj
                            .get("type")
                            .and_then(|t| t.as_str())
                            .map(str::to_owned),
                        message: Some(msg),
                        request_id,
                        body,
                        retryable: false,
                        retry_after: None,
                    };
                    if self.adjust_request_after_token_error(&mut request, &error, token_adjusted) {
                        token_adjusted = true;
                        continue;
                    }
                    if self.adjust_request_after_capability_error(
                        &mut request,
                        &error,
                        capability_adjusted,
                    ) {
                        capability_adjusted = true;
                        continue;
                    }
                    self.remember_token_limits(&request.model, &error);
                    return Err(error);
                }
            }
            let payload =
                serde_json::from_str::<ChatCompletionResponse>(&body).map_err(|error| {
                    ApiError::json_deserialize(
                        self.config.provider_name,
                        &request.model,
                        &body,
                        error,
                    )
                })?;
            let mut normalized = normalize_response(&request.model, payload)?;
            if normalized.request_id.is_none() {
                normalized.request_id = request_id;
            }
            if !reasoning_adjusted
                && response_exhausted_in_reasoning(&normalized)
                && self.expand_reasoning_budget_once(&mut request)
            {
                reasoning_adjusted = true;
                continue;
            }
            return Ok(normalized);
        }
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let mut request = self.prepare_request(request, true);
        let mut token_adjusted = false;
        let mut capability_adjusted = false;
        self.preflight_request(&request)?;

        let mut attempts = 0;

        loop {
            attempts += 1;
            let response = match self.send_raw_streaming(&request).await {
                Ok(r) => r,
                Err(e) if Self::is_retryable_error(&e) && attempts <= self.max_retries => {
                    tracing::warn!(
                        "stream_message: retryable HTTP/network error on attempt {}, sleeping: {}",
                        attempts,
                        e
                    );
                    tokio::time::sleep(self.jittered_backoff_for_attempt(attempts)?).await;
                    continue;
                }
                Err(e) => return Err(e),
            };

            let status = response.status();

            if status.is_success() {
                return Ok(MessageStream {
                    request_id: request_id_from_headers(response.headers()),
                    response,
                    parser: OpenAiSseParser::with_context(
                        self.config.provider_name,
                        request.model.clone(),
                    ),
                    pending: VecDeque::new(),
                    done: false,
                    state: StreamState::new(request.model.clone()),
                });
            }

            // For non-2xx, check if it's retryable. Quota / daily-limit 429s
            // are hard failures for this key and should fail over immediately.
            if Self::status_is_retryable(status) && attempts <= self.max_retries {
                // Honor the server's Retry-After hint on 429 before falling
                // back to jittered exponential backoff.
                let retry_after = parse_retry_after(response.headers(), status);
                let request_id = request_id_from_headers(response.headers());
                let body = response.text().await.unwrap_or_default();
                if is_non_retryable_quota_error(status, None, None, &body) {
                    return Err(Self::api_error_from_status_body_with_retry_after(
                        status,
                        request_id,
                        body,
                        retry_after,
                    ));
                }
                tracing::warn!(
                    "stream_message: retryable status {} on attempt {}, body: {}",
                    status,
                    attempts,
                    &body[..body.len().min(300)]
                );
                let delay = match retry_after {
                    Some(delay) => delay,
                    None => self.jittered_backoff_for_attempt(attempts)?,
                };
                tokio::time::sleep(delay).await;
                continue;
            }

            // Non-retryable error — read error and return
            let error = Self::read_error_from_response(status, response).await;
            if self.adjust_request_after_token_error(&mut request, &error, token_adjusted) {
                token_adjusted = true;
                self.preflight_request(&request)?;
                continue;
            }
            if self.adjust_request_after_capability_error(&mut request, &error, capability_adjusted)
            {
                capability_adjusted = true;
                self.preflight_request(&request)?;
                continue;
            }
            self.remember_token_limits(&request.model, &error);
            return Err(error);
        }
    }

    pub async fn send_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let mut request = self.prepare_request(request, false);
        let mut token_adjusted = false;
        loop {
            self.preflight_request(&request)?;
            let request_url = responses_endpoint(&self.base_url);
            let mut payload = build_responses_web_search_request(&request);
            adapt_responses_web_search_request_for_provider(
                &mut payload,
                &request.model,
                &self.base_url,
            );
            log_responses_web_search_payload("send", &self.base_url, &request, &payload);
            let response = self
                .http
                .post(&request_url)
                .header("content-type", "application/json")
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await
                .map_err(ApiError::from)?;
            let status = response.status();
            if !status.is_success() {
                let request_id = request_id_from_headers(response.headers());
                let headers_summary = responses_error_headers_summary(response.headers());
                let body = response.text().await.unwrap_or_default();
                log_responses_web_search_error(
                    "send",
                    &self.base_url,
                    &request,
                    status,
                    request_id.as_deref(),
                    &headers_summary,
                    &body,
                );
                let error = Self::api_error_from_status_body(status, request_id, body);
                if self.adjust_request_after_token_error(&mut request, &error, token_adjusted) {
                    token_adjusted = true;
                    continue;
                }
                self.remember_token_limits(&request.model, &error);
                return Err(error);
            }
            let request_id = request_id_from_headers(response.headers());
            let body = response.text().await.map_err(ApiError::from)?;
            let payload = serde_json::from_str::<ResponsesApiResponse>(&body).map_err(|error| {
                ApiError::json_deserialize(self.config.provider_name, &request.model, &body, error)
            })?;
            let mut normalized = normalize_responses_response(&request.model, payload)?;
            if normalized.request_id.is_none() {
                normalized.request_id = request_id;
            }
            return Ok(normalized);
        }
    }

    pub async fn stream_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<ResponsesMessageStream, ApiError> {
        let mut request = self.prepare_request(request, true);
        let mut token_adjusted = false;
        loop {
            self.preflight_request(&request)?;
            let request_url = responses_endpoint(&self.base_url);
            let mut body = build_responses_web_search_request(&request);
            adapt_responses_web_search_request_for_provider(
                &mut body,
                &request.model,
                &self.base_url,
            );
            body["stream"] = Value::Bool(true);
            log_responses_web_search_payload("stream", &self.base_url, &request, &body);
            let response = self
                .http
                .post(&request_url)
                .header("content-type", "application/json")
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(ApiError::from)?;
            let status = response.status();
            if !status.is_success() {
                let request_id = request_id_from_headers(response.headers());
                let headers_summary = responses_error_headers_summary(response.headers());
                let body = response.text().await.unwrap_or_default();
                log_responses_web_search_error(
                    "stream",
                    &self.base_url,
                    &request,
                    status,
                    request_id.as_deref(),
                    &headers_summary,
                    &body,
                );
                let error = Self::api_error_from_status_body(status, request_id, body);
                if self.adjust_request_after_token_error(&mut request, &error, token_adjusted) {
                    token_adjusted = true;
                    continue;
                }
                self.remember_token_limits(&request.model, &error);
                return Err(error);
            }
            return Ok(ResponsesMessageStream {
                request_id: request_id_from_headers(response.headers()),
                response,
                parser: ResponsesSseParser::with_context(
                    self.config.provider_name,
                    request.model.clone(),
                ),
                pending: VecDeque::new(),
                done: false,
                state: ResponsesStreamState::new(request.model),
            });
        }
    }

    /// Returns true if the `ApiError` is a retryable network/connection error.
    fn is_retryable_error(e: &ApiError) -> bool {
        match e {
            ApiError::Http(e) => e.is_timeout() || e.is_connect(),
            _ => false,
        }
    }

    /// Check if an HTTP status code is retryable.
    fn status_is_retryable(status: reqwest::StatusCode) -> bool {
        matches!(status.as_u16(), 408 | 409 | 429 | 500 | 502 | 503 | 504)
    }

    /// Read error details from a non-2xx response without consuming the body
    /// in a way that prevents subsequent reads.
    async fn read_error_from_response(
        status: reqwest::StatusCode,
        response: reqwest::Response,
    ) -> ApiError {
        let request_id = request_id_from_headers(response.headers());
        let retry_after = parse_retry_after(response.headers(), status);
        let body = response.text().await.unwrap_or_default();
        Self::api_error_from_status_body_with_retry_after(status, request_id, body, retry_after)
    }

    fn api_error_from_status_body(
        status: reqwest::StatusCode,
        request_id: Option<String>,
        body: String,
    ) -> ApiError {
        Self::api_error_from_status_body_with_retry_after(status, request_id, body, None)
    }

    fn api_error_from_status_body_with_retry_after(
        status: reqwest::StatusCode,
        request_id: Option<String>,
        body: String,
        retry_after: Option<Duration>,
    ) -> ApiError {
        let parsed_error = serde_json::from_str::<ErrorEnvelope>(&body).ok();

        ApiError::Api {
            status,
            error_type: parsed_error
                .as_ref()
                .and_then(|error| error.error.error_type.clone()),
            message: parsed_error
                .as_ref()
                .and_then(|error| error.error.message.clone())
                .or_else(|| {
                    if body.len() > 200 {
                        Some(format!("{}...", &body[..200]))
                    } else {
                        Some(body.clone())
                    }
                }),
            request_id,
            body,
            retryable: false,
            retry_after,
        }
    }

    /// Send a streaming request without retry logic (retries are handled at a higher level
    /// via `ConversationRuntime`'s primary/fallback key mechanism).
    async fn send_raw_streaming(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let request_url = chat_completions_endpoint(&self.base_url);
        self.http
            .post(&request_url)
            .header("content-type", "application/json")
            .bearer_auth(&self.api_key)
            .json(&build_chat_completion_request(
                request,
                self.config(),
                &self.base_url,
            ))
            .send()
            .await
            .map_err(ApiError::from)
    }

    async fn send_with_retry(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let mut attempts = 0;

        let last_error = loop {
            attempts += 1;
            let retryable_error = match self.send_raw_request(request).await {
                Ok(response) => match expect_success(response).await {
                    Ok(response) => return Ok(response),
                    Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                    Err(error) => return Err(error),
                },
                Err(error) if error.is_retryable() && attempts <= self.max_retries + 1 => error,
                Err(error) => return Err(error),
            };

            if attempts > self.max_retries {
                break retryable_error;
            }

            tokio::time::sleep(self.jittered_backoff_for_attempt(attempts)?).await;
        };

        Err(ApiError::RetriesExhausted {
            attempts,
            last_error: Box::new(last_error),
        })
    }

    async fn send_raw_request(
        &self,
        request: &MessageRequest,
    ) -> Result<reqwest::Response, ApiError> {
        let request_url = chat_completions_endpoint(&self.base_url);
        self.http
            .post(&request_url)
            .header("content-type", "application/json")
            .bearer_auth(&self.api_key)
            .json(&build_chat_completion_request(
                request,
                self.config(),
                &self.base_url,
            ))
            .send()
            .await
            .map_err(ApiError::from)
    }

    fn backoff_for_attempt(&self, attempt: u32) -> Result<Duration, ApiError> {
        let Some(multiplier) = 1_u32.checked_shl(attempt.saturating_sub(1)) else {
            return Err(ApiError::BackoffOverflow {
                attempt,
                base_delay: self.initial_backoff,
            });
        };
        Ok(self
            .initial_backoff
            .checked_mul(multiplier)
            .map_or(self.max_backoff, |delay| delay.min(self.max_backoff)))
    }

    fn jittered_backoff_for_attempt(&self, attempt: u32) -> Result<Duration, ApiError> {
        let base = self.backoff_for_attempt(attempt)?;
        Ok(base + jitter_for_base(base))
    }
}

/// Process-wide counter that guarantees distinct jitter samples even when
/// the system clock resolution is coarser than consecutive retry sleeps.
static JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns a random additive jitter in `[0, base]` to decorrelate retries
/// Deserialize a JSON field as a `Vec<T>`, treating an explicit `null` value
/// the same as a missing field (i.e. as an empty vector).
/// Some OpenAI-compatible providers emit `"tool_calls": null` instead of
/// omitting the field or using `[]`, which serde's `#[serde(default)]` alone
/// does not tolerate — `default` only handles absent keys, not null values.
fn deserialize_null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// from multiple concurrent clients. Entropy is drawn from the nanosecond
/// wall clock mixed with a monotonic counter and run through a splitmix64
/// finalizer; adequate for retry jitter (no cryptographic requirement).
fn jitter_for_base(base: Duration) -> Duration {
    let base_nanos = u64::try_from(base.as_nanos()).unwrap_or(u64::MAX);
    if base_nanos == 0 {
        return Duration::ZERO;
    }
    let raw_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let tick = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut mixed = raw_nanos
        .wrapping_add(tick)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^= mixed >> 31;
    let jitter_nanos = mixed % base_nanos.saturating_add(1);
    Duration::from_nanos(jitter_nanos)
}

impl Provider for OpenAiCompatClient {
    type Stream = MessageStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        Box::pin(async move { self.send_message(request).await })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        Box::pin(async move { self.stream_message(request).await })
    }
}

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    response: reqwest::Response,
    parser: OpenAiSseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    state: StreamState,
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }

            if self.done {
                self.pending.extend(self.state.finish()?);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            if let Some(chunk) = self.response.chunk().await? {
                for parsed in self.parser.push(&chunk)? {
                    self.pending.extend(self.state.ingest_chunk(parsed)?);
                }
            } else {
                if self.parser.parsed_chunks == 0 {
                    tracing::warn!(
                        provider = %self.parser.provider,
                        model = %self.parser.model,
                        diagnostics = %self.parser.diagnostic_summary(),
                        "OpenAI-compatible SSE stream ended without parsed chat completion chunks"
                    );
                } else {
                    tracing::debug!("SSE stream ended (chunk=None)");
                }
                self.done = true;
            }
        }
    }
}

#[derive(Debug)]
pub struct ResponsesMessageStream {
    request_id: Option<String>,
    response: reqwest::Response,
    parser: ResponsesSseParser,
    pending: VecDeque<StreamEvent>,
    done: bool,
    state: ResponsesStreamState,
}

impl ResponsesMessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }

            if self.done {
                self.pending.extend(self.state.finish()?);
                if let Some(event) = self.pending.pop_front() {
                    return Ok(Some(event));
                }
                return Ok(None);
            }

            if let Some(chunk) = self.response.chunk().await? {
                for event in self.parser.push(&chunk)? {
                    self.pending.extend(self.state.ingest_event(event)?);
                }
            } else {
                if self.parser.parsed_events == 0 {
                    tracing::warn!(
                        provider = %self.parser.provider,
                        model = %self.parser.model,
                        diagnostics = %self.parser.diagnostic_summary(),
                        "OpenAI Responses SSE stream ended without parsed events"
                    );
                }
                self.done = true;
            }
        }
    }

    #[must_use]
    pub fn provider_metadata(&self) -> Option<Value> {
        self.state.provider_metadata()
    }
}

#[derive(Debug, Default)]
struct ResponsesSseParser {
    buffer: Vec<u8>,
    provider: String,
    model: String,
    parsed_events: usize,
    ignored_frames: usize,
    done_frames: usize,
    ignored_samples: Vec<String>,
}

impl ResponsesSseParser {
    fn with_context(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            provider: provider.into(),
            model: model.into(),
            parsed_events: 0,
            ignored_frames: 0,
            done_frames: 0,
            ignored_samples: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ResponsesStreamEvent>, ApiError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            let summary = summarize_sse_frame(&frame);
            match parse_responses_sse_frame(&frame, &self.provider, &self.model)? {
                Some(event) => {
                    self.parsed_events += 1;
                    events.push(event);
                }
                None => {
                    self.ignored_frames += 1;
                    if summary.contains("data=[DONE]") {
                        self.done_frames += 1;
                    }
                    if self.ignored_samples.len() < 3 {
                        self.ignored_samples.push(summary);
                    }
                }
            }
        }

        Ok(events)
    }

    fn diagnostic_summary(&self) -> String {
        format!(
            "parsed_events={} ignored_frames={} done_frames={} samples=[{}]",
            self.parsed_events,
            self.ignored_frames,
            self.done_frames,
            self.ignored_samples.join(" | ")
        )
    }
}

#[derive(Debug, Clone)]
struct ResponsesStreamEvent {
    event_type: String,
    payload: Value,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct ResponsesStreamState {
    model: String,
    response_id: Option<String>,
    response_model: Option<String>,
    message_started: bool,
    reasoning_started: bool,
    reasoning_finished: bool,
    text_started: bool,
    text_finished: bool,
    completed: bool,
    usage: Option<Usage>,
    web_search_seen: bool,
    web_search_actions: Vec<Value>,
    citations: Vec<Value>,
    function_calls: BTreeMap<u32, ResponsesFunctionCallState>,
}

impl ResponsesStreamState {
    fn new(model: String) -> Self {
        Self {
            model,
            response_id: None,
            response_model: None,
            message_started: false,
            reasoning_started: false,
            reasoning_finished: false,
            text_started: false,
            text_finished: false,
            completed: false,
            usage: None,
            web_search_seen: false,
            web_search_actions: Vec::new(),
            citations: Vec::new(),
            function_calls: BTreeMap::new(),
        }
    }

    fn ingest_event(&mut self, event: ResponsesStreamEvent) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = Vec::new();
        match event.event_type.as_str() {
            "response.created" | "response.in_progress" => {
                self.capture_response_metadata(&event.payload);
                out.extend(self.ensure_message_started());
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                out.extend(self.ensure_message_started());
                out.extend(self.ensure_reasoning_started());
                if let Some(delta) = event.payload.get("delta").and_then(Value::as_str) {
                    if !delta.is_empty() {
                        out.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index: 0,
                            delta: ContentBlockDelta::ThinkingDelta {
                                thinking: delta.to_string(),
                            },
                        }));
                    }
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                out.extend(self.stop_reasoning_block());
            }
            "response.output_text.delta" => {
                out.extend(self.ensure_message_started());
                out.extend(self.stop_reasoning_block());
                out.extend(self.ensure_text_started());
                if let Some(delta) = event.payload.get("delta").and_then(Value::as_str) {
                    if !delta.is_empty() {
                        out.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index: self.text_block_index(),
                            delta: ContentBlockDelta::TextDelta {
                                text: delta.to_string(),
                            },
                        }));
                    }
                }
            }
            "response.output_text.done" | "response.content_part.done" => {
                self.capture_response_content_part_metadata(&event.payload);
                out.extend(self.stop_text_block());
            }
            "response.output_text.annotation.added" => {
                self.capture_response_output_text_annotation(&event.payload);
            }
            "response.output_item.added" => {
                self.capture_response_output_item_metadata(&event.payload);
                out.extend(self.ingest_response_output_item(&event.payload)?);
            }
            "response.function_call_arguments.delta" => {
                out.extend(self.ingest_function_call_delta(&event.payload)?);
            }
            "response.function_call_arguments.done" | "response.output_item.done" => {
                self.capture_response_output_item_metadata(&event.payload);
                out.extend(self.finish_function_call_from_payload(&event.payload)?);
            }
            "response.completed" => {
                self.capture_response_metadata(&event.payload);
                self.capture_response_web_search_metadata(&event.payload);
                self.capture_usage_from_completed(&event.payload);
                if !self.text_started {
                    if let Some(text) = responses_output_text_from_value(&event.payload)
                        .filter(|value| !value.is_empty())
                    {
                        out.extend(self.ensure_message_started());
                        out.extend(self.stop_reasoning_block());
                        out.extend(self.ensure_text_started());
                        out.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index: self.text_block_index(),
                            delta: ContentBlockDelta::TextDelta { text },
                        }));
                    }
                }
                self.completed = true;
                out.extend(self.finish()?);
            }
            "response.failed" | "response.incomplete" => {
                self.capture_response_metadata(&event.payload);
                return Err(ApiError::InvalidSseFrame(
                    "responses stream failed or ended incomplete",
                ));
            }
            kind if kind.starts_with("response.web_search_call.") => {
                self.web_search_seen = true;
                if let Some(action) = event.payload.get("action").cloned().or_else(|| {
                    event
                        .payload
                        .get("item")
                        .and_then(|item| item.get("action"))
                        .cloned()
                }) {
                    self.web_search_actions.push(action);
                }
                tracing::debug!(
                    event_type = kind,
                    payload = %truncate_chars(&event.payload.to_string(), 300),
                    "OpenAI Responses web_search streaming event observed"
                );
            }
            _ => {}
        }
        Ok(out)
    }

    fn ingest_response_output_item(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<StreamEvent>, ApiError> {
        let Some(item) = payload.get("item") else {
            return Ok(Vec::new());
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Ok(Vec::new());
        }
        let index = payload
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_else(|| self.function_calls.len() as u32);
        let state = self.function_calls.entry(index).or_default();
        state.id = item
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| state.id.clone());
        state.call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| state.call_id.clone());
        state.name = item
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| state.name.clone());
        if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
            state.arguments.push_str(arguments);
        }
        self.start_function_call_if_ready(index)
    }

    fn ingest_function_call_delta(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<StreamEvent>, ApiError> {
        let index = payload
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let state = self.function_calls.entry(index).or_default();
        if let Some(delta) = payload.get("delta").and_then(Value::as_str) {
            state.arguments.push_str(delta);
        }
        self.function_call_delta_if_started(index)
    }

    fn finish_function_call_from_payload(
        &mut self,
        payload: &Value,
    ) -> Result<Vec<StreamEvent>, ApiError> {
        let index = payload
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        if let Some(item) = payload.get("item") {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let state = self.function_calls.entry(index).or_default();
                state.id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| state.id.clone());
                state.call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| state.call_id.clone());
                state.name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| state.name.clone());
                if state.arguments.is_empty() {
                    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                        state.arguments.push_str(arguments);
                    }
                }
            }
        }
        self.stop_function_call_if_ready(index)
    }

    fn ensure_message_started(&mut self) -> Vec<StreamEvent> {
        if self.message_started {
            return Vec::new();
        }
        self.message_started = true;
        vec![StreamEvent::MessageStart(MessageStartEvent {
            message: MessageResponse {
                id: self
                    .response_id
                    .clone()
                    .unwrap_or_else(|| "responses_stream".to_string()),
                kind: "message".to_string(),
                role: "assistant".to_string(),
                content: Vec::new(),
                model: self
                    .response_model
                    .clone()
                    .unwrap_or_else(|| self.model.clone()),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
                request_id: None,
                provider_metadata: None,
            },
        })]
    }

    fn ensure_text_started(&mut self) -> Vec<StreamEvent> {
        if self.text_started {
            return Vec::new();
        }
        self.text_started = true;
        vec![StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: self.text_block_index(),
            content_block: OutputContentBlock::Text {
                text: String::new(),
            },
        })]
    }

    fn stop_text_block(&mut self) -> Vec<StreamEvent> {
        if !self.text_started || self.text_finished {
            return Vec::new();
        }
        self.text_finished = true;
        vec![StreamEvent::ContentBlockStop(ContentBlockStopEvent {
            index: self.text_block_index(),
        })]
    }

    fn ensure_reasoning_started(&mut self) -> Vec<StreamEvent> {
        if self.reasoning_started || self.reasoning_finished {
            return Vec::new();
        }
        self.reasoning_started = true;
        vec![StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: 0,
            content_block: OutputContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            },
        })]
    }

    fn stop_reasoning_block(&mut self) -> Vec<StreamEvent> {
        if !self.reasoning_started || self.reasoning_finished {
            return Vec::new();
        }
        self.reasoning_finished = true;
        vec![StreamEvent::ContentBlockStop(ContentBlockStopEvent {
            index: 0,
        })]
    }

    const fn text_block_index(&self) -> u32 {
        if self.reasoning_started {
            1
        } else {
            0
        }
    }

    const fn tool_call_block_index_offset(&self) -> u32 {
        if self.reasoning_started {
            2
        } else {
            1
        }
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = Vec::new();
        out.extend(self.ensure_message_started());
        out.extend(self.stop_reasoning_block());
        out.extend(self.stop_text_block());
        let indexes: Vec<u32> = self.function_calls.keys().copied().collect();
        for index in indexes {
            out.extend(self.stop_function_call_if_ready(index)?);
        }
        if self.completed {
            out.push(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some("end_turn".to_string()),
                    stop_sequence: None,
                },
                usage: self.usage.clone().unwrap_or_default(),
            }));
            out.push(StreamEvent::MessageStop(MessageStopEvent {}));
            self.completed = false;
        }
        Ok(out)
    }

    fn start_function_call_if_ready(&mut self, index: u32) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = self.stop_reasoning_block();
        let index_offset = self.tool_call_block_index_offset();
        let Some(state) = self.function_calls.get_mut(&index) else {
            return Ok(out);
        };
        if state.started || state.name.is_none() {
            return Ok(out);
        }
        state.started = true;
        out.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
            index: index + index_offset,
            content_block: OutputContentBlock::ToolUse {
                id: state
                    .call_id
                    .clone()
                    .or_else(|| state.id.clone())
                    .unwrap_or_else(|| format!("responses_call_{index}")),
                name: state.name.clone().unwrap_or_default(),
                input: json!({}),
            },
        }));
        Ok(out)
    }

    fn function_call_delta_if_started(&mut self, index: u32) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = self.start_function_call_if_ready(index)?;
        let index_offset = self.tool_call_block_index_offset();
        let Some(state) = self.function_calls.get_mut(&index) else {
            return Ok(out);
        };
        if !state.started || state.emitted_len >= state.arguments.len() {
            return Ok(out);
        }
        let delta = state.arguments[state.emitted_len..].to_string();
        state.emitted_len = state.arguments.len();
        out.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
            index: index + index_offset,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: delta,
            },
        }));
        Ok(out)
    }

    fn stop_function_call_if_ready(&mut self, index: u32) -> Result<Vec<StreamEvent>, ApiError> {
        let mut out = self.function_call_delta_if_started(index)?;
        let index_offset = self.tool_call_block_index_offset();
        let Some(state) = self.function_calls.get_mut(&index) else {
            return Ok(out);
        };
        if state.started && !state.stopped {
            state.stopped = true;
            out.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                index: index + index_offset,
            }));
        }
        Ok(out)
    }

    fn capture_response_metadata(&mut self, payload: &Value) {
        let response = payload.get("response").unwrap_or(payload);
        if self.response_id.is_none() {
            self.response_id = response
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if self.response_model.is_none() {
            self.response_model = response
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
    }

    fn capture_usage_from_completed(&mut self, payload: &Value) {
        let usage = payload
            .get("response")
            .and_then(|response| response.get("usage"))
            .or_else(|| payload.get("usage"));
        let Some(usage) = usage else {
            return;
        };
        self.usage = Some(Usage {
            input_tokens: usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            output_tokens: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
        });
    }

    fn capture_response_web_search_metadata(&mut self, payload: &Value) {
        let response = payload.get("response").unwrap_or(payload);
        for item in response
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("web_search_call") => {
                    self.web_search_seen = true;
                    if let Some(action) = item.get("action") {
                        self.web_search_actions.push(action.clone());
                    }
                }
                Some("message") => {
                    for part in item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let part_text = part.get("text").and_then(Value::as_str);
                        self.citations.extend(
                            part.get("annotations")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|annotation| {
                                    responses_annotation_value_to_metadata(annotation, part_text)
                                }),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn capture_response_output_item_metadata(&mut self, payload: &Value) {
        let Some(item) = payload.get("item") else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("web_search_call") {
            return;
        }
        self.web_search_seen = true;
        if let Some(action) = item.get("action") {
            self.web_search_actions.push(action.clone());
        }
    }

    fn capture_response_content_part_metadata(&mut self, payload: &Value) {
        let Some(part) = payload.get("part") else {
            return;
        };
        let part_text = part.get("text").and_then(Value::as_str);
        self.citations.extend(
            part.get("annotations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|annotation| {
                    responses_annotation_value_to_metadata(annotation, part_text)
                }),
        );
    }

    fn capture_response_output_text_annotation(&mut self, payload: &Value) {
        let Some(annotation) = payload.get("annotation") else {
            return;
        };
        if let Some(value) = responses_annotation_value_to_metadata(annotation, None) {
            self.citations.push(value);
        }
    }

    fn provider_metadata(&self) -> Option<Value> {
        (self.web_search_seen || !self.web_search_actions.is_empty() || !self.citations.is_empty())
            .then(|| {
                json!({
                    "responses": {
                        "citations": self.citations.clone(),
                        "webSearchSeen": self.web_search_seen,
                        "webSearchActions": self.web_search_actions.clone(),
                    }
                })
            })
    }
}

#[derive(Debug, Default)]
struct ResponsesFunctionCallState {
    id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_len: usize,
    started: bool,
    stopped: bool,
}

#[derive(Debug, Default)]
struct OpenAiSseParser {
    buffer: Vec<u8>,
    provider: String,
    model: String,
    parsed_chunks: usize,
    ignored_frames: usize,
    done_frames: usize,
    ignored_samples: Vec<String>,
}

impl OpenAiSseParser {
    fn with_context(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            provider: provider.into(),
            model: model.into(),
            parsed_chunks: 0,
            ignored_frames: 0,
            done_frames: 0,
            ignored_samples: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<ChatCompletionChunk>, ApiError> {
        self.buffer.extend_from_slice(chunk);

        let mut events = Vec::new();

        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            let summary = summarize_sse_frame(&frame);
            match parse_sse_frame(&frame, &self.provider, &self.model)? {
                Some(event) => {
                    self.parsed_chunks += 1;
                    tracing::debug!(
                        "SSE frame parsed: {} bytes → ChatCompletionChunk id={} choices={}",
                        frame.len(),
                        event.id,
                        event.choices.len()
                    );
                    events.push(event);
                }
                None => {
                    self.ignored_frames += 1;
                    if summary.contains("data=[DONE]") {
                        self.done_frames += 1;
                    }
                    if self.ignored_samples.len() < 3 {
                        self.ignored_samples.push(summary);
                    }
                }
            }
        }

        Ok(events)
    }

    fn diagnostic_summary(&self) -> String {
        format!(
            "parsed_chunks={} ignored_frames={} done_frames={} samples=[{}]",
            self.parsed_chunks,
            self.ignored_frames,
            self.done_frames,
            self.ignored_samples.join(" | ")
        )
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct StreamState {
    model: String,
    message_started: bool,
    /// Upstream reasoning (thinking) stream. Sits at index 0 when present,
    /// pushing text to index 1 and tool calls to index `openai_index + 2`
    /// so the event sequence matches Anthropic's Messages streaming shape.
    thinking_started: bool,
    thinking_finished: bool,
    text_started: bool,
    text_finished: bool,
    finished: bool,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    tool_calls: BTreeMap<u32, ToolCallState>,
}

impl StreamState {
    fn new(model: String) -> Self {
        Self {
            model,
            message_started: false,
            thinking_started: false,
            thinking_finished: false,
            text_started: false,
            text_finished: false,
            finished: false,
            stop_reason: None,
            usage: None,
            tool_calls: BTreeMap::new(),
        }
    }

    /// Index at which the text block should live. When the model streamed
    /// reasoning first, text shifts from 0 to 1.
    const fn text_block_index(&self) -> u32 {
        if self.thinking_started {
            1
        } else {
            0
        }
    }

    /// Offset applied to each tool-call's `openai_index` to produce the
    /// Anthropic-style block index. One slot for the text block; if
    /// thinking is present, one more slot for that.
    const fn tool_call_block_index_offset(&self) -> u32 {
        if self.thinking_started {
            2
        } else {
            1
        }
    }

    #[allow(clippy::too_many_lines)]
    fn ingest_chunk(&mut self, chunk: ChatCompletionChunk) -> Result<Vec<StreamEvent>, ApiError> {
        let mut events = Vec::new();
        if !self.message_started {
            self.message_started = true;
            events.push(StreamEvent::MessageStart(MessageStartEvent {
                message: MessageResponse {
                    id: chunk.id.clone(),
                    kind: "message".to_string(),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    model: chunk.model.clone().unwrap_or_else(|| self.model.clone()),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage {
                        input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        output_tokens: 0,
                    },
                    request_id: None,
                    provider_metadata: None,
                },
            }));
        }

        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens: usage.completion_tokens,
            });
        }

        for choice in chunk.choices {
            // Reasoning/thinking content first, so downstream consumers
            // see ThinkingDelta events before TextDelta ones. DeepSeek
            // and Zhipu GLM use `reasoning_content`; OpenRouter uses
            // `reasoning` — we accept both and merge them in case a
            // provider ever sends both in the same chunk.
            let thinking_delta = {
                let mut combined = String::new();
                if let Some(r) = choice.delta.reasoning_content.filter(|s| !s.is_empty()) {
                    combined.push_str(&r);
                }
                if let Some(r) = choice.delta.reasoning.filter(|s| !s.is_empty()) {
                    combined.push_str(&r);
                }
                combined
            };
            if !thinking_delta.is_empty() && !self.thinking_finished {
                if !self.thinking_started {
                    self.thinking_started = true;
                    events.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index: 0,
                        content_block: OutputContentBlock::Thinking {
                            thinking: String::new(),
                            signature: None,
                        },
                    }));
                }
                events.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: 0,
                    delta: ContentBlockDelta::ThinkingDelta {
                        thinking: thinking_delta,
                    },
                }));
            }

            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
                // Close the thinking block on the first text token. The
                // provider protocol doesn't emit an explicit transition
                // event; the appearance of `content` is the only signal.
                if self.thinking_started && !self.thinking_finished {
                    self.thinking_finished = true;
                    events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index: 0,
                    }));
                }
                let text_idx = self.text_block_index();
                if !self.text_started {
                    self.text_started = true;
                    events.push(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index: text_idx,
                        content_block: OutputContentBlock::Text {
                            text: String::new(),
                        },
                    }));
                }
                events.push(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    index: text_idx,
                    delta: ContentBlockDelta::TextDelta { text: content },
                }));
            }

            for tool_call in choice.delta.tool_calls {
                // Tool calls can follow either thinking+text, text only,
                // or thinking only (rare but valid). Close any open
                // reasoning block before we start a tool call, mirroring
                // Anthropic's protocol.
                if self.thinking_started && !self.thinking_finished {
                    self.thinking_finished = true;
                    events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index: 0,
                    }));
                }
                let offset = self.tool_call_block_index_offset();
                let state = self.tool_calls.entry(tool_call.index).or_default();
                state.index_offset = offset;
                state.apply(tool_call);
                let block_index = state.block_index();
                if !state.started {
                    if let Some(start_event) = state.start_event()? {
                        state.started = true;
                        events.push(StreamEvent::ContentBlockStart(start_event));
                    } else {
                        continue;
                    }
                }
                if let Some(delta_event) = state.delta_event() {
                    events.push(StreamEvent::ContentBlockDelta(delta_event));
                }
                if choice.finish_reason.as_deref() == Some("tool_calls") && !state.stopped {
                    state.stopped = true;
                    events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index: block_index,
                    }));
                }
            }

            if let Some(finish_reason) = choice.finish_reason {
                self.stop_reason = Some(normalize_finish_reason(&finish_reason));
                if finish_reason == "tool_calls" {
                    for state in self.tool_calls.values_mut() {
                        if state.started && !state.stopped {
                            state.stopped = true;
                            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                                index: state.block_index(),
                            }));
                        }
                    }
                }
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, ApiError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;

        let mut events = Vec::new();
        // Close an open thinking block first. Some providers end the
        // stream with `finish_reason=stop` right after reasoning without
        // emitting any text (e.g. when the model's answer happens to be
        // identical to its reasoning); the consumer still needs a
        // matching Stop event.
        if self.thinking_started && !self.thinking_finished {
            self.thinking_finished = true;
            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                index: 0,
            }));
        }
        if self.text_started && !self.text_finished {
            self.text_finished = true;
            events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                index: self.text_block_index(),
            }));
        }

        for state in self.tool_calls.values_mut() {
            if !state.started {
                // Propagate the final offset in case reasoning happened
                // after the tool-call chunk arrived (rare, but possible
                // when a provider interleaves blocks).
                state.index_offset = if self.thinking_started { 2 } else { 1 };
                if let Some(start_event) = state.start_event()? {
                    state.started = true;
                    events.push(StreamEvent::ContentBlockStart(start_event));
                    if let Some(delta_event) = state.delta_event() {
                        events.push(StreamEvent::ContentBlockDelta(delta_event));
                    }
                }
            }
            if state.started && !state.stopped {
                state.stopped = true;
                events.push(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                    index: state.block_index(),
                }));
            }
        }

        if self.message_started {
            events.push(StreamEvent::MessageDelta(MessageDeltaEvent {
                delta: MessageDelta {
                    stop_reason: Some(
                        self.stop_reason
                            .clone()
                            .unwrap_or_else(|| "end_turn".to_string()),
                    ),
                    stop_sequence: None,
                },
                usage: self.usage.clone().unwrap_or(Usage {
                    input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    output_tokens: 0,
                }),
            }));
            events.push(StreamEvent::MessageStop(MessageStopEvent {}));
        }
        Ok(events)
    }
}

#[derive(Debug, Default)]
struct ToolCallState {
    openai_index: u32,
    /// Base offset that `openai_index` is added to before emission. The
    /// ingest loop sets this so thinking-first streams shift tool calls
    /// from index `n+1` to `n+2`, keeping the block-index space contiguous.
    index_offset: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_len: usize,
    started: bool,
    stopped: bool,
}

impl ToolCallState {
    fn apply(&mut self, tool_call: DeltaToolCall) {
        self.openai_index = tool_call.index;
        if let Some(id) = tool_call.id {
            self.id = Some(id);
        }
        if let Some(name) = tool_call.function.name {
            self.name = Some(name);
        }
        if let Some(arguments) = tool_call.function.arguments {
            self.arguments.push_str(&arguments);
        }
    }

    const fn block_index(&self) -> u32 {
        // Default offset of 1 preserves the old "text at 0, tool calls
        // at openai_index + 1" layout when thinking isn't in play.
        let offset = if self.index_offset == 0 {
            1
        } else {
            self.index_offset
        };
        self.openai_index + offset
    }

    #[allow(clippy::unnecessary_wraps)]
    fn start_event(&self) -> Result<Option<ContentBlockStartEvent>, ApiError> {
        let Some(name) = self.name.clone() else {
            return Ok(None);
        };
        let id = self
            .id
            .clone()
            .unwrap_or_else(|| format!("tool_call_{}", self.openai_index));
        Ok(Some(ContentBlockStartEvent {
            index: self.block_index(),
            content_block: OutputContentBlock::ToolUse {
                id,
                name,
                input: json!({}),
            },
        }))
    }

    fn delta_event(&mut self) -> Option<ContentBlockDeltaEvent> {
        if self.emitted_len >= self.arguments.len() {
            return None;
        }
        let delta = self.arguments[self.emitted_len..].to_string();
        self.emitted_len = self.arguments.len();
        Some(ContentBlockDeltaEvent {
            index: self.block_index(),
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: delta,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    id: String,
    model: String,
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    /// See [`ChunkDelta::reasoning_content`]. Same convention, non-stream variant.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// See [`ChunkDelta::reasoning`]. `OpenRouter`'s spelling.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseToolFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    /// `reasoning_content` is the convention used by `DeepSeek`, `Zhipu GLM`,
    /// and `Alibaba DashScope` (`QwQ` / `Qwen3-Thinking`) for the chain-of-thought
    /// stream. It is streamed as a plain string delta alongside `content`,
    /// and MUST be surfaced as `ThinkingDelta` so the web UI can render it
    /// in the collapsible reasoning panel.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// `reasoning` is `OpenRouter`'s equivalent field name. We accept both so
    /// one parser covers every OpenAI-compat reasoning provider we've seen.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: DeltaFunction,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct TopLevelErrorEnvelope {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: Option<String>,
}

/// Returns true for models known to reject tuning parameters like temperature,
/// `top_p`, `frequency_penalty`, and `presence_penalty`. These are typically
/// reasoning/chain-of-thought models with fixed sampling.
fn is_reasoning_model(model: &str) -> bool {
    let lowered = model.to_ascii_lowercase();
    // Strip any provider/ prefix for the check (e.g. qwen/qwen-qwq -> qwen-qwq)
    let canonical = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    // OpenAI reasoning models
    canonical.starts_with("o1")
        || canonical.starts_with("o3")
        || canonical.starts_with("o4")
        // xAI reasoning: grok-3-mini always uses reasoning mode
        || canonical == "grok-3-mini"
        // Alibaba DashScope reasoning variants (QwQ + Qwen3-Thinking family)
        || canonical.starts_with("qwen-qwq")
        || canonical.starts_with("qwq")
        || canonical.contains("thinking")
        // DeepSeek reasoning family — `deepseek-reasoner` (API) and
        // `deepseek-r1` (OpenRouter / self-hosted).
        || canonical.starts_with("deepseek-r1")
        || canonical.starts_with("deepseek-reasoner")
        // Zhipu GLM reasoning family. GLM-4.5/GLM-5 support an internal
        // chain-of-thought exposed as `reasoning_content`.
        || canonical.starts_with("glm-4.5")
        || canonical.starts_with("glm-4-5")
        || canonical.starts_with("glm-5")
}

/// Returns true when the upstream endpoint is `OpenRouter`.
///
/// `OpenRouter` only emits reasoning deltas when the request body includes
/// `"reasoning": { "exclude": false }` (or the legacy `"include_reasoning": true`).
/// We detect it by base URL rather than provider name because users can
/// point the `OpenAI`-compat client at `OpenRouter` via `BYOK` `base_url`.
fn is_openrouter_base_url(base_url: &str) -> bool {
    base_url.contains("openrouter.ai")
}

/// Returns true for OpenAI-compatible `DeepSeek` V4 models that require prior
/// assistant reasoning to be echoed back as `reasoning_content` in history.
///
/// Most OpenAI-compatible providers reject an assistant turn that carries a
/// `reasoning_content` field they did not themselves emit (HTTP 400). Only the
/// models matched here actually require the reasoning to be replayed, so we gate
/// the replay on the model id to avoid breaking multi-turn conversations on
/// every other provider.
#[must_use]
pub fn model_requires_reasoning_content_in_history(model: &str) -> bool {
    let lowered = model.to_ascii_lowercase();
    let canonical = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    canonical.starts_with("deepseek-v4")
}

fn official_deepseek_v4_thinking_is_enabled(request: &MessageRequest, base_url: &str) -> bool {
    let canonical_model = request
        .model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !canonical_model.starts_with("deepseek-v4") || !is_official_deepseek_base_url(base_url) {
        return false;
    }
    !request
        .extra_body
        .as_ref()
        .and_then(|body| body.get("thinking"))
        .and_then(Value::as_object)
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("disabled"))
}

/// Strip routing prefix (e.g., "openai/gpt-4" → "gpt-4") for the wire.
/// The prefix is used only to select transport; the backend expects the
/// bare model id.
fn strip_routing_prefix(model: &str) -> &str {
    if let Some(pos) = model.find('/') {
        let prefix = &model[..pos];
        // Only strip if the prefix before "/" is a known routing prefix,
        // not if "/" appears in the middle of the model name for other reasons.
        if matches!(prefix, "openai" | "xai" | "grok" | "qwen") {
            &model[pos + 1..]
        } else {
            model
        }
    } else {
        model
    }
}

fn build_chat_completion_request(
    request: &MessageRequest,
    config: OpenAiCompatConfig,
    base_url: &str,
) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = request.system.as_ref().filter(|value| !value.is_empty()) {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }
    for message in &request.messages {
        messages.extend(translate_message(message, &request.model));
    }
    // Sanitize: drop any `role:"tool"` message that does not have a valid
    // paired `role:"assistant"` with a `tool_calls` entry carrying the same
    // `id` immediately before it (directly or as part of a run of tool
    // results). OpenAI-compatible backends return 400 for orphaned tool
    // messages regardless of how they were produced (compaction, session
    // editing, resume, etc.). We drop rather than error so the request can
    // still proceed with the remaining history intact.
    messages = sanitize_tool_message_pairing(messages);

    // Strip routing prefix (e.g., "openai/gpt-4" → "gpt-4") for the wire.
    let wire_model = strip_routing_prefix(&request.model);

    // Some OpenAI-compatible endpoints require `max_completion_tokens` instead
    // of `max_tokens`. This is controlled by model/key capabilities rather than
    // model-name guesses so newly released models can opt in via configuration.
    let max_tokens_key = if request.use_max_completion_tokens == Some(true) {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };

    let mut payload = json!({
        "model": wire_model,
        max_tokens_key: request.max_tokens,
        "messages": messages,
        "stream": request.stream,
    });

    if request.stream && should_request_stream_usage(config, base_url) {
        payload["stream_options"] = json!({ "include_usage": true });
    }

    if let Some(tools) = &request.tools {
        payload["tools"] =
            Value::Array(tools.iter().map(openai_tool_definition).collect::<Vec<_>>());
    }
    let omit_tool_choice = official_deepseek_v4_thinking_is_enabled(request, base_url);
    if let Some(tool_choice) = &request.tool_choice {
        if !omit_tool_choice {
            payload["tool_choice"] = openai_tool_choice(tool_choice);
        }
    }

    // OpenAI-compatible tuning parameters — only included when explicitly set.
    // Reasoning models (o1/o3/o4/grok-3-mini) reject these params with 400;
    // silently strip them to avoid cryptic provider errors.
    if !is_reasoning_model(&request.model) {
        if let Some(temperature) = request.temperature {
            payload["temperature"] = json!(temperature);
        }
        if let Some(top_p) = request.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(frequency_penalty) = request.frequency_penalty {
            payload["frequency_penalty"] = json!(frequency_penalty);
        }
        if let Some(presence_penalty) = request.presence_penalty {
            payload["presence_penalty"] = json!(presence_penalty);
        }
    }
    // stop is generally safe for all providers
    if let Some(stop) = &request.stop {
        if !stop.is_empty() {
            payload["stop"] = json!(stop);
        }
    }
    // reasoning_effort for OpenAI-compatible reasoning models (o4-mini, o3, etc.)
    if let Some(effort) = &request.reasoning_effort {
        payload["reasoning_effort"] = json!(effort);
    }

    // OpenRouter opt-in for reasoning deltas. Without one of these fields
    // the provider may not stream `reasoning` at all, so the web UI's
    // thinking panel stays empty for DeepSeek-R1 / GLM-5 / QwQ hosted
    // behind OpenRouter. We send both the current (`reasoning: { exclude:
    // false }`) and legacy (`include_reasoning`) spellings so the request
    // works across OpenRouter's rolling deprecation of either flag.
    //
    // Only emitted when (a) the provider is OpenRouter and (b) the runtime has
    // positively opted into reasoning deltas. Capability config is preferred;
    // the model allowlist remains only as conservative backwards compatibility
    // for existing unconfigured keys.
    if is_openrouter_base_url(base_url)
        && (request.include_reasoning == Some(true)
            || (request.include_reasoning.is_none() && is_reasoning_model(&request.model)))
    {
        payload["reasoning"] = json!({ "exclude": false });
        payload["include_reasoning"] = json!(true);
    }

    if let Some(extra_body) = &request.extra_body {
        if let Some(payload_object) = payload.as_object_mut() {
            for (key, value) in extra_body {
                match key.as_str() {
                    "__aos_append_tools" => {
                        let Some(items) = value.as_array() else {
                            continue;
                        };
                        let tools_entry = payload_object
                            .entry("tools".to_string())
                            .or_insert_with(|| Value::Array(Vec::new()));
                        if let Some(tools) = tools_entry.as_array_mut() {
                            tools.extend(items.iter().cloned());
                        }
                    }
                    "__aos_tool_choice_auto" => {
                        if value.as_bool().unwrap_or(false)
                            && !omit_tool_choice
                            && !payload_object.contains_key("tool_choice")
                        {
                            payload_object.insert("tool_choice".to_string(), json!("auto"));
                        }
                    }
                    "__aos_responses_web_search_options_explicit" => {}
                    _ => {
                        if is_reserved_extra_body_key(key) {
                            continue;
                        }
                        payload_object.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    payload
}

fn translate_message(message: &InputMessage, model: &str) -> Vec<Value> {
    if message.role.as_str() == "assistant" {
        translate_assistant_message(message, model)
    } else {
        translate_non_assistant_message(message)
    }
}

fn translate_assistant_message(message: &InputMessage, model: &str) -> Vec<Value> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in &message.content {
        match block {
            InputContentBlock::Text { text: value } => text.push_str(value),
            // Replay reasoning back to the provider only for models that require
            // it. DeepSeek V4 rejects a follow-up turn (400) if the prior
            // `reasoning_content` is dropped, but most other providers reject a
            // turn that carries a `reasoning_content` they did not emit, so the
            // replay is gated on the model id below.
            InputContentBlock::Thinking { thinking, .. } => reasoning.push_str(thinking),
            InputContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": input.to_string(),
                }
            })),
            InputContentBlock::ToolResult { .. }
            | InputContentBlock::Image { .. }
            | InputContentBlock::Document { .. } => {}
        }
    }
    let needs_reasoning = model_requires_reasoning_content_in_history(model);
    if text.is_empty() && tool_calls.is_empty() && !(needs_reasoning && !reasoning.is_empty()) {
        Vec::new()
    } else {
        let mut msg = serde_json::json!({
            "role": "assistant",
            "content": (!text.is_empty()).then_some(text),
        });
        if needs_reasoning && !reasoning.is_empty() {
            msg["reasoning_content"] = json!(reasoning);
        }
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
        }
        vec![msg]
    }
}

#[allow(clippy::too_many_lines)]
fn translate_non_assistant_message(message: &InputMessage) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let mut text_parts: Vec<&str> = Vec::new();
    let mut has_media = false;

    for block in &message.content {
        match block {
            InputContentBlock::Text { text } => text_parts.push(text),
            InputContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                parts.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": flatten_tool_result_content(content),
                    "is_error": is_error,
                }));
            }
            InputContentBlock::Image {
                media_type,
                source_type,
                data,
            } => {
                if let Some(text) = (!text_parts.join("").is_empty()).then(|| text_parts.join("")) {
                    parts.push(json!({
                        "role": message.role,
                        "content": text,
                    }));
                    text_parts.clear();
                }
                let url = match source_type {
                    crate::types::ImageSourceType::Url => data.clone(),
                    crate::types::ImageSourceType::Base64 => {
                        format!("data:{media_type};base64,{data}")
                    }
                };
                parts.push(json!({
                    "role": message.role,
                    "content": [{
                        "type": "image_url",
                        "image_url": { "url": url }
                    }]
                }));
                has_media = true;
            }
            InputContentBlock::Document {
                media_type,
                source_type,
                data,
                name,
            } => {
                if !text_parts.is_empty() {
                    parts.push(json!({
                        "role": message.role,
                        "content": text_parts.join(""),
                    }));
                    text_parts.clear();
                }
                let _url = match source_type {
                    crate::types::ImageSourceType::Url => data.clone(),
                    crate::types::ImageSourceType::Base64 => {
                        format!("data:{media_type};base64,{data}")
                    }
                };
                let doc_part = if let Some(n) = name {
                    json!({
                        "role": message.role,
                        "content": [{
                            "type": "file",
                            "file": {
                                "filename": n,
                                "media_type": media_type,
                                "data": data,
                            }
                        }]
                    })
                } else {
                    json!({
                        "role": message.role,
                        "content": [{
                            "type": "file",
                            "file": {
                                "media_type": media_type,
                                "data": data,
                            }
                        }]
                    })
                };
                parts.push(doc_part);
                has_media = true;
            }
            InputContentBlock::ToolUse { .. } => {}
            // Thinking blocks only appear on assistant messages; ignore on
            // non-assistant roles.
            InputContentBlock::Thinking { .. } => {}
        }
    }

    if !text_parts.is_empty() {
        let text = text_parts.join("");
        if has_media {
            parts.push(json!({
                "role": message.role,
                "content": text,
            }));
        } else if parts.is_empty() {
            return vec![json!({
                "role": message.role,
                "content": text,
            })];
        } else {
            let mut result = vec![json!({
                "role": message.role,
                "content": text,
            })];
            result.extend(parts);
            return result;
        }
    }

    if parts.is_empty() {
        Vec::new()
    } else {
        parts
    }
}

/// Sanitize tool-call pairing invariants for OpenAI-compatible transcripts.
///
/// Invariants:
/// 1) A `role:"tool"` message must be paired with the nearest preceding
///    `role:"assistant"` that declares a matching `tool_calls[].id`.
/// 2) An assistant message that declares `tool_calls` must have matching tool
///    outputs immediately following it (before the next non-tool turn).
///
/// We treat this as a last-resort recovery layer: malformed tool transcripts
/// can be produced by crashes, partial persistence, manual edits, or provider
/// translation edge cases.
fn sanitize_tool_message_pairing(messages: Vec<Value>) -> Vec<Value> {
    // Collect indices that should be dropped from the request payload.
    let mut drop_indices = std::collections::HashSet::new();

    // Pass 1: remove assistant tool-call turns that are missing one or more
    // matching tool outputs in the immediately following tool segment.
    let mut i = 0usize;
    while i < messages.len() {
        let msg = &messages[i];
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "assistant" {
            i += 1;
            continue;
        }
        let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) else {
            i += 1;
            continue;
        };
        if tool_calls.is_empty() {
            i += 1;
            continue;
        }

        let expected_ids: std::collections::HashSet<String> = tool_calls
            .iter()
            .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        if expected_ids.is_empty() {
            i += 1;
            continue;
        }

        let mut j = i + 1;
        let mut seen_ids = std::collections::HashSet::new();
        while j < messages.len() && messages[j].get("role").and_then(|v| v.as_str()) == Some("tool")
        {
            if let Some(id) = messages[j].get("tool_call_id").and_then(|v| v.as_str()) {
                seen_ids.insert(id.to_string());
            }
            j += 1;
        }

        let missing_any = expected_ids.iter().any(|id| !seen_ids.contains(id));
        if missing_any {
            drop_indices.insert(i);
            for k in (i + 1)..j {
                drop_indices.insert(k);
            }
        }
        i = j.max(i + 1);
    }

    // Pass 2: remove orphaned tool messages (tool_call_id does not match the
    // nearest preceding assistant tool_calls declaration).
    for (i, msg) in messages.iter().enumerate() {
        if msg.get("role").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Find the nearest preceding non-tool message.
        let preceding = messages[..i]
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|v| v.as_str()) != Some("tool"));
        let preceding_role = preceding
            .and_then(|m| m.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if preceding_role != "assistant" {
            drop_indices.insert(i);
            continue;
        }
        let paired = preceding
            .and_then(|m| m.get("tool_calls").and_then(|tc| tc.as_array()))
            .is_some_and(|tool_calls| {
                tool_calls
                    .iter()
                    .any(|tc| tc.get("id").and_then(|v| v.as_str()) == Some(tool_call_id))
            });
        if !paired {
            drop_indices.insert(i);
        }
    }
    if drop_indices.is_empty() {
        return messages;
    }
    messages
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !drop_indices.contains(i))
        .map(|(_, m)| m)
        .collect()
}

fn flatten_tool_result_content(content: &[ToolResultContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text { text } => text.clone(),
            ToolResultContentBlock::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recursively ensure every object-type node in a JSON Schema has
/// `"properties"` (at least `{}`) and `"additionalProperties": false`.
/// The `OpenAI` `/responses` endpoint validates schemas strictly and rejects
/// objects that omit these fields; `/chat/completions` is lenient but also
/// accepts them, so we normalise unconditionally.
fn normalize_object_schema(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if obj.get("type").and_then(Value::as_str) == Some("object") {
            obj.entry("properties").or_insert_with(|| json!({}));
            obj.entry("additionalProperties")
                .or_insert(Value::Bool(false));
        }
        // Recurse into properties values
        if let Some(props) = obj.get_mut("properties") {
            if let Some(props_obj) = props.as_object_mut() {
                let keys: Vec<String> = props_obj.keys().cloned().collect();
                for k in keys {
                    if let Some(v) = props_obj.get_mut(&k) {
                        normalize_object_schema(v);
                    }
                }
            }
        }
        // Recurse into items (arrays)
        if let Some(items) = obj.get_mut("items") {
            normalize_object_schema(items);
        }
    }
}

fn openai_tool_definition(tool: &ToolDefinition) -> Value {
    let mut parameters = tool.input_schema.clone();
    normalize_object_schema(&mut parameters);
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        }
    })
}

fn responses_tool_definition(tool: &ToolDefinition) -> Value {
    let mut parameters = tool.input_schema.clone();
    normalize_object_schema(&mut parameters);
    let mut definition = json!({
        "type": "function",
        "name": tool.name,
        "parameters": parameters,
        "strict": false,
    });
    if let Some(description) = tool.description.as_deref() {
        definition["description"] = Value::String(description.to_string());
    }
    definition
}

fn openai_tool_choice(tool_choice: &ToolChoice) -> Value {
    match tool_choice {
        ToolChoice::Auto => Value::String("auto".to_string()),
        ToolChoice::Any => Value::String("required".to_string()),
        ToolChoice::Tool { name } => json!({
            "type": "function",
            "function": { "name": name },
        }),
    }
}

fn should_request_stream_usage(config: OpenAiCompatConfig, base_url: &str) -> bool {
    let normalized = base_url.to_ascii_lowercase();
    match config.provider_name {
        // Many OpenAI-compatible proxies accept basic ChatCompletions streaming
        // but mishandle `stream_options.include_usage` by returning an empty
        // `[DONE]`-only stream. Only send this opt-in to official endpoints
        // where the field is part of the documented contract.
        "OpenAI" => normalized.contains("api.openai.com"),
        "xAI" => normalized.contains("api.x.ai"),
        "DashScope" => normalized.contains("dashscope.aliyuncs.com"),
        _ => false,
    }
}

fn normalize_response(
    model: &str,
    response: ChatCompletionResponse,
) -> Result<MessageResponse, ApiError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or(ApiError::InvalidSseFrame(
            "chat completion response missing choices",
        ))?;
    let mut content = Vec::new();
    // Surface reasoning content (`reasoning_content` from DeepSeek /
    // Zhipu GLM / Alibaba, `reasoning` from OpenRouter) as a dedicated
    // Thinking block so the web UI can render it in its collapsible
    // reasoning panel, mirroring Anthropic's native API shape. We accept
    // both spellings and concatenate when a provider unexpectedly sends
    // both in the same response.
    let reasoning_text = {
        let mut combined = String::new();
        if let Some(r) = choice
            .message
            .reasoning_content
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            combined.push_str(r);
        }
        if let Some(r) = choice
            .message
            .reasoning
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            combined.push_str(r);
        }
        combined
    };
    if !reasoning_text.is_empty() {
        content.push(OutputContentBlock::Thinking {
            thinking: reasoning_text,
            signature: None,
        });
    }
    if let Some(text) = choice.message.content.filter(|value| !value.is_empty()) {
        content.push(OutputContentBlock::Text { text });
    }
    for tool_call in choice.message.tool_calls {
        content.push(OutputContentBlock::ToolUse {
            id: tool_call.id,
            name: tool_call.function.name,
            input: parse_tool_arguments(&tool_call.function.arguments),
        });
    }

    Ok(MessageResponse {
        id: response.id,
        kind: "message".to_string(),
        role: choice.message.role,
        content,
        model: response.model.if_empty_then(model.to_string()),
        stop_reason: choice
            .finish_reason
            .map(|value| normalize_finish_reason(&value)),
        stop_sequence: None,
        usage: Usage {
            input_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.prompt_tokens),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: response
                .usage
                .as_ref()
                .map_or(0, |usage| usage.completion_tokens),
        },
        request_id: None,
        provider_metadata: None,
    })
}

fn response_exhausted_in_reasoning(response: &MessageResponse) -> bool {
    let stopped_for_length = response
        .stop_reason
        .as_deref()
        .is_some_and(|reason| matches!(reason, "length" | "max_tokens" | "max_output_tokens"));
    if !stopped_for_length {
        return false;
    }
    let has_reasoning = response.content.iter().any(|block| {
        matches!(
            block,
            OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. }
        )
    });
    let has_visible_output = response.content.iter().any(|block| {
        matches!(
            block,
            OutputContentBlock::Text { .. } | OutputContentBlock::ToolUse { .. }
        )
    });
    has_reasoning && !has_visible_output
}

fn build_responses_web_search_request(request: &MessageRequest) -> Value {
    let web_search_tool = json!({
        "type": "web_search",
        "external_web_access": true,
    });

    let mut tools = vec![web_search_tool];
    tools.extend(
        request
            .tools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(responses_tool_definition),
    );

    let mut payload = json!({
        "model": strip_routing_prefix(&request.model),
        "input": responses_web_search_plain_input(request),
        "tools": tools,
        "tool_choice": "auto",
        "max_output_tokens": responses_web_search_max_output_tokens(request.max_tokens),
    });

    if let Some(extra_body) = &request.extra_body {
        if has_explicit_responses_web_search_options(extra_body) {
            if let Some(options) = extra_body.get("web_search_options") {
                if let Some(tool) = payload
                    .get_mut("tools")
                    .and_then(Value::as_array_mut)
                    .and_then(|tools| tools.first_mut())
                {
                    apply_responses_web_search_options_to_tool(tool, options);
                }
            }
        }
    }

    payload
}

fn adapt_responses_web_search_request_for_provider(
    payload: &mut Value,
    model: &str,
    base_url: &str,
) {
    if !supports_official_deepseek_responses_web_search(model, base_url) {
        return;
    }
    if let Some(tool) = payload
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.first_mut())
        .and_then(Value::as_object_mut)
    {
        // DeepSeek's Responses schema currently documents only `type` for its
        // server-side web_search tool. Keep OpenAI-only extensions off the
        // official DeepSeek request so native search cannot fail validation.
        tool.remove("external_web_access");
    }
}

fn apply_responses_web_search_options_to_tool(tool: &mut Value, options: &Value) {
    let Some(options) = options.as_object() else {
        return;
    };
    if let Some(search_context_size) = options.get("search_context_size") {
        tool["search_context_size"] = search_context_size.clone();
    }
    if let Some(user_location) = options.get("user_location") {
        tool["user_location"] = user_location.clone();
    }
    if let Some(filters) = options.get("filters") {
        tool["filters"] = filters.clone();
    }
    if let Some(search_content_types) = options.get("search_content_types") {
        tool["search_content_types"] = search_content_types.clone();
    }
    if let Some(external_web_access) = options.get("external_web_access") {
        tool["external_web_access"] = external_web_access.clone();
    }
    if let Some(index_gated_web_access) = options.get("index_gated_web_access") {
        tool["index_gated_web_access"] = index_gated_web_access.clone();
    }
}

fn responses_web_search_max_output_tokens(max_tokens: u32) -> u32 {
    max_tokens.max(1)
}

fn has_explicit_responses_web_search_options(extra_body: &Map<String, Value>) -> bool {
    extra_body.contains_key("web_search_options")
}

fn responses_web_search_plain_input(request: &MessageRequest) -> String {
    let mut sections = Vec::new();
    let latest_user_index = request
        .messages
        .iter()
        .rposition(|message| message.role == "user");
    let latest_user = latest_user_index
        .and_then(|index| message_text_for_responses_web_search(&request.messages[index]));

    sections.push(
        "Search brief:\nUse the provider-native web_search tool as an iterative research tool, not as a single generic lookup. Preserve explicit entities, metrics, time ranges, locations, product names, and constraints from the context below. Do not let older context override the current user request.\n\nSearch quality protocol:\n- Start with the current user request, then reformulate the query if the first results are broad, generic, stale, or off-topic.\n- Prefer directly relevant source-backed pages over generic homepages, SEO pages, policy pages, or unrelated academic/project pages.\n- Open or inspect promising results when the tool supports it, and discard sources whose page content does not actually answer the request.\n- For research-style requests, seek multiple distinct relevant sources before answering; for simple live facts, keep the search focused and concise.\n- Return only useful source-backed facts with URLs; if live search is unavailable, say so plainly."
            .to_string(),
    );

    if let Some(latest_user) = latest_user.as_deref() {
        sections.push(format!(
            "Current user request:\n{}",
            truncate_chars(latest_user, RESPONSES_WEB_SEARCH_CURRENT_REQUEST_CHAR_LIMIT)
        ));
    }

    if let Some(system) = request
        .system
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        sections.push(format!(
            "Relevant system/developer constraints:\n{}",
            truncate_chars(system, RESPONSES_WEB_SEARCH_SYSTEM_CHAR_LIMIT)
        ));
    }

    if let Some(relevant_context) = relevant_history_for_responses_web_search(
        &request.messages,
        latest_user_index,
        latest_user.as_deref().unwrap_or(""),
    ) {
        sections.push(format!("Relevant prior context:\n{relevant_context}"));
    }

    if let Some(recent_context) =
        recent_history_for_responses_web_search(&request.messages, latest_user_index)
    {
        sections.push(format!("Recent conversation tail:\n{recent_context}"));
    }

    if let Some(query_hints) = search_query_hints(latest_user.as_deref().unwrap_or("")) {
        sections.push(format!("Likely search focus:\n{query_hints}"));
    }

    let input = sections.join("\n\n");
    if input.trim().is_empty() {
        "Search the web and answer with concise, source-backed facts.".to_string()
    } else {
        truncate_chars(&input, RESPONSES_WEB_SEARCH_BRIEF_CHAR_LIMIT)
    }
}

fn message_text_for_responses_web_search(message: &InputMessage) -> Option<String> {
    let mut parts = Vec::new();
    for block in &message.content {
        match block {
            InputContentBlock::Text { text } if !text.trim().is_empty() => {
                parts.push(text.trim().to_string());
            }
            InputContentBlock::Image { .. } => {
                parts.push("[image omitted for web search request]".to_string());
            }
            InputContentBlock::Document { name, .. } => {
                parts.push(format!(
                    "[document omitted for web search request: {}]",
                    name.as_deref().unwrap_or("unnamed")
                ));
            }
            InputContentBlock::ToolResult { content, .. } => {
                let text = flatten_tool_result_content(content);
                if !text.trim().is_empty() {
                    parts.push(format!(
                        "[tool result summary]\n{}",
                        truncate_chars(text.trim(), RESPONSES_WEB_SEARCH_TOOL_RESULT_CHAR_LIMIT)
                    ));
                }
            }
            InputContentBlock::ToolUse { name, input, .. } => {
                parts.push(format!(
                    "[previous tool call omitted: {name} {}]",
                    truncate_chars(
                        &input.to_string(),
                        RESPONSES_WEB_SEARCH_TOOL_CALL_CHAR_LIMIT
                    )
                ));
            }
            InputContentBlock::Thinking { .. } | InputContentBlock::Text { .. } => {}
        }
    }
    (!parts.is_empty())
        .then(|| truncate_chars(&parts.join("\n"), RESPONSES_WEB_SEARCH_MESSAGE_CHAR_LIMIT))
}

fn recent_history_for_responses_web_search(
    messages: &[InputMessage],
    latest_user_index: Option<usize>,
) -> Option<String> {
    let Some(latest_index) = latest_user_index else {
        return None;
    };
    let mut selected = Vec::new();
    for (index, message) in messages
        .iter()
        .enumerate()
        .take(latest_index)
        .rev()
        .take(RESPONSES_WEB_SEARCH_MAX_RECENT_MESSAGES)
    {
        if let Some(text) = message_text_for_responses_web_search(message) {
            selected.push(format!("- [{} #{index}] {text}", message.role));
        }
    }
    selected.reverse();
    let joined = selected.join("\n");
    (!joined.trim().is_empty())
        .then(|| truncate_chars(&joined, RESPONSES_WEB_SEARCH_RECENT_CONTEXT_CHAR_LIMIT))
}

fn relevant_history_for_responses_web_search(
    messages: &[InputMessage],
    latest_user_index: Option<usize>,
    latest_user: &str,
) -> Option<String> {
    let latest_index = latest_user_index?;
    let query_terms = salient_terms_for_responses_web_search(latest_user);
    if query_terms.is_empty() {
        return None;
    }

    let mut scored = Vec::new();
    for (index, message) in messages.iter().enumerate().take(latest_index) {
        let Some(text) = message_text_for_responses_web_search(message) else {
            continue;
        };
        let score = relevance_score_for_responses_web_search(&text, &query_terms);
        if score > 0 {
            scored.push((score, index, message.role.as_str(), text));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut selected = scored
        .into_iter()
        .take(RESPONSES_WEB_SEARCH_MAX_RELEVANT_MESSAGES)
        .map(|(score, index, role, text)| format!("- [{role} #{index}, relevance {score}] {text}"))
        .collect::<Vec<_>>();
    selected.reverse();
    let joined = selected.join("\n");
    (!joined.trim().is_empty())
        .then(|| truncate_chars(&joined, RESPONSES_WEB_SEARCH_RELEVANT_CONTEXT_CHAR_LIMIT))
}

fn salient_terms_for_responses_web_search(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    push_salient_terms_from_tokens(text.split(is_search_term_separator), &mut terms, 32);
    for token in ascii_runs_for_responses_web_search(text) {
        push_salient_term(&token, &mut terms);
        if terms.len() >= 32 {
            break;
        }
    }
    for token in cjk_bigrams_for_responses_web_search(text) {
        push_salient_term(&token, &mut terms);
        if terms.len() >= 32 {
            break;
        }
    }
    terms
}

fn is_search_term_separator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | '.'
                | ';'
                | ':'
                | '!'
                | '?'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '"'
                | '\''
                | '`'
                | '|'
                | '/'
                | '\\'
                | '，'
                | '。'
                | '；'
                | '：'
                | '！'
                | '？'
                | '（'
                | '）'
                | '【'
                | '】'
                | '、'
        )
}

fn push_salient_terms_from_tokens<'a>(
    tokens: impl Iterator<Item = &'a str>,
    terms: &mut Vec<String>,
    max_terms: usize,
) {
    for token in tokens {
        push_salient_term(token, terms);
        if terms.len() >= max_terms {
            break;
        }
    }
}

fn push_salient_term(token: &str, terms: &mut Vec<String>) {
    let cleaned = clean_search_term(token);
    let normalized = cleaned.to_ascii_lowercase();
    if normalized.is_empty() || terms.iter().any(|term| term == &normalized) {
        return;
    }
    if is_salient_search_term(&cleaned, &normalized) {
        terms.push(normalized);
    }
}

fn ascii_runs_for_responses_web_search(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+') {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

fn cjk_bigrams_for_responses_web_search(text: &str) -> Vec<String> {
    let chars = text
        .chars()
        .filter(|ch| is_cjk_char(*ch))
        .collect::<Vec<_>>();
    chars
        .windows(2)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF
    )
}

fn clean_search_term(token: &str) -> String {
    token
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-' && ch != '+')
        .to_string()
}

fn is_salient_search_term(original: &str, normalized: &str) -> bool {
    if normalized.chars().any(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if !original.is_ascii() {
        return original.chars().count() >= 2;
    }
    if original.chars().all(|ch| ch.is_ascii_uppercase()) && original.chars().count() >= 2 {
        return true;
    }
    if normalized.chars().count() >= 4 {
        return !matches!(
            normalized,
            "please"
                | "about"
                | "with"
                | "from"
                | "that"
                | "this"
                | "what"
                | "when"
                | "where"
                | "which"
                | "would"
                | "should"
                | "could"
                | "have"
                | "need"
                | "search"
                | "lookup"
                | "find"
                | "tell"
                | "give"
        );
    }
    false
}

fn relevance_score_for_responses_web_search(text: &str, terms: &[String]) -> usize {
    let normalized = text.to_ascii_lowercase();
    terms
        .iter()
        .filter(|term| normalized.contains(term.as_str()))
        .map(|term| {
            if term.chars().any(|ch| ch.is_ascii_digit()) {
                2
            } else {
                1
            }
        })
        .sum()
}

fn search_query_hints(latest_user: &str) -> Option<String> {
    let terms = salient_terms_for_responses_web_search(latest_user);
    if terms.is_empty() {
        return None;
    }
    Some(format!(
        "Prioritize these extracted terms when forming web queries: {}",
        terms.into_iter().take(16).collect::<Vec<_>>().join(", ")
    ))
}

fn log_responses_web_search_payload(
    mode: &'static str,
    base_url: &str,
    request: &MessageRequest,
    payload: &Value,
) {
    let input = payload.get("input").unwrap_or(&Value::Null);
    let input_shape = if input.is_string() {
        "string"
    } else if input.is_array() {
        "array"
    } else {
        "other"
    };
    let input_chars = match input {
        Value::String(value) => value.chars().count(),
        Value::Array(_) => input.to_string().chars().count(),
        other => other.to_string().chars().count(),
    };
    let input_item_count = input.as_array().map_or(0, Vec::len);
    let tool_types = payload
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    let kind = tool.get("type").and_then(Value::as_str)?;
                    let name = tool.get("name").and_then(Value::as_str);
                    Some(match name {
                        Some(name) => format!("{kind}:{name}"),
                        None => kind.to_string(),
                    })
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let tool_choice = payload
        .get("tool_choice")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let max_output_tokens = payload
        .get("max_output_tokens")
        .and_then(|value| value.as_u64())
        .unwrap_or_default();
    let stream = payload
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let web_search_options_present = payload.get("web_search_options").is_some();
    tracing::info!(
        requested_model = %request.model,
        wire_model = %strip_routing_prefix(&request.model),
        base_url = %base_url,
        mode = mode,
        endpoint = "/responses",
        input_shape = input_shape,
        input_item_count = input_item_count,
        input_chars = input_chars,
        tool_types = %tool_types,
        tool_choice = tool_choice,
        max_output_tokens = max_output_tokens,
        stream = stream,
        web_search_options_present = web_search_options_present,
        "OpenAI Responses web_search request payload summary"
    );
}

fn log_responses_web_search_error(
    mode: &'static str,
    base_url: &str,
    request: &MessageRequest,
    status: reqwest::StatusCode,
    request_id: Option<&str>,
    headers_summary: &str,
    body: &str,
) {
    tracing::warn!(
        requested_model = %request.model,
        wire_model = %strip_routing_prefix(&request.model),
        base_url = %base_url,
        mode = mode,
        endpoint = "/responses",
        status = status.as_u16(),
        request_id = request_id.unwrap_or(""),
        headers = %headers_summary,
        response_body_chars = body.chars().count(),
        "OpenAI Responses web_search request returned non-success status"
    );
}

fn responses_error_headers_summary(headers: &reqwest::header::HeaderMap) -> String {
    let mut pairs = Vec::new();
    for name in [
        "cf-ray",
        "server",
        "x-request-id",
        "openai-request-id",
        "retry-after",
    ] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            pairs.push(format!("{name}={}", truncate_chars(value, 180)));
        }
    }
    pairs.join(",")
}

#[derive(Debug, Deserialize)]
struct ResponsesApiResponse {
    id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Debug, Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesContentItem>,
    #[serde(default)]
    action: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContentItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    annotations: Vec<ResponsesContentAnnotation>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContentAnnotation {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    start_index: Option<u32>,
    #[serde(default)]
    end_index: Option<u32>,
}

fn responses_annotation_to_metadata(
    annotation: &ResponsesContentAnnotation,
    part_text: Option<&str>,
) -> Option<Value> {
    let url = annotation.url.as_deref()?.trim();
    if url.is_empty() {
        return None;
    }
    let cited_text = part_text
        .and_then(|text| {
            let start = annotation.start_index? as usize;
            let end = annotation.end_index? as usize;
            text.get(start..end).map(str::trim)
        })
        .filter(|text| !text.is_empty())
        .unwrap_or("");
    Some(json!({
        "type": annotation.kind,
        "url": url,
        "title": annotation.title.as_deref().unwrap_or("").trim(),
        "text": cited_text,
        "startIndex": annotation.start_index,
        "endIndex": annotation.end_index,
    }))
}

fn responses_annotation_value_to_metadata(
    annotation: &Value,
    part_text: Option<&str>,
) -> Option<Value> {
    let url = annotation.get("url").and_then(Value::as_str)?.trim();
    if url.is_empty() {
        return None;
    }
    let cited_text = part_text
        .and_then(|text| {
            let start = annotation.get("start_index").and_then(Value::as_u64)? as usize;
            let end = annotation.get("end_index").and_then(Value::as_u64)? as usize;
            text.get(start..end).map(str::trim)
        })
        .filter(|text| !text.is_empty())
        .unwrap_or("");
    Some(json!({
        "type": annotation
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("url_citation"),
        "url": url,
        "title": annotation
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim(),
        "text": cited_text,
        "startIndex": annotation.get("start_index"),
        "endIndex": annotation.get("end_index"),
    }))
}

fn normalize_responses_response(
    model: &str,
    response: ResponsesApiResponse,
) -> Result<MessageResponse, ApiError> {
    let mut text = String::new();
    let mut content = Vec::new();
    let mut citations = Vec::<Value>::new();
    if let Some(output_text) = response
        .output_text
        .filter(|value| !value.trim().is_empty())
    {
        text.push_str(&output_text);
    }
    let mut web_search_seen = false;
    let mut web_search_actions = Vec::<Value>::new();
    for item in &response.output {
        if item.kind == "web_search_call" {
            web_search_seen = true;
            if let Some(action) = item.action.clone() {
                web_search_actions.push(action.clone());
            }
            tracing::info!(
                item_id = item.id.as_deref().unwrap_or(""),
                status = item.status.as_deref().unwrap_or(""),
                action = %item.action.as_ref().map_or_else(String::new, ToString::to_string),
                "OpenAI Responses web_search_call observed"
            );
            continue;
        }
        if item.kind == "message" {
            for part in &item.content {
                citations.extend(part.annotations.iter().filter_map(|annotation| {
                    responses_annotation_to_metadata(annotation, part.text.as_deref())
                }));
                if matches!(part.kind.as_str(), "output_text" | "text") {
                    if let Some(value) = part.text.as_deref().filter(|value| !value.is_empty()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(value);
                    }
                }
            }
        } else if item.kind == "function_call" {
            if let Some(name) = item.name.as_deref().filter(|value| !value.is_empty()) {
                content.push(OutputContentBlock::ToolUse {
                    id: item
                        .call_id
                        .clone()
                        .or_else(|| item.id.clone())
                        .unwrap_or_else(|| "responses_function_call".to_string()),
                    name: name.to_string(),
                    input: item
                        .arguments
                        .as_deref()
                        .map(parse_tool_arguments)
                        .unwrap_or_else(|| json!({})),
                });
            }
        }
    }
    if !text.trim().is_empty() {
        content.insert(0, OutputContentBlock::Text { text });
    }
    if content.is_empty() {
        return Err(ApiError::InvalidSseFrame(if web_search_seen {
            "responses web_search returned no final text"
        } else {
            "responses endpoint returned no final text"
        }));
    }
    Ok(MessageResponse {
        id: response.id.unwrap_or_else(|| "responses".to_string()),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model: response.model.unwrap_or_else(|| model.to_string()),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Usage {
            input_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.input_tokens)
                .unwrap_or(0),
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.output_tokens)
                .unwrap_or(0),
        },
        request_id: None,
        provider_metadata: (web_search_seen
            || !citations.is_empty()
            || !web_search_actions.is_empty())
        .then(|| {
            json!({
                "responses": {
                    "citations": citations,
                    "webSearchSeen": web_search_seen,
                    "webSearchActions": web_search_actions,
                }
            })
        }),
    })
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({ "raw": arguments }))
}

fn next_sse_frame(buffer: &mut Vec<u8>) -> Option<String> {
    let separator = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })?;

    let (position, separator_len) = separator;
    let frame = buffer.drain(..position + separator_len).collect::<Vec<_>>();
    let frame_len = frame.len().saturating_sub(separator_len);
    Some(String::from_utf8_lossy(&frame[..frame_len]).into_owned())
}

fn summarize_sse_frame(frame: &str) -> String {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return "empty-frame".to_string();
    }

    let mut event_name: Option<&str> = None;
    let mut data_lines = Vec::new();
    for line in trimmed.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(event) = line.strip_prefix("event:") {
            event_name = Some(event.trim());
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }

    let data = data_lines.join("\\n");
    let data_preview = truncate_chars(&data, 220);
    format!(
        "event={} data={}",
        event_name.unwrap_or("(none)"),
        if data_preview.is_empty() {
            "(empty)".to_string()
        } else {
            data_preview
        }
    )
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn parse_sse_frame(
    frame: &str,
    provider: &str,
    model: &str,
) -> Result<Option<ChatCompletionChunk>, ApiError> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let mut data_lines = Vec::new();
    for line in trimmed.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let payload = data_lines.join("\n");
    if payload == "[DONE]" {
        return Ok(None);
    }
    // Some backends embed an error object in a data: frame instead of using an
    // HTTP error status. Surface the error message directly rather than letting
    // ChatCompletionChunk deserialization fail with a cryptic 'missing field' error.
    if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&payload) {
        if let Some(err_obj) = raw.get("error") {
            let msg = err_obj
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("provider returned an error in stream")
                .to_string();
            let code = err_obj
                .get("code")
                .and_then(serde_json::Value::as_u64)
                .map(|c| c as u16);
            let status = reqwest::StatusCode::from_u16(code.unwrap_or(400))
                .unwrap_or(reqwest::StatusCode::BAD_REQUEST);
            return Err(ApiError::Api {
                status,
                error_type: err_obj
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(str::to_owned),
                message: Some(msg),
                request_id: None,
                body: payload.clone(),
                retryable: false,
                retry_after: None,
            });
        }
    }
    serde_json::from_str::<ChatCompletionChunk>(&payload)
        .map(Some)
        .map_err(|error| ApiError::json_deserialize(provider, model, &payload, error))
}

fn parse_responses_sse_frame(
    frame: &str,
    provider: &str,
    model: &str,
) -> Result<Option<ResponsesStreamEvent>, ApiError> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let mut event_type: Option<String> = None;
    let mut data_lines = Vec::new();
    for line in trimmed.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(event) = line.strip_prefix("event:") {
            event_type = Some(event.trim().to_string());
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let payload = data_lines.join("\n");
    if payload == "[DONE]" {
        return Ok(None);
    }
    let raw = serde_json::from_str::<Value>(&payload)
        .map_err(|error| ApiError::json_deserialize(provider, model, &payload, error))?;
    if let Some(err_obj) = raw.get("error") {
        let msg = err_obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("provider returned an error in responses stream")
            .to_string();
        return Err(ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: err_obj
                .get("type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            message: Some(msg),
            request_id: None,
            body: payload,
            retryable: false,
            retry_after: None,
        });
    }
    let event_type = event_type
        .or_else(|| {
            raw.get("type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_default();
    if event_type.is_empty() {
        return Ok(None);
    }
    Ok(Some(ResponsesStreamEvent {
        event_type,
        payload: raw,
    }))
}

fn responses_output_text_from_value(payload: &Value) -> Option<String> {
    let response = payload.get("response").unwrap_or(payload);
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let mut combined = String::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        for content in item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let content_type = content.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(content_type, "output_text" | "text") {
                if let Some(text) = content.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(text);
                    }
                }
            }
        }
    }
    (!combined.is_empty()).then_some(combined)
}

fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(super::dotenv_value(key)),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[must_use]
pub fn has_api_key(key: &str) -> bool {
    read_env_non_empty(key)
        .ok()
        .and_then(std::convert::identity)
        .is_some()
}

#[must_use]
pub fn read_base_url(config: OpenAiCompatConfig) -> String {
    std::env::var(config.base_url_env).unwrap_or_else(|_| config.default_base_url.to_string())
}

/// Build the chat completions URL from a base URL.
/// Handles all common patterns:
///   - `https://api.openai.com/v1`             → `/v1/chat/completions`
///   - `https://open.bigmodel.cn/api/paas/v4`  → `/v4/chat/completions`
///   - `https://open.bigmodel.cn/api/paas/v4/chat` → `/v4/chat/completions`
///   - `https://.../v1/chat/completions`       → as-is (no double-append)
#[must_use]
pub fn chat_completions_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v4/chat") {
        format!("{trimmed}/completions")
    } else {
        // Both /v4 and /v1 paths end up at /chat/completions
        format!("{trimmed}/chat/completions")
    }
}

#[must_use]
pub fn responses_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/responses") {
        trimmed.to_string()
    } else if let Some(prefix) = trimmed.strip_suffix("/chat/completions") {
        format!("{prefix}/responses")
    } else {
        format!("{trimmed}/responses")
    }
}

/// DeepSeek exposes provider-native Responses API web search only for the
/// official Flash runtime. Other DeepSeek models and OpenAI-compatible hosts
/// must keep using their explicit AOS search tools until they declare support.
#[must_use]
pub fn supports_official_deepseek_responses_web_search(model: &str, base_url: &str) -> bool {
    let canonical_model = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if canonical_model != "deepseek-v4-flash" {
        return false;
    }
    is_official_deepseek_base_url(base_url)
}

fn is_official_deepseek_base_url(base_url: &str) -> bool {
    let normalized_url = base_url.trim().to_ascii_lowercase();
    let without_scheme = normalized_url
        .strip_prefix("https://")
        .or_else(|| normalized_url.strip_prefix("http://"))
        .unwrap_or(&normalized_url);
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    host == "api.deepseek.com"
}

/// Build the embeddings URL from a base URL.
/// Mirrors [`chat_completions_endpoint`] but for the `/embeddings` path.
/// Handles all common patterns:
///   - `https://api.openai.com/v1`             → `/v1/embeddings`
///   - `https://open.bigmodel.cn/api/paas/v4`  → `/v4/embeddings`
///   - `https://.../v1/embeddings`            → as-is (no double-append)
#[must_use]
pub fn embeddings_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/embeddings") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1/embed") || trimmed.ends_with("/v4/embed") {
        format!("{trimmed}/dings") // /v1/embed + dings → /v1/embeddings
    } else {
        // Both /v4 and /v1 paths end up at /embeddings
        format!("{trimmed}/embeddings")
    }
}

/// Build the images generations URL from a base URL.
/// Mirrors [`chat_completions_endpoint`] but for `/images/generations`.
#[must_use]
pub fn images_generations_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/images/generations") {
        trimmed.to_string()
    } else if trimmed.ends_with("/images") {
        format!("{trimmed}/generations")
    } else {
        format!("{trimmed}/images/generations")
    }
}

fn request_id_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(REQUEST_ID_HEADER)
        .or_else(|| headers.get(ALT_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

/// Parse the `Retry-After` header (seconds form) from a 429 response. Returns
/// `None` for non-429 responses or when the header is absent/unparseable. When
/// present, callers should honor this delay instead of the computed backoff so
/// we respect the server-provided rate-limit hint (matches Claude Code).
fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
    status: reqwest::StatusCode,
) -> Option<Duration> {
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}

async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let request_id = request_id_from_headers(response.headers());
    let retry_after = parse_retry_after(response.headers(), status);
    let body = response.text().await.unwrap_or_default();
    let parsed_error = serde_json::from_str::<ErrorEnvelope>(&body).ok();
    let top_level_error = serde_json::from_str::<TopLevelErrorEnvelope>(&body).ok();
    let retryable = is_retryable_status(status)
        && !is_non_retryable_quota_error(
            status,
            parsed_error
                .as_ref()
                .and_then(|error| error.error.error_type.as_deref())
                .or_else(|| {
                    top_level_error
                        .as_ref()
                        .and_then(|error| error.code.as_deref())
                }),
            parsed_error
                .as_ref()
                .and_then(|error| error.error.message.as_deref())
                .or_else(|| {
                    top_level_error
                        .as_ref()
                        .and_then(|error| error.message.as_deref())
                }),
            &body,
        );

    Err(ApiError::Api {
        status,
        error_type: parsed_error
            .as_ref()
            .and_then(|error| error.error.error_type.clone()),
        message: parsed_error
            .as_ref()
            .and_then(|error| error.error.message.clone())
            .or_else(|| {
                top_level_error
                    .as_ref()
                    .and_then(|error| error.message.clone())
            })
            .or_else(|| {
                top_level_error
                    .as_ref()
                    .and_then(|error| error.code.clone())
            }),
        request_id,
        body,
        retryable,
        retry_after,
    })
}

const fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429 | 500 | 502 | 503 | 504)
}

fn is_non_retryable_quota_error(
    status: reqwest::StatusCode,
    error_type: Option<&str>,
    message: Option<&str>,
    body: &str,
) -> bool {
    if status.as_u16() != 429 {
        return false;
    }

    let mut haystack = String::new();
    if let Some(t) = error_type {
        haystack.push_str(t);
        haystack.push(' ');
    }
    if let Some(m) = message {
        haystack.push_str(m);
        haystack.push(' ');
    }
    haystack.push_str(body);
    let text = haystack.to_ascii_lowercase();

    [
        "insufficient_quota",
        "usage_limit_exceeded",
        "daily_limit_exceeded",
        "daily usage limit exceeded",
        "quota exceeded",
        "quota_exceeded",
        "insufficient balance",
        "insufficient_balance",
        "insufficient credits",
        "insufficient_credits",
        "credit balance",
        "billing",
        "payment required",
        "recharge",
        "余额不足",
        "额度不足",
        "欠费",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn normalize_finish_reason(value: &str) -> String {
    match value {
        "stop" => "end_turn",
        "tool_calls" => "tool_use",
        other => other,
    }
    .to_string()
}

trait StringExt {
    fn if_empty_then(self, fallback: String) -> String;
}

impl StringExt for String {
    fn if_empty_then(self, fallback: String) -> String {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        adapt_responses_web_search_request_for_provider, build_chat_completion_request,
        build_responses_web_search_request, chat_completions_endpoint, images_generations_endpoint,
        is_reasoning_model, normalize_finish_reason, normalize_response,
        normalize_responses_response, openai_tool_choice, parse_responses_sse_frame,
        parse_tool_arguments, response_exhausted_in_reasoning, responses_endpoint,
        supports_official_deepseek_responses_web_search, ChatChoice, ChatCompletionChunk,
        ChatCompletionResponse, ChatMessage, ChunkChoice, ChunkDelta, OpenAiCompatClient,
        OpenAiCompatConfig, ResponsesApiResponse, ResponsesStreamState, StreamState,
        DEFAULT_MAX_RETRIES, DEFAULT_OPENAI_BASE_URL, DEFAULT_XAI_BASE_URL,
        RESPONSES_WEB_SEARCH_BRIEF_CHAR_LIMIT,
    };
    use crate::error::ApiError;
    use crate::types::{
        ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
        InputContentBlock, InputMessage, MessageDeltaEvent, MessageRequest, OutputContentBlock,
        StreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock,
    };
    use serde_json::json;

    #[test]
    fn interactive_client_uses_bounded_default_retry_budget() {
        let client = OpenAiCompatClient::new("test-key", OpenAiCompatConfig::openai());
        assert_eq!(DEFAULT_MAX_RETRIES, 2);
        assert_eq!(client.max_retries, 2);
    }

    #[test]
    fn declared_and_learned_output_limits_cap_future_requests_per_endpoint_key() {
        let model = "adaptive-limit-test-model";
        let client = OpenAiCompatClient::new("adaptive-limit-key-a", OpenAiCompatConfig::openai())
            .with_base_url("https://limits-a.example/v1/")
            .with_token_limits(Some(100_000), Some(16_000));
        let request = MessageRequest {
            model: model.to_string(),
            max_tokens: 32_000,
            ..Default::default()
        };
        assert_eq!(client.prepare_request(&request, false).max_tokens, 16_000);

        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: Some("invalid_request_error".to_string()),
            message: Some("max_completion_tokens must be less than or equal to 8192".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            retry_after: None,
        };
        client.remember_token_limits(model, &error);

        let recreated =
            OpenAiCompatClient::new("adaptive-limit-key-a", OpenAiCompatConfig::openai())
                .with_base_url("https://limits-a.example/v1")
                .with_token_limits(Some(100_000), Some(16_000));
        assert_eq!(recreated.prepare_request(&request, false).max_tokens, 8_192);

        let other_key =
            OpenAiCompatClient::new("adaptive-limit-key-b", OpenAiCompatConfig::openai())
                .with_base_url("https://limits-a.example/v1")
                .with_token_limits(Some(100_000), Some(16_000));
        assert_eq!(
            other_key.prepare_request(&request, false).max_tokens,
            16_000
        );
    }

    #[test]
    fn learned_context_limit_is_used_by_local_preflight() {
        let model = "adaptive-context-test-model";
        let client = OpenAiCompatClient::new("adaptive-context-key", OpenAiCompatConfig::openai())
            .with_base_url("https://limits-context.example/v1");
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: Some("invalid_request_error".to_string()),
            message: Some("maximum context length is 128 tokens".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            retry_after: None,
        };
        client.remember_token_limits(model, &error);
        let request = MessageRequest {
            model: model.to_string(),
            max_tokens: 64,
            messages: vec![InputMessage::user_text("x".repeat(1_024))],
            ..Default::default()
        };
        assert!(matches!(
            client.preflight_request(&request),
            Err(ApiError::ContextWindowExceeded {
                context_window_tokens: 128,
                ..
            })
        ));
    }

    #[test]
    fn reasoning_only_length_response_requires_budget_expansion() {
        let response = crate::types::MessageResponse {
            id: "response-1".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![OutputContentBlock::Thinking {
                thinking: "still reasoning".to_string(),
                signature: None,
            }],
            model: "deepseek-v4-pro".to_string(),
            stop_reason: Some("length".to_string()),
            stop_sequence: None,
            usage: crate::types::Usage::default(),
            request_id: None,
            provider_metadata: None,
        };
        assert!(response_exhausted_in_reasoning(&response));

        let completed = crate::types::MessageResponse {
            content: vec![OutputContentBlock::Text {
                text: "SELECT 1".to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            ..response
        };
        assert!(!response_exhausted_in_reasoning(&completed));
    }
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn request_translation_uses_openai_compatible_shape() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "grok-3".to_string(),
                max_tokens: 64,
                messages: vec![
                    InputMessage {
                        role: "user".to_string(),
                        content: vec![InputContentBlock::Text {
                            text: "hello".to_string(),
                        }],
                    },
                    InputMessage {
                        role: "assistant".to_string(),
                        content: vec![InputContentBlock::ToolUse {
                            id: "tool_1".to_string(),
                            name: "weather".to_string(),
                            input: json!({"city": "Beijing"}),
                        }],
                    },
                    InputMessage {
                        role: "user".to_string(),
                        content: vec![InputContentBlock::ToolResult {
                            tool_use_id: "tool_1".to_string(),
                            content: vec![ToolResultContentBlock::Json {
                                value: json!({"ok": true}),
                            }],
                            is_error: false,
                        }],
                    },
                ],
                system: Some("be helpful".to_string()),
                tools: Some(vec![ToolDefinition {
                    name: "weather".to_string(),
                    description: Some("Get weather".to_string()),
                    input_schema: json!({"type": "object"}),
                }]),
                tool_choice: Some(ToolChoice::Auto),
                stream: false,
                ..Default::default()
            },
            OpenAiCompatConfig::xai(),
            DEFAULT_XAI_BASE_URL,
        );

        assert_eq!(payload["messages"][0]["role"], json!("system"));
        assert_eq!(payload["messages"][1]["role"], json!("user"));
        assert_eq!(payload["messages"][2]["role"], json!("assistant"));
        assert_eq!(payload["messages"][3]["role"], json!("tool"));
        assert_eq!(payload["tools"][0]["type"], json!("function"));
        assert_eq!(payload["tool_choice"], json!("auto"));
    }

    #[test]
    fn tool_schema_object_gets_strict_fields_for_responses_endpoint() {
        // OpenAI /responses endpoint rejects object schemas missing
        // "properties" and "additionalProperties". Verify normalize_object_schema
        // fills them in so the request shape is strict-validator-safe.
        use super::normalize_object_schema;

        // Bare object — no properties at all
        let mut schema = json!({"type": "object"});
        normalize_object_schema(&mut schema);
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["additionalProperties"], json!(false));

        // Nested object inside properties
        let mut schema2 = json!({
            "type": "object",
            "properties": {
                "location": {"type": "object", "properties": {"lat": {"type": "number"}}}
            }
        });
        normalize_object_schema(&mut schema2);
        assert_eq!(schema2["additionalProperties"], json!(false));
        assert_eq!(
            schema2["properties"]["location"]["additionalProperties"],
            json!(false)
        );

        // Existing properties/additionalProperties should not be overwritten
        let mut schema3 = json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "additionalProperties": true
        });
        normalize_object_schema(&mut schema3);
        assert_eq!(
            schema3["additionalProperties"],
            json!(true),
            "must not overwrite existing"
        );
    }

    #[test]
    fn reasoning_effort_is_included_when_set() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "o4-mini".to_string(),
                max_tokens: 1024,
                messages: vec![InputMessage::user_text("think hard")],
                reasoning_effort: Some("high".to_string()),
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert_eq!(payload["reasoning_effort"], json!("high"));
    }

    #[test]
    fn reasoning_effort_omitted_when_not_set() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "gpt-4o".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert!(payload.get("reasoning_effort").is_none());
    }

    #[test]
    fn openai_streaming_requests_include_usage_opt_in() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "gpt-5".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                system: None,
                tools: None,
                tool_choice: None,
                stream: true,
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );

        assert_eq!(payload["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn custom_openai_compatible_base_does_not_receive_stream_usage_opt_in() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "gpt-5.5".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                system: None,
                tools: Some(vec![ToolDefinition {
                    name: "read_file".to_string(),
                    description: Some("Read file".to_string()),
                    input_schema: json!({"type": "object"}),
                }]),
                tool_choice: Some(ToolChoice::Auto),
                stream: true,
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            "https://code.newcli.com/codex/v1",
        );

        assert!(
            payload.get("stream_options").is_none(),
            "custom OpenAI-compatible gateways may return empty streams when stream_options is sent"
        );
    }

    #[test]
    fn xai_streaming_requests_include_usage_opt_in() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "grok-3".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                system: None,
                tools: None,
                tool_choice: None,
                stream: true,
                ..Default::default()
            },
            OpenAiCompatConfig::xai(),
            DEFAULT_XAI_BASE_URL,
        );

        assert_eq!(payload["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn tool_choice_translation_supports_required_function() {
        assert_eq!(openai_tool_choice(&ToolChoice::Any), json!("required"));
        assert_eq!(
            openai_tool_choice(&ToolChoice::Tool {
                name: "weather".to_string(),
            }),
            json!({"type": "function", "function": {"name": "weather"}})
        );
    }

    #[test]
    fn parses_tool_arguments_fallback() {
        assert_eq!(
            parse_tool_arguments("{\"city\":\"Paris\"}"),
            json!({"city": "Paris"})
        );
        assert_eq!(parse_tool_arguments("not-json"), json!({"raw": "not-json"}));
    }

    #[test]
    fn missing_xai_api_key_is_provider_specific() {
        let _lock = env_lock();
        std::env::remove_var("XAI_API_KEY");
        let error = OpenAiCompatClient::from_env(OpenAiCompatConfig::xai())
            .expect_err("missing key should error");
        assert!(matches!(
            error,
            ApiError::MissingCredentials {
                provider: "xAI",
                ..
            }
        ));
    }

    #[test]
    fn endpoint_builder_accepts_base_urls_and_full_endpoints() {
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/"),
            "https://api.x.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://api.x.ai/v1/chat/completions"),
            "https://api.x.ai/v1/chat/completions"
        );
        // GLM-style: base includes version path /v4/chat
        assert_eq!(
            chat_completions_endpoint("https://open.bigmodel.cn/api/paas/v4/chat"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        // OpenAI-compatible proxy with /v4 base
        assert_eq!(
            chat_completions_endpoint("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        // Bare base without version segment
        assert_eq!(
            chat_completions_endpoint("https://open.bigmodel.cn/api/paas"),
            "https://open.bigmodel.cn/api/paas/chat/completions"
        );
        assert_eq!(
            responses_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            responses_endpoint("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            responses_endpoint("https://proxy.example.com/v1/responses"),
            "https://proxy.example.com/v1/responses"
        );
        assert!(supports_official_deepseek_responses_web_search(
            "deepseek-v4-flash",
            "https://api.deepseek.com/v1"
        ));
        assert!(!supports_official_deepseek_responses_web_search(
            "deepseek-v4-pro",
            "https://api.deepseek.com/v1"
        ));
        assert!(!supports_official_deepseek_responses_web_search(
            "deepseek-v4-flash",
            "https://api.deepseek.com.evil.example/v1"
        ));
        assert_eq!(
            images_generations_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/images/generations"
        );
        assert_eq!(
            images_generations_endpoint("https://api.openai.com/v1/images"),
            "https://api.openai.com/v1/images/generations"
        );
        assert_eq!(
            images_generations_endpoint("https://api.openai.com/v1/images/generations"),
            "https://api.openai.com/v1/images/generations"
        );
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    #[test]
    fn normalizes_stop_reasons() {
        assert_eq!(normalize_finish_reason("stop"), "end_turn");
        assert_eq!(normalize_finish_reason("tool_calls"), "tool_use");
    }

    #[test]
    fn tuning_params_included_in_payload_when_set() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.7),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.3),
            stop: Some(vec!["\n".to_string()]),
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert_eq!(payload["temperature"], 0.7);
        assert_eq!(payload["top_p"], 0.9);
        assert_eq!(payload["frequency_penalty"], 0.5);
        assert_eq!(payload["presence_penalty"], 0.3);
        assert_eq!(payload["stop"], json!(["\n"]));
    }

    #[test]
    fn reasoning_model_strips_tuning_params() {
        let request = MessageRequest {
            model: "o1-mini".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            temperature: Some(0.7),
            top_p: Some(0.9),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.3),
            stop: Some(vec!["\n".to_string()]),
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert!(
            payload.get("temperature").is_none(),
            "reasoning model should strip temperature"
        );
        assert!(
            payload.get("top_p").is_none(),
            "reasoning model should strip top_p"
        );
        assert!(payload.get("frequency_penalty").is_none());
        assert!(payload.get("presence_penalty").is_none());
        // stop is safe for all providers
        assert_eq!(payload["stop"], json!(["\n"]));
    }

    #[test]
    fn responses_web_search_request_uses_responses_tool_shape() {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert(
            "web_search_options".to_string(),
            json!({"search_context_size": "high"}),
        );
        let request = MessageRequest {
            model: "openai/gpt-5.4".to_string(),
            max_tokens: 2048,
            system: Some("Use current sources.".to_string()),
            messages: vec![InputMessage::user_text("weather beijing")],
            tools: Some(vec![ToolDefinition {
                name: "lookup_local".to_string(),
                description: Some("Look up authorized local context".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                }),
            }]),
            extra_body: Some(extra_body),
            ..Default::default()
        };

        let payload = build_responses_web_search_request(&request);

        assert_eq!(payload["model"], "gpt-5.4");
        assert_eq!(
            payload["tools"][0],
            json!({
                "type": "web_search",
                "external_web_access": true,
                "search_context_size": "high",
            })
        );
        assert_eq!(payload["tools"].as_array().map(Vec::len), Some(2));
        assert_eq!(payload["tools"][1]["type"], "function");
        assert_eq!(payload["tools"][1]["name"], "lookup_local");
        assert_eq!(
            payload["tools"][1]["description"],
            "Look up authorized local context"
        );
        assert_eq!(payload["tools"][1]["strict"], false);
        assert_eq!(payload["tools"][1]["parameters"]["type"], "object");
        assert_eq!(
            payload["tools"][1]["parameters"]["additionalProperties"],
            false
        );
        assert!(payload["tools"][1].get("function").is_none());
        assert_eq!(payload["tool_choice"], "auto");
        assert!(payload.get("web_search_options").is_none());
        assert!(payload["input"]
            .as_str()
            .unwrap_or("")
            .contains("Use current sources."));
        assert!(payload["input"]
            .as_str()
            .unwrap_or("")
            .contains("weather beijing"));
    }

    #[test]
    fn deepseek_flash_responses_request_omits_openai_only_tool_field() {
        let request = MessageRequest {
            model: "deepseek-v4-flash".to_string(),
            max_tokens: 2048,
            messages: vec![InputMessage::user_text("北京天气")],
            ..Default::default()
        };
        let mut payload = build_responses_web_search_request(&request);
        adapt_responses_web_search_request_for_provider(
            &mut payload,
            &request.model,
            "https://api.deepseek.com/v1",
        );

        assert_eq!(payload["tools"][0], json!({"type": "web_search"}));
        assert_eq!(payload["tool_choice"], json!("auto"));
    }

    #[test]
    fn official_deepseek_v4_thinking_omits_tool_choice_for_tools() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            max_tokens: 2048,
            messages: vec![InputMessage::user_text("北京天气")],
            tools: Some(vec![ToolDefinition {
                name: "WebSearch".to_string(),
                description: Some("Search the web".to_string()),
                input_schema: json!({"type": "object", "properties": {}}),
            }]),
            tool_choice: Some(ToolChoice::Auto),
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            "https://api.deepseek.com/v1",
        );

        assert!(payload.get("tools").is_some());
        assert!(payload.get("tool_choice").is_none());
    }

    #[test]
    fn official_deepseek_v4_with_thinking_disabled_preserves_tool_choice() {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert("thinking".to_string(), json!({"type": "disabled"}));
        let request = MessageRequest {
            model: "deepseek-v4-flash".to_string(),
            max_tokens: 2048,
            messages: vec![InputMessage::user_text("北京天气")],
            tools: Some(vec![ToolDefinition {
                name: "WebSearch".to_string(),
                description: Some("Search the web".to_string()),
                input_schema: json!({"type": "object", "properties": {}}),
            }]),
            tool_choice: Some(ToolChoice::Auto),
            extra_body: Some(extra_body),
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            "https://api.deepseek.com/v1",
        );

        assert_eq!(payload["tool_choice"], json!("auto"));
        assert_eq!(payload["thinking"], json!({"type": "disabled"}));
    }

    #[test]
    fn responses_web_search_request_defaults_to_plain_minimal_payload() {
        let request = MessageRequest {
            model: "openai/gpt-5.4".to_string(),
            max_tokens: 64_000,
            system: Some("Use current sources.".to_string()),
            messages: vec![InputMessage::user_text("weather beijing")],
            ..Default::default()
        };

        let payload = build_responses_web_search_request(&request);

        assert_eq!(payload["model"], "gpt-5.4");
        assert_eq!(
            payload["tools"],
            json!([{ "type": "web_search", "external_web_access": true }])
        );
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["max_output_tokens"], json!(64_000));
        assert!(payload["input"].is_string());
        assert!(payload.get("web_search_options").is_none());
    }

    #[test]
    fn responses_web_search_brief_keeps_current_request_ahead_of_long_system_prompt() {
        let request = MessageRequest {
            model: "openai/gpt-5.4".to_string(),
            max_tokens: 64_000,
            system: Some("Very long policy. ".repeat(800)),
            messages: vec![InputMessage::user_text(
                "查一下北京明天天气，重点看降雨概率和来源。",
            )],
            ..Default::default()
        };

        let payload = build_responses_web_search_request(&request);
        let input = payload["input"].as_str().expect("plain string input");

        assert!(input.starts_with("Search brief:"));
        assert!(input.contains("Current user request:"));
        assert!(input.contains("北京明天天气"));
        assert!(
            input.find("北京明天天气") < input.find("Very long policy").or(Some(usize::MAX)),
            "current request must not be buried behind a long system prompt"
        );
        assert!(input.chars().count() <= RESPONSES_WEB_SEARCH_BRIEF_CHAR_LIMIT);
    }

    #[test]
    fn responses_web_search_brief_instructs_iterative_source_quality() {
        let request = MessageRequest {
            model: "openai/gpt-5.4".to_string(),
            max_tokens: 4096,
            system: Some("Use current sources.".to_string()),
            messages: vec![InputMessage::user_text(
                "查竞品策略和可验证案例，不要泛泛而谈。",
            )],
            ..Default::default()
        };

        let payload = build_responses_web_search_request(&request);
        let input = payload["input"].as_str().expect("plain string input");

        assert!(input.contains("iterative research tool"));
        assert!(input.contains("reformulate the query"));
        assert!(input.contains("generic, stale, or off-topic"));
        assert!(input.contains("Open or inspect promising results"));
        assert!(input.contains("multiple distinct relevant sources"));
    }

    #[test]
    fn responses_web_search_brief_recovers_relevant_business_context() {
        let request = MessageRequest {
            model: "openai/gpt-5.4".to_string(),
            max_tokens: 4096,
            system: Some("Use concise source-backed search.".to_string()),
            messages: vec![
                InputMessage::user_text(
                    "业务背景：LUCKYMAHJONGMATCH_A / mahjong_ID，印尼网赚休闲产品。ROI 1.235，AIPU 17.11，eCPM 5+ 用户贡献 51% 收入。",
                ),
                InputMessage::user_text("无关闲聊：今天午饭吃什么？"),
                InputMessage::user_text(
                    "基于 ROI、AIPU、eCPM 分层，查竞品 rewarded ads 和激励经济的有效玩法。",
                ),
            ],
            ..Default::default()
        };

        let payload = build_responses_web_search_request(&request);
        let input = payload["input"].as_str().expect("plain string input");

        assert!(input.contains("Current user request:"));
        assert!(input.contains("Relevant prior context:"));
        assert!(input.contains("LUCKYMAHJONGMATCH_A"));
        assert!(input.contains("mahjong_ID"));
        assert!(input.contains("AIPU 17.11"));
        assert!(input.contains("Likely search focus:"));
        assert!(
            input.find("Current user request:").unwrap()
                < input.find("Relevant prior context:").unwrap()
        );
    }

    #[test]
    fn responses_web_search_response_normalizes_output_text() {
        let raw = serde_json::json!({
            "id": "resp_1",
            "model": "gpt-5.4",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {"type": "search", "query": "weather beijing"}
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "北京今天有小雨概率。"}
                    ]
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });
        let response: ResponsesApiResponse = serde_json::from_value(raw).expect("valid response");
        let normalized =
            normalize_responses_response("fallback-model", response).expect("normalized");

        assert_eq!(normalized.model, "gpt-5.4");
        assert_eq!(normalized.usage.input_tokens, 10);
        assert_eq!(normalized.usage.output_tokens, 20);
        assert_eq!(
            normalized.content,
            vec![OutputContentBlock::Text {
                text: "北京今天有小雨概率。".to_string()
            }]
        );
    }

    #[test]
    fn responses_web_search_response_preserves_url_citations_metadata() {
        let raw = serde_json::json!({
            "id": "resp_1",
            "model": "gpt-5.4",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": {"type": "search", "query": "gold price"}
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Gold moved higher today.",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://example.com/gold",
                                    "title": "Gold market update",
                                    "start_index": 0,
                                    "end_index": 4
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 20}
        });
        let response: ResponsesApiResponse = serde_json::from_value(raw).expect("valid response");
        let normalized =
            normalize_responses_response("fallback-model", response).expect("normalized");
        let citations = normalized
            .provider_metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/responses/citations"))
            .and_then(Value::as_array)
            .expect("citations metadata");

        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["url"], "https://example.com/gold");
        assert_eq!(citations[0]["title"], "Gold market update");
        assert_eq!(citations[0]["text"], "Gold");
    }

    #[test]
    fn responses_response_normalizes_function_call() {
        let raw = serde_json::json!({
            "id": "resp_tool",
            "model": "gpt-5.4",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "lookup_local",
                    "arguments": "{\"query\":\"北京天气\"}"
                }
            ],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        });
        let response: ResponsesApiResponse = serde_json::from_value(raw).expect("valid response");
        let normalized =
            normalize_responses_response("fallback-model", response).expect("normalized");

        assert_eq!(
            normalized.content,
            vec![OutputContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "lookup_local".to_string(),
                input: json!({"query": "北京天气"})
            }]
        );
    }

    #[test]
    fn responses_sse_state_streams_text_usage_without_local_tool_use() {
        let frames = [
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_stream\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.web_search_call.searching\ndata: {\"type\":\"response.web_search_call.searching\",\"item_id\":\"ws_1\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"北京\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"今天有雨。\"}\n\n",
            "event: response.output_text.done\ndata: {\"type\":\"response.output_text.done\",\"text\":\"北京今天有雨。\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_stream\",\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":6,\"input_tokens_details\":{\"cached_tokens\":2}},\"output_text\":\"北京今天有雨。\"}}\n\n",
        ];
        let mut state = ResponsesStreamState::new("fallback-model".to_string());
        let mut events = Vec::new();
        for frame in frames {
            let parsed = parse_responses_sse_frame(frame, "OpenAI", "gpt-5.5")
                .expect("frame parses")
                .expect("event");
            events.extend(state.ingest_event(parsed).expect("state ingests"));
        }

        let text = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    delta: ContentBlockDelta::TextDelta { text },
                    ..
                }) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        assert_eq!(text, "北京今天有雨。");
        assert!(
            events.iter().all(|event| !matches!(
                event,
                StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                    content_block: OutputContentBlock::ToolUse { .. },
                    ..
                })
            )),
            "Responses web_search_call must stay provider-native activity, not a local tool call"
        );
        let usage = events.iter().find_map(|event| match event {
            StreamEvent::MessageDelta(MessageDeltaEvent { usage, .. }) => Some(usage),
            _ => None,
        });
        let usage = usage.expect("completed response should emit usage");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.cache_read_input_tokens, 2);
        assert_eq!(usage.output_tokens, 6);
    }

    #[test]
    fn responses_sse_state_streams_reasoning_before_text() {
        let frames = [
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_reasoning\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"先检索来源\"}\n\n",
            "event: response.reasoning_summary_text.done\ndata: {\"type\":\"response.reasoning_summary_text.done\",\"text\":\"先检索来源\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"结论\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_reasoning\",\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":4}}}\n\n",
        ];
        let mut state = ResponsesStreamState::new("fallback-model".to_string());
        let mut events = Vec::new();
        for frame in frames {
            let parsed = parse_responses_sse_frame(frame, "OpenAI", "gpt-5.5")
                .expect("frame parses")
                .expect("event");
            events.extend(state.ingest_event(parsed).expect("state ingests"));
        }

        let body = events
            .iter()
            .filter(|event| !matches!(event, StreamEvent::MessageStart(_)))
            .collect::<Vec<_>>();
        assert!(matches!(
            body[0],
            StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                index: 0,
                content_block: OutputContentBlock::Thinking { .. },
            })
        ));
        assert!(matches!(
            body[1],
            StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                index: 0,
                delta: ContentBlockDelta::ThinkingDelta { .. },
            })
        ));
        assert!(matches!(
            body[2],
            StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0 })
        ));
        assert!(matches!(
            body[3],
            StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                index: 1,
                content_block: OutputContentBlock::Text { .. },
            })
        ));
        assert!(matches!(
            body[4],
            StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                index: 1,
                delta: ContentBlockDelta::TextDelta { .. },
            })
        ));
    }

    #[test]
    fn responses_sse_state_streams_function_call_as_tool_use() {
        let frames = [
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool_stream\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"lookup_local\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"query\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"北京天气\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"arguments\":\"{\\\"query\\\":\\\"北京天气\\\"}\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_stream\",\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":8,\"output_tokens\":5}}}\n\n",
        ];
        let mut state = ResponsesStreamState::new("fallback-model".to_string());
        let mut events = Vec::new();
        for frame in frames {
            let parsed = parse_responses_sse_frame(frame, "OpenAI", "gpt-5.5")
                .expect("frame parses")
                .expect("event");
            events.extend(state.ingest_event(parsed).expect("state ingests"));
        }

        assert!(matches!(
            events.iter().find(|event| matches!(
                event,
                StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                    content_block: OutputContentBlock::ToolUse { name, .. },
                    ..
                }) if name == "lookup_local"
            )),
            Some(_)
        ));
        let args = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                    delta: ContentBlockDelta::InputJsonDelta { partial_json },
                    ..
                }) => Some(partial_json.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(args, "{\"query\":\"北京天气\"}");
    }

    #[test]
    fn grok_3_mini_is_reasoning_model() {
        assert!(is_reasoning_model("grok-3-mini"));
        assert!(is_reasoning_model("o1"));
        assert!(is_reasoning_model("o1-mini"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("grok-3"));
        assert!(!is_reasoning_model("claude-sonnet-4-6"));
    }

    #[test]
    fn qwen_reasoning_variants_are_detected() {
        // QwQ reasoning model
        assert!(is_reasoning_model("qwen-qwq-32b"));
        assert!(is_reasoning_model("qwen/qwen-qwq-32b"));
        // Qwen3 thinking family
        assert!(is_reasoning_model("qwen3-30b-a3b-thinking"));
        assert!(is_reasoning_model("qwen/qwen3-30b-a3b-thinking"));
        // Bare qwq
        assert!(is_reasoning_model("qwq-plus"));
        // Regular Qwen models must NOT be classified as reasoning
        assert!(!is_reasoning_model("qwen-max"));
        assert!(!is_reasoning_model("qwen/qwen-plus"));
        assert!(!is_reasoning_model("qwen-turbo"));
    }

    #[test]
    fn deepseek_and_glm_reasoning_variants_are_detected() {
        assert!(is_reasoning_model("deepseek-r1"));
        assert!(is_reasoning_model("deepseek-reasoner"));
        assert!(is_reasoning_model("deepseek/deepseek-r1"));
        assert!(is_reasoning_model("z-ai/glm-5"));
        assert!(is_reasoning_model("glm-4.5"));
        assert!(is_reasoning_model("glm-4-5-turbo"));
        // Non-reasoning variants must NOT match.
        assert!(!is_reasoning_model("deepseek-chat"));
        assert!(!is_reasoning_model("glm-4"));
        assert!(!is_reasoning_model("glm-4-plus"));
    }

    #[test]
    fn openrouter_reasoning_opt_in_is_added_for_reasoning_models() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "deepseek/deepseek-r1".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            "https://openrouter.ai/api/v1",
        );
        assert_eq!(payload["reasoning"], json!({"exclude": false}));
        assert_eq!(payload["include_reasoning"], json!(true));
    }

    #[test]
    fn openrouter_reasoning_opt_in_is_skipped_for_non_reasoning_models() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "openai/gpt-4o".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            "https://openrouter.ai/api/v1",
        );
        assert!(payload.get("reasoning").is_none());
        assert!(payload.get("include_reasoning").is_none());
    }

    #[test]
    fn non_openrouter_backends_do_not_receive_openrouter_flags() {
        let payload = build_chat_completion_request(
            &MessageRequest {
                model: "deepseek-reasoner".to_string(),
                max_tokens: 64,
                messages: vec![InputMessage::user_text("hello")],
                ..Default::default()
            },
            OpenAiCompatConfig::openai(),
            "https://api.deepseek.com/v1",
        );
        assert!(
            payload.get("reasoning").is_none(),
            "deepseek API uses reasoning_content, not the OpenRouter flag"
        );
        assert!(payload.get("include_reasoning").is_none());
    }

    #[test]
    fn stream_state_emits_thinking_delta_for_reasoning_content() {
        // A single chunk with both reasoning_content and content should
        // produce: ThinkingStart → ThinkingDelta → ThinkingStop → TextStart → TextDelta.
        let mut state = StreamState::new("deepseek-r1".to_string());
        let chunk = ChatCompletionChunk {
            id: "chatcmpl_1".to_string(),
            model: Some("deepseek-r1".to_string()),
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: Some("Hi".to_string()),
                    reasoning_content: Some("Let me think".to_string()),
                    reasoning: None,
                    tool_calls: vec![],
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events = state.ingest_chunk(chunk).expect("ingest should succeed");

        // Filter out MessageStart (present only on the first chunk).
        let body: Vec<&StreamEvent> = events
            .iter()
            .filter(|e| !matches!(e, StreamEvent::MessageStart(_)))
            .collect();

        // Expected sequence: ThinkingStart(0), ThinkingDelta(0), ThinkingStop(0), TextStart(1), TextDelta(1).
        assert!(matches!(
            body[0],
            StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                index: 0,
                content_block: OutputContentBlock::Thinking { .. },
                ..
            })
        ));
        assert!(matches!(
            body[1],
            StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                index: 0,
                delta: ContentBlockDelta::ThinkingDelta { .. },
                ..
            })
        ));
        assert!(matches!(
            body[2],
            StreamEvent::ContentBlockStop(ContentBlockStopEvent { index: 0, .. })
        ));
        assert!(matches!(
            body[3],
            StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                index: 1,
                content_block: OutputContentBlock::Text { .. },
                ..
            })
        ));
        assert!(matches!(
            body[4],
            StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                index: 1,
                delta: ContentBlockDelta::TextDelta { .. },
                ..
            })
        ));
    }

    #[test]
    fn stream_state_emits_text_only_when_no_reasoning() {
        // Sanity check: backwards-compat path — without reasoning, text
        // stays at index 0 so existing non-reasoning models are unaffected.
        let mut state = StreamState::new("gpt-4o".to_string());
        let chunk = ChatCompletionChunk {
            id: "chatcmpl_2".to_string(),
            model: Some("gpt-4o".to_string()),
            choices: vec![ChunkChoice {
                delta: ChunkDelta {
                    content: Some("Hello".to_string()),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events = state.ingest_chunk(chunk).expect("ingest should succeed");
        let starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart(ev) => Some(ev.index),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![0]);
    }

    #[test]
    fn normalize_response_surfaces_reasoning_as_thinking_block() {
        let response = ChatCompletionResponse {
            id: "msg_1".to_string(),
            model: "deepseek-r1".to_string(),
            choices: vec![ChatChoice {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some("Final answer".to_string()),
                    reasoning_content: Some("Step 1: inspect the data".to_string()),
                    reasoning: None,
                    tool_calls: vec![],
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        };
        let result = normalize_response("deepseek-r1", response).expect("normalize ok");
        assert_eq!(result.content.len(), 2);
        assert!(matches!(
            result.content[0],
            OutputContentBlock::Thinking { .. }
        ));
        assert!(matches!(result.content[1], OutputContentBlock::Text { .. }));
    }

    #[test]
    fn tuning_params_omitted_from_payload_when_none() {
        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert!(
            payload.get("temperature").is_none(),
            "temperature should be absent"
        );
        assert!(payload.get("top_p").is_none(), "top_p should be absent");
        assert!(payload.get("frequency_penalty").is_none());
        assert!(payload.get("presence_penalty").is_none());
        assert!(payload.get("stop").is_none());
    }

    #[test]
    fn extra_body_appends_native_search_tool_but_cannot_override_core_fields() {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert("model".to_string(), json!("must-not-override"));
        extra_body.insert("stream".to_string(), json!(false));
        extra_body.insert("messages".to_string(), json!([]));
        extra_body.insert("tools".to_string(), json!([]));
        extra_body.insert("tool_choice".to_string(), json!("none"));
        extra_body.insert(
            "web_search_options".to_string(),
            json!({"search_context_size": "high"}),
        );
        extra_body.insert(
            "__aos_append_tools".to_string(),
            json!([{ "type": "web_search_preview" }]),
        );
        extra_body.insert("__aos_tool_choice_auto".to_string(), json!(true));

        let request = MessageRequest {
            model: "future-model".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: true,
            tools: None,
            tool_choice: None,
            extra_body: Some(extra_body),
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );

        assert_eq!(payload["model"], json!("future-model"));
        assert_eq!(payload["stream"], json!(true));
        assert_eq!(payload["messages"], json!([]));
        assert_eq!(payload["tools"], json!([{ "type": "web_search_preview" }]));
        assert_eq!(payload["tool_choice"], json!("auto"));
        assert_eq!(
            payload["web_search_options"],
            json!({"search_context_size": "high"})
        );
    }

    #[test]
    fn capability_uses_max_completion_tokens_not_max_tokens() {
        // Some models/endpoints require `max_completion_tokens`; runtime config
        // opts into this without relying on model-name prefixes.
        let request = MessageRequest {
            model: "future-reasoning-model".to_string(),
            max_tokens: 512,
            messages: vec![],
            stream: false,
            use_max_completion_tokens: Some(true),
            extra_body: None,
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert_eq!(
            payload["max_completion_tokens"],
            json!(512),
            "capability-enabled request should emit max_completion_tokens"
        );
        assert!(
            payload.get("max_tokens").is_none(),
            "capability-enabled request must not emit max_tokens"
        );
    }

    /// Regression test: some OpenAI-compatible providers emit `"tool_calls": null`
    /// in stream delta chunks instead of omitting the field or using `[]`.
    /// Before the fix this produced: `invalid type: null, expected a sequence`.
    #[test]
    fn delta_with_null_tool_calls_deserializes_as_empty_vec() {
        use super::deserialize_null_as_empty_vec;

        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Delta {
            content: Option<String>,
            #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
            tool_calls: Vec<super::DeltaToolCall>,
        }

        // Simulate the exact shape observed in the wild (gaebal-gajae repro 2026-04-09)
        let json = r#"{
            "content": "",
            "function_call": null,
            "refusal": null,
            "role": "assistant",
            "tool_calls": null
        }"#;
        let delta: Delta = serde_json::from_str(json)
            .expect("delta with tool_calls:null must deserialize without error");
        assert!(
            delta.tool_calls.is_empty(),
            "tool_calls:null must produce an empty vec, not an error"
        );
    }

    /// Regression: when building a multi-turn request where a prior assistant
    /// turn has no tool calls, the serialized assistant message must NOT include
    /// `tool_calls: []`. Some providers reject requests that carry an empty
    /// `tool_calls` array on assistant turns (gaebal-gajae repro 2026-04-09).
    #[test]
    fn assistant_message_without_tool_calls_omits_tool_calls_field() {
        use crate::types::{InputContentBlock, InputMessage};

        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            }],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        let messages = payload["messages"].as_array().unwrap();
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message must be present");
        assert!(
            assistant_msg.get("tool_calls").is_none(),
            "assistant message without tool calls must omit tool_calls field: {assistant_msg:?}"
        );
    }

    /// Regression: assistant messages WITH tool calls must still include
    /// the `tool_calls` array (normal multi-turn tool-use flow).
    #[test]
    fn assistant_message_with_tool_calls_includes_tool_calls_field() {
        use crate::types::{InputContentBlock, InputMessage};

        let request = MessageRequest {
            model: "gpt-4o".to_string(),
            max_tokens: 100,
            messages: vec![
                InputMessage {
                    role: "assistant".to_string(),
                    content: vec![InputContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "/tmp/test"}),
                    }],
                },
                InputMessage::user_tool_result("call_1", r#"{"ok":true}"#, false),
            ],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        let messages = payload["messages"].as_array().unwrap();
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message must be present");
        let tool_calls = assistant_msg
            .get("tool_calls")
            .expect("assistant message with tool calls must include tool_calls field");
        assert!(tool_calls.is_array());
        assert_eq!(tool_calls.as_array().unwrap().len(), 1);
    }

    /// Regression: some OpenAI-compatible backends return `tool_calls: null`
    /// in non-stream chat completion responses. We must treat it as `[]`.
    #[test]
    fn non_stream_message_with_null_tool_calls_deserializes_as_empty_vec() {
        let payload = r#"{
          "id":"resp_test",
          "object":"chat.completion",
          "created":1778684339,
          "model":"gpt-5.5",
          "choices":[
            {
              "index":0,
              "message":{
                "role":"assistant",
                "content":"[]",
                "tool_calls":null
              },
              "finish_reason":"stop"
            }
          ],
          "usage":{"prompt_tokens":12,"completion_tokens":3}
        }"#;

        let parsed: ChatCompletionResponse =
            serde_json::from_str(payload).expect("non-stream tool_calls:null should parse");
        assert_eq!(parsed.choices.len(), 1);
        assert!(
            parsed.choices[0].message.tool_calls.is_empty(),
            "tool_calls:null must deserialize to empty vec"
        );
    }

    /// Orphaned tool messages (no preceding assistant `tool_calls`) must be
    /// dropped by the request-builder sanitizer. Regression for the second
    /// layer of the tool-pairing invariant fix (gaebal-gajae 2026-04-10).
    #[test]
    fn sanitize_drops_orphaned_tool_messages() {
        use super::sanitize_tool_message_pairing;

        // Valid pair: assistant with tool_calls → tool result
        let valid = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "search", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_1", "content": "result"}),
        ];
        let out = sanitize_tool_message_pairing(valid);
        assert_eq!(out.len(), 2, "valid pair must be preserved");

        // Orphaned tool message: no preceding assistant tool_calls
        let orphaned = vec![
            json!({"role": "assistant", "content": "hi"}),
            json!({"role": "tool", "tool_call_id": "call_2", "content": "orphaned"}),
        ];
        let out = sanitize_tool_message_pairing(orphaned);
        assert_eq!(out.len(), 1, "orphaned tool message must be dropped");
        assert_eq!(out[0]["role"], json!("assistant"));

        // Orphaned tool message after a non-assistant turn must also be dropped.
        let orphaned_after_user = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "tool", "tool_call_id": "call_user_orphan", "content": "orphaned"}),
        ];
        let out = sanitize_tool_message_pairing(orphaned_after_user);
        assert_eq!(
            out.len(),
            1,
            "tool message after user/system must be dropped"
        );
        assert_eq!(out[0]["role"], json!("user"));

        // Mismatched tool_call_id
        let mismatched = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_3", "type": "function", "function": {"name": "f", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_WRONG", "content": "bad"}),
        ];
        let out = sanitize_tool_message_pairing(mismatched);
        assert_eq!(
            out.len(),
            0,
            "broken assistant/tool segment must be dropped"
        );

        // Two tool results both valid (same preceding assistant)
        let two_results = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [
                {"id": "call_a", "type": "function", "function": {"name": "fa", "arguments": "{}"}},
                {"id": "call_b", "type": "function", "function": {"name": "fb", "arguments": "{}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "call_a", "content": "ra"}),
            json!({"role": "tool", "tool_call_id": "call_b", "content": "rb"}),
        ];
        let out = sanitize_tool_message_pairing(two_results);
        assert_eq!(out.len(), 3, "both valid tool results must be preserved");

        // Dangling assistant tool call (missing tool output): drop the broken segment.
        let dangling = vec![
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "call_missing", "type": "function", "function": {"name": "f", "arguments": "{}"}}]}),
            json!({"role": "user", "content": "next turn"}),
        ];
        let out = sanitize_tool_message_pairing(dangling);
        assert_eq!(out.len(), 1, "dangling assistant tool call must be dropped");
        assert_eq!(out[0]["role"], json!("user"));
    }

    #[test]
    fn unconfigured_model_uses_max_tokens() {
        // Default behavior remains `max_tokens`; no model-name prefix is enough
        // to switch request shape without capability config.
        let request = MessageRequest {
            model: "unconfigured-model".to_string(),
            max_tokens: 512,
            messages: vec![],
            stream: false,
            ..Default::default()
        };
        let payload = build_chat_completion_request(
            &request,
            OpenAiCompatConfig::openai(),
            DEFAULT_OPENAI_BASE_URL,
        );
        assert_eq!(payload["max_tokens"], json!(512));
        assert!(
            payload.get("max_completion_tokens").is_none(),
            "unconfigured models must not emit max_completion_tokens"
        );
    }

    #[test]
    fn explicit_unsupported_reasoning_parameter_is_removed_and_learned() {
        let client = OpenAiCompatClient::new("test-capability-key", OpenAiCompatConfig::openai())
            .with_base_url("https://capability.example/v1");
        let mut request = MessageRequest {
            model: "capability-model".to_string(),
            reasoning_effort: Some("high".to_string()),
            ..Default::default()
        };
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: Some("invalid_request_error".to_string()),
            message: Some("Unsupported parameter: reasoning_effort".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            retry_after: None,
        };
        assert!(client.adjust_request_after_capability_error(&mut request, &error, false));
        assert!(request.reasoning_effort.is_none());

        let prepared = client.prepare_request(
            &MessageRequest {
                model: "capability-model".to_string(),
                reasoning_effort: Some("high".to_string()),
                ..Default::default()
            },
            false,
        );
        assert!(prepared.reasoning_effort.is_none());
    }

    #[test]
    fn transient_errors_never_downgrade_capabilities() {
        let client = OpenAiCompatClient::new("test-transient-key", OpenAiCompatConfig::openai())
            .with_base_url("https://transient.example/v1");
        let mut request = MessageRequest {
            model: "transient-model".to_string(),
            reasoning_effort: Some("high".to_string()),
            ..Default::default()
        };
        let error = ApiError::Api {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            error_type: Some("rate_limit".to_string()),
            message: Some("reasoning_effort request was rate limited".to_string()),
            request_id: None,
            body: String::new(),
            retryable: true,
            retry_after: None,
        };
        assert!(!client.adjust_request_after_capability_error(&mut request, &error, false));
        assert_eq!(request.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn explicit_unsupported_thinking_transport_is_removed_and_learned() {
        let client =
            OpenAiCompatClient::new("test-thinking-transport-key", OpenAiCompatConfig::openai())
                .with_base_url("https://thinking-transport.example/v1");
        let mut request = MessageRequest {
            model: "thinking-transport-model".to_string(),
            extra_body: Some(serde_json::Map::from_iter([(
                "enable_thinking".to_string(),
                json!(true),
            )])),
            ..Default::default()
        };
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: None,
            message: Some("Unsupported parameter: enable_thinking".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            retry_after: None,
        };
        assert!(client.adjust_request_after_capability_error(&mut request, &error, false));
        assert!(request
            .extra_body
            .as_ref()
            .is_none_or(|body| !body.contains_key("enable_thinking")));

        let prepared = client.prepare_request(
            &MessageRequest {
                model: "thinking-transport-model".to_string(),
                extra_body: Some(serde_json::Map::from_iter([(
                    "enable_thinking".to_string(),
                    json!(true),
                )])),
                ..Default::default()
            },
            true,
        );
        assert!(prepared
            .extra_body
            .as_ref()
            .is_none_or(|body| !body.contains_key("enable_thinking")));
    }
}
