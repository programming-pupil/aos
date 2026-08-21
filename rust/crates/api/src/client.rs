use crate::error::ApiError;
use crate::prompt_cache::{PromptCache, PromptCacheRecord, PromptCacheStats};
use crate::providers::anthropic::{self, AnthropicClient, AuthSource};
use crate::providers::openai_compat::{self, OpenAiCompatClient, OpenAiCompatConfig};
use crate::providers::{self, ProviderKind};
use crate::types::{
    MessageRequest, MessageResponse, ResponsesCompactRequest, ResponsesCompactResult, StreamEvent,
    Usage,
};

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ProviderClient {
    Anthropic(AnthropicClient),
    Xai(OpenAiCompatClient),
    OpenAi(OpenAiCompatClient),
}

impl ProviderClient {
    pub fn from_model(model: &str) -> Result<Self, ApiError> {
        Self::from_model_with_anthropic_auth(model, None)
    }

    /// Create a `ProviderClient` with custom base URL for OpenAI-compatible providers.
    pub fn from_model_with_base_url(model: &str, base_url: &str) -> Result<Self, ApiError> {
        use crate::providers::anthropic::AnthropicClient;

        let resolved_model = providers::resolve_model_alias(model);
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::Anthropic => {
                // For Anthropic, custom base_url means proxy (e.g. NovitaAI, OpenRouter)
                let client = AnthropicClient::from_env()?.with_base_url(base_url);
                Ok(Self::Anthropic(client))
            }
            ProviderKind::Xai => {
                let api_key = std::env::var("XAI_API_KEY")
                    .map_err(|_| ApiError::missing_credentials("xAI", &["XAI_API_KEY"]))?;
                let mut client = OpenAiCompatClient::new(api_key, OpenAiCompatConfig::xai());
                client = client.with_base_url(base_url);
                Ok(Self::Xai(client))
            }
            ProviderKind::OpenAi => {
                let api_key = std::env::var("OPENAI_API_KEY")
                    .map_err(|_| ApiError::missing_credentials("OpenAI", &["OPENAI_API_KEY"]))?;
                let mut client = OpenAiCompatClient::new(api_key, OpenAiCompatConfig::openai());
                client = client.with_base_url(base_url);
                Ok(Self::OpenAi(client))
            }
        }
    }

    pub fn from_model_with_anthropic_auth(
        model: &str,
        anthropic_auth: Option<AuthSource>,
    ) -> Result<Self, ApiError> {
        let resolved_model = providers::resolve_model_alias(model);
        match providers::detect_provider_kind(&resolved_model) {
            ProviderKind::Anthropic => Ok(Self::Anthropic(match anthropic_auth {
                Some(auth) => AnthropicClient::from_auth(auth),
                None => AnthropicClient::from_env()?,
            })),
            ProviderKind::Xai => Ok(Self::Xai(OpenAiCompatClient::from_env(
                OpenAiCompatConfig::xai(),
            )?)),
            ProviderKind::OpenAi => {
                // DashScope models (qwen-*) also return ProviderKind::OpenAi because they
                // speak the OpenAI wire format, but they need the DashScope config which
                // reads DASHSCOPE_API_KEY and points at dashscope.aliyuncs.com.
                let config = match providers::metadata_for_model(&resolved_model) {
                    Some(meta) if meta.auth_env == "DASHSCOPE_API_KEY" => {
                        OpenAiCompatConfig::dashscope()
                    }
                    _ => OpenAiCompatConfig::openai(),
                };
                Ok(Self::OpenAi(OpenAiCompatClient::from_env(config)?))
            }
        }
    }
}

/// Build a `ProviderClient` using explicit provider, model, and pre-decrypted API key.
///
/// This is the canonical entry point for any code path that resolves an API key from the
/// database (rather than environment variables). It bypasses `detect_provider_kind` so that
/// the presence of env vars (e.g. `OPENAI_API_KEY`) cannot incorrectly override a
/// user-configured custom proxy or third-party endpoint.
///
/// Provider semantics:
/// - `anthropic`: native Anthropic API (`/v1/messages`), no custom base URL.
/// - `openai`: OpenAI-compatible (`/chat/completions`), uses official endpoint by default.
/// - `custom`: OpenAI-compatible proxy (e.g. `OpenRouter`, `NovitaAI`, generic third-party),
///   requires a `base_url`. The caller must provide it; otherwise a helpful error is returned.
///
/// When `base_url` is set, OpenAI-compatible format is always used — regardless of the
/// provider string. This handles the edge case where a user configured a third-party
/// proxy but selected the wrong provider in the UI.
pub fn build_provider(
    provider: &str,
    model: &str,
    api_key: &str,
    base_url: Option<&str>,
) -> Result<ProviderClient, ApiError> {
    let resolved_model = providers::resolve_model_alias(model);

    match (provider, base_url) {
        // Native Anthropic API (no custom endpoint).
        ("anthropic", None) => {
            let client = AnthropicClient::new(api_key);
            Ok(ProviderClient::Anthropic(client))
        }
        // Custom proxy or third-party: always OpenAI-compatible format.
        // The `custom` provider is the canonical UI choice for this.
        ("custom", _) | (_, Some(_)) => {
            let config = match providers::metadata_for_model(&resolved_model) {
                Some(meta) if meta.auth_env == "DASHSCOPE_API_KEY" => {
                    OpenAiCompatConfig::dashscope()
                }
                _ => OpenAiCompatConfig::openai(),
            };
            let mut client = OpenAiCompatClient::new(api_key, config);
            if let Some(url) = base_url {
                client = client.with_base_url(url.trim_end_matches('/'));
            }
            Ok(ProviderClient::OpenAi(client))
        }
        // `openai` without base_url — official OpenAI endpoint.
        // Legacy unknown providers without base_url — default to OpenAI-compatible.
        _ => Ok(ProviderClient::OpenAi(OpenAiCompatClient::new(
            api_key,
            OpenAiCompatConfig::openai(),
        ))),
    }
}

impl ProviderClient {
    #[must_use]
    pub const fn provider_kind(&self) -> ProviderKind {
        match self {
            Self::Anthropic(_) => ProviderKind::Anthropic,
            Self::Xai(_) => ProviderKind::Xai,
            Self::OpenAi(_) => ProviderKind::OpenAi,
        }
    }

    #[must_use]
    pub fn with_prompt_cache(self, prompt_cache: PromptCache) -> Self {
        match self {
            Self::Anthropic(client) => Self::Anthropic(client.with_prompt_cache(prompt_cache)),
            other => other,
        }
    }

    /// Apply authoritative token ceilings to OpenAI-compatible clients.
    /// Anthropic keeps its native model/count-token preflight behavior.
    #[must_use]
    pub fn with_token_limits(
        self,
        context_window_tokens: Option<u32>,
        max_output_tokens: Option<u32>,
    ) -> Self {
        match self {
            Self::Xai(client) => {
                Self::Xai(client.with_token_limits(context_window_tokens, max_output_tokens))
            }
            Self::OpenAi(client) => {
                Self::OpenAi(client.with_token_limits(context_window_tokens, max_output_tokens))
            }
            anthropic @ Self::Anthropic(_) => anthropic,
        }
    }

    #[must_use]
    pub fn prompt_cache_stats(&self) -> Option<PromptCacheStats> {
        match self {
            Self::Anthropic(client) => client.prompt_cache_stats(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    #[must_use]
    pub fn take_last_prompt_cache_record(&self) -> Option<PromptCacheRecord> {
        match self {
            Self::Anthropic(client) => client.take_last_prompt_cache_record(),
            Self::Xai(_) | Self::OpenAi(_) => None,
        }
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        match self {
            Self::Anthropic(client) => client.base_url(),
            Self::Xai(client) | Self::OpenAi(client) => client.base_url(),
        }
    }

    #[must_use]
    pub fn output_token_ceiling(&self, model: &str) -> Option<u32> {
        match self {
            Self::Anthropic(_) => {
                providers::model_token_limit(model).map(|limit| limit.max_output_tokens)
            }
            Self::Xai(client) | Self::OpenAi(client) => client.output_token_ceiling(model),
        }
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let (request, protection) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_outbound_protection(self.provider_kind(), &request.model, &protection);
        let response = match self {
            Self::Anthropic(client) => client.send_message(&request).await,
            Self::Xai(client) | Self::OpenAi(client) => client.send_message(&request).await,
        }?;
        let (response, inbound_protection) =
            response.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_inbound_protection(self.provider_kind(), &response.model, &inbound_protection);
        Ok(response)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let (request, protection) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_outbound_protection(self.provider_kind(), &request.model, &protection);
        match self {
            Self::Anthropic(client) => client
                .stream_message(&request)
                .await
                .map(MessageStream::Anthropic),
            Self::Xai(client) | Self::OpenAi(client) => client
                .stream_message(&request)
                .await
                .map(MessageStream::OpenAiCompat),
        }
    }

    pub async fn send_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let (request, protection) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_outbound_protection(self.provider_kind(), &request.model, &protection);
        match self {
            Self::OpenAi(client) => client.send_responses_web_search_message(&request).await,
            Self::Anthropic(_) | Self::Xai(_) => Err(ApiError::InvalidSseFrame(
                "responses web_search is only available for OpenAI-compatible providers",
            )),
        }
    }

    pub async fn stream_responses_web_search_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let (request, protection) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_outbound_protection(self.provider_kind(), &request.model, &protection);
        match self {
            Self::OpenAi(client) => client
                .stream_responses_web_search_message(&request)
                .await
                .map(MessageStream::OpenAiResponses),
            Self::Anthropic(_) | Self::Xai(_) => Err(ApiError::InvalidSseFrame(
                "responses web_search streaming is only available for OpenAI-compatible providers",
            )),
        }
    }

    /// Call the OpenAI-compatible `/responses/compact` endpoint. Anthropic and
    /// xAI variants fail explicitly rather than falling back to a summary
    /// prompt, because that fallback has different semantics.
    pub async fn compact_responses(
        &self,
        request: &ResponsesCompactRequest,
    ) -> Result<ResponsesCompactResult, ApiError> {
        let (request, protection) =
            request.protect_sensitive_content(runtime::configured_data_protection_mode());
        log_outbound_protection(self.provider_kind(), &request.model, &protection);
        match self {
            Self::OpenAi(client) => client.compact_responses(&request).await,
            Self::Anthropic(_) | Self::Xai(_) => Err(ApiError::InvalidSseFrame(
                "responses compact is only available for OpenAI-compatible providers",
            )),
        }
    }

    #[must_use]
    pub const fn supports_responses_compact_v1(&self) -> bool {
        matches!(self, Self::OpenAi(_))
    }
}

fn log_outbound_protection(
    provider: ProviderKind,
    model: &str,
    report: &runtime::DataProtectionReport,
) {
    if report.redacted {
        tracing::warn!(
            provider_kind = ?provider,
            model,
            finding_count = report.finding_count,
            categories = ?report.categories,
            "model outbound data protection redacted sensitive values"
        );
    }
}

fn log_inbound_protection(
    provider: ProviderKind,
    model: &str,
    report: &runtime::DataProtectionReport,
) {
    if report.redacted {
        tracing::warn!(
            provider_kind = ?provider,
            model,
            finding_count = report.finding_count,
            categories = ?report.categories,
            "model inbound data protection redacted sensitive values"
        );
    }
}

#[derive(Debug)]
pub enum MessageStream {
    Anthropic(anthropic::MessageStream),
    OpenAiCompat(openai_compat::MessageStream),
    OpenAiResponses(openai_compat::ResponsesMessageStream),
}

impl MessageStream {
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Anthropic(stream) => stream.request_id(),
            Self::OpenAiCompat(stream) => stream.request_id(),
            Self::OpenAiResponses(stream) => stream.request_id(),
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        let event = match self {
            Self::Anthropic(stream) => stream.next_event().await,
            Self::OpenAiCompat(stream) => stream.next_event().await,
            Self::OpenAiResponses(stream) => stream.next_event().await,
        }?;
        let Some(event) = event else {
            return Ok(None);
        };
        let (event, protection) =
            event.protect_sensitive_content(runtime::configured_data_protection_mode());
        if protection.redacted {
            tracing::warn!(
                finding_count = protection.finding_count,
                categories = ?protection.categories,
                "streaming model response data protection redacted sensitive values"
            );
        }
        Ok(Some(event))
    }

    /// Returns token usage summary after the stream has been fully consumed.
    /// Call this once all events have been read.
    pub fn usage_summary(&mut self) -> Option<Usage> {
        match self {
            Self::Anthropic(stream) => stream.usage_summary(),
            Self::OpenAiCompat(_) | Self::OpenAiResponses(_) => None,
        }
    }

    #[must_use]
    pub fn provider_metadata(&self) -> Option<serde_json::Value> {
        match self {
            Self::OpenAiResponses(stream) => stream.provider_metadata(),
            Self::Anthropic(_) | Self::OpenAiCompat(_) => None,
        }
    }
}

pub use anthropic::{
    oauth_token_is_expired, resolve_saved_oauth_token, resolve_startup_auth_source, OAuthTokenSet,
};
#[must_use]
pub fn read_base_url() -> String {
    anthropic::read_base_url()
}

#[must_use]
pub fn read_xai_base_url() -> String {
    openai_compat::read_base_url(OpenAiCompatConfig::xai())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::ProviderClient;
    use crate::providers::{detect_provider_kind, resolve_model_alias, ProviderKind};

    /// Serializes every test in this module that mutates process-wide
    /// environment variables so concurrent test threads cannot observe
    /// each other's partially-applied state.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn resolves_existing_and_grok_aliases() {
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-8");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
    }

    #[test]
    fn provider_detection_prefers_model_family() {
        assert_eq!(detect_provider_kind("grok-3"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::Anthropic
        );
    }

    /// Snapshot-restore guard for a single environment variable. Mirrors
    /// the pattern used in `providers/mod.rs` tests: captures the original
    /// value on construction, applies the override, and restores on drop so
    /// tests leave the process env untouched even when they panic.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn dashscope_model_uses_dashscope_config_not_openai() {
        // Regression: qwen-plus was being routed to OpenAiCompatConfig::openai()
        // which reads OPENAI_API_KEY and points at api.openai.com, when it should
        // use OpenAiCompatConfig::dashscope() which reads DASHSCOPE_API_KEY and
        // points at dashscope.aliyuncs.com.
        let _lock = env_lock();
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("test-dashscope-key"));
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);

        let client = ProviderClient::from_model("qwen-plus");

        // Must succeed (not fail with "missing OPENAI_API_KEY")
        assert!(
            client.is_ok(),
            "qwen-plus with DASHSCOPE_API_KEY set should build successfully, got: {:?}",
            client.err()
        );

        // Verify it's the OpenAi variant pointed at the DashScope base URL.
        match client.unwrap() {
            ProviderClient::OpenAi(openai_client) => {
                assert!(
                    openai_client.base_url().contains("dashscope.aliyuncs.com"),
                    "qwen-plus should route to DashScope base URL (contains 'dashscope.aliyuncs.com'), got: {}",
                    openai_client.base_url()
                );
            }
            other => panic!("Expected ProviderClient::OpenAi for qwen-plus, got: {other:?}"),
        }
    }
}
