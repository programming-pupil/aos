use std::env::VarError;
use std::fmt::{Display, Formatter};
use std::time::Duration;

const GENERIC_FATAL_WRAPPER_MARKERS: &[&str] = &[
    "something went wrong while processing your request",
    "please try again, or use /new to start a fresh session",
];

const CONTEXT_WINDOW_ERROR_MARKERS: &[&str] = &[
    "maximum context length",
    "context window",
    "context length",
    "too many tokens",
    "prompt is too long",
    "input is too long",
    "request is too large",
];

pub enum ApiError {
    MissingCredentials {
        provider: &'static str,
        env_vars: &'static [&'static str],
        /// Optional, runtime-computed hint appended to the error Display
        /// output. Populated when the provider resolver can infer what the
        /// user probably intended (e.g. an `OpenAI` key is set but Anthropic
        /// was selected because no Anthropic credentials exist).
        hint: Option<String>,
    },
    ContextWindowExceeded {
        model: String,
        estimated_input_tokens: u32,
        requested_output_tokens: u32,
        estimated_total_tokens: u32,
        context_window_tokens: u32,
    },
    ExpiredOAuthToken,
    Auth(String),
    InvalidApiKeyEnv(VarError),
    Http(reqwest::Error),
    Io(std::io::Error),
    Json {
        provider: String,
        model: String,
        body_snippet: String,
        source: serde_json::Error,
    },
    Api {
        status: reqwest::StatusCode,
        error_type: Option<String>,
        message: Option<String>,
        request_id: Option<String>,
        body: String,
        retryable: bool,
        /// Parsed `Retry-After` header value for 429 responses. When present,
        /// callers should prefer this over the computed exponential backoff so
        /// we honor the server-provided rate-limit hint (matches Claude Code).
        retry_after: Option<Duration>,
    },
    RetriesExhausted {
        attempts: u32,
        last_error: Box<ApiError>,
    },
    InvalidSseFrame(&'static str),
    BackoffOverflow {
        attempt: u32,
        base_delay: Duration,
    },
}

impl ApiError {
    #[must_use]
    pub const fn missing_credentials(
        provider: &'static str,
        env_vars: &'static [&'static str],
    ) -> Self {
        Self::MissingCredentials {
            provider,
            env_vars,
            hint: None,
        }
    }

    /// Build a `MissingCredentials` error carrying an extra, runtime-computed
    /// hint string that the Display impl appends after the canonical "missing
    /// <provider> credentials" message. Used by the provider resolver to
    /// suggest the likely fix when the user has credentials for a different
    /// provider already in the environment.
    #[must_use]
    pub fn missing_credentials_with_hint(
        provider: &'static str,
        env_vars: &'static [&'static str],
        hint: impl Into<String>,
    ) -> Self {
        Self::MissingCredentials {
            provider,
            env_vars,
            hint: Some(hint.into()),
        }
    }

    /// Build a `Self::Json` enriched with provider/model identity and a
    /// non-sensitive response-size summary.
    #[must_use]
    pub fn json_deserialize(
        provider: impl Into<String>,
        model: impl Into<String>,
        body: &str,
        source: serde_json::Error,
    ) -> Self {
        Self::Json {
            provider: provider.into(),
            model: model.into(),
            body_snippet: format!("[response body omitted; chars={}]", body.chars().count()),
            source,
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(error) => error.is_connect() || error.is_timeout() || error.is_request(),
            Self::Api { retryable, .. } => *retryable,
            Self::RetriesExhausted { last_error, .. } => last_error.is_retryable(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. } => false,
        }
    }

    /// Return the `Retry-After` delay if this error came from a 429 response
    /// that included a `retry-after` header. Callers should prefer this value
    /// over the computed backoff delay when it exists.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            Self::RetriesExhausted { last_error, .. } => last_error.retry_after(),
            _ => None,
        }
    }

    /// Returns the provider-advertised output-token ceiling when a request was
    /// rejected for exceeding it. The requested value itself is deliberately
    /// ignored so callers never "learn" the value that just failed.
    #[must_use]
    pub fn max_output_tokens_limit(&self) -> Option<u32> {
        match self {
            Self::Api {
                status,
                message,
                body,
                ..
            } if matches!(status.as_u16(), 400 | 413 | 422) => message
                .as_deref()
                .and_then(parse_output_token_limit)
                .or_else(|| parse_output_token_limit(body)),
            Self::RetriesExhausted { last_error, .. } => last_error.max_output_tokens_limit(),
            _ => None,
        }
    }

    /// Returns the provider-advertised context-window ceiling when present in
    /// a 4xx response. This feeds the endpoint-scoped capability cache so the
    /// next turn can compact before issuing another oversized request.
    #[must_use]
    pub fn context_window_tokens_limit(&self) -> Option<u32> {
        match self {
            Self::ContextWindowExceeded {
                context_window_tokens,
                ..
            } => Some(*context_window_tokens),
            Self::Api {
                status,
                message,
                body,
                ..
            } if matches!(status.as_u16(), 400 | 413 | 422) => message
                .as_deref()
                .and_then(parse_context_token_limit)
                .or_else(|| parse_context_token_limit(body)),
            Self::RetriesExhausted { last_error, .. } => last_error.context_window_tokens_limit(),
            _ => None,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Api { request_id, .. } => request_id.as_deref(),
            Self::RetriesExhausted { last_error, .. } => last_error.request_id(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. } => None,
        }
    }

    #[must_use]
    pub fn safe_failure_class(&self) -> &'static str {
        match self {
            Self::RetriesExhausted { .. } if self.is_context_window_failure() => "context_window",
            Self::RetriesExhausted { .. } if self.is_generic_fatal_wrapper() => {
                "provider_retry_exhausted"
            }
            Self::RetriesExhausted { last_error, .. } => last_error.safe_failure_class(),
            Self::MissingCredentials { .. } | Self::ExpiredOAuthToken | Self::Auth(_) => {
                "provider_auth"
            }
            Self::Api { status, .. } if matches!(status.as_u16(), 401 | 403) => "provider_auth",
            Self::ContextWindowExceeded { .. } => "context_window",
            Self::Api { .. } if self.is_context_window_failure() => "context_window",
            Self::Api { status, .. } if status.as_u16() == 429 => "provider_rate_limit",
            Self::Api { .. } if self.is_generic_fatal_wrapper() => "provider_internal",
            Self::Api { .. } => "provider_error",
            Self::Http(_) | Self::InvalidSseFrame(_) | Self::BackoffOverflow { .. } => {
                "provider_transport"
            }
            Self::InvalidApiKeyEnv(_) | Self::Io(_) | Self::Json { .. } => "runtime_io",
        }
    }

    #[must_use]
    pub fn is_generic_fatal_wrapper(&self) -> bool {
        match self {
            Self::Api { message, body, .. } => {
                message
                    .as_deref()
                    .is_some_and(looks_like_generic_fatal_wrapper)
                    || looks_like_generic_fatal_wrapper(body)
            }
            Self::RetriesExhausted { last_error, .. } => last_error.is_generic_fatal_wrapper(),
            Self::MissingCredentials { .. }
            | Self::ContextWindowExceeded { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. } => false,
        }
    }

    #[must_use]
    pub fn is_context_window_failure(&self) -> bool {
        match self {
            Self::ContextWindowExceeded { .. } => true,
            Self::Api {
                status,
                message,
                body,
                ..
            } => {
                matches!(status.as_u16(), 400 | 413 | 422)
                    && (message
                        .as_deref()
                        .is_some_and(looks_like_context_window_error)
                        || looks_like_context_window_error(body))
            }
            Self::RetriesExhausted { last_error, .. } => last_error.is_context_window_failure(),
            Self::MissingCredentials { .. }
            | Self::ExpiredOAuthToken
            | Self::Auth(_)
            | Self::InvalidApiKeyEnv(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Json { .. }
            | Self::InvalidSseFrame(_)
            | Self::BackoffOverflow { .. } => false,
        }
    }
}

impl Display for ApiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials {
                provider,
                env_vars,
                hint,
            } => {
                write!(
                    f,
                    "missing {provider} credentials; export {} before calling the {provider} API",
                    env_vars.join(" or ")
                )?;
                if cfg!(target_os = "windows") {
                    if let Some(primary) = env_vars.first() {
                        write!(
                            f,
                            " (on Windows, environment variables set in PowerShell only persist for the current session; use `setx {primary} <value>` to make it permanent, then open a new terminal, or place a `.env` file containing `{primary}=<value>` in the current working directory)"
                        )?;
                    } else {
                        write!(
                            f,
                            " (on Windows, environment variables set in PowerShell only persist for the current session; use `setx` to make them permanent, then open a new terminal, or place a `.env` file in the current working directory)"
                        )?;
                    }
                }
                if let Some(hint) = hint {
                    write!(f, " — hint: {hint}")?;
                }
                Ok(())
            }
            Self::ContextWindowExceeded {
                model,
                estimated_input_tokens,
                requested_output_tokens,
                estimated_total_tokens,
                context_window_tokens,
            } => write!(
                f,
                "context_window_blocked for {model}: estimated input {estimated_input_tokens} + requested output {requested_output_tokens} = {estimated_total_tokens} tokens exceeds the {context_window_tokens}-token context window; compact the session or reduce request size before retrying"
            ),
            Self::ExpiredOAuthToken => {
                write!(
                    f,
                    "saved OAuth token is expired and no refresh token is available"
                )
            }
            Self::Auth(message) => write!(
                f,
                "auth error: {}",
                runtime::protect_sensitive_text(message, runtime::configured_data_protection_mode())
                    .value
            ),
            Self::InvalidApiKeyEnv(error) => {
                write!(f, "failed to read credential environment variable: {error}")
            }
            Self::Http(error) => write!(
                f,
                "http error: {}",
                runtime::protect_sensitive_text(
                    &error.to_string(),
                    runtime::configured_data_protection_mode(),
                )
                .value
            ),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Json {
                provider,
                model,
                body_snippet,
                source,
            } => write!(
                f,
                "failed to parse {provider} response for model {model}: {source}; {body_snippet}"
            ),
            Self::Api {
                status,
                error_type,
                message,
                request_id,
                ..
            } => {
                if let (Some(error_type), Some(message)) = (error_type, message) {
                    write!(f, "api returned {status} ({error_type})")?;
                    if let Some(request_id) = request_id {
                        write!(f, " [trace {request_id}]")?;
                    }
                    let safe_message = runtime::protect_sensitive_text(
                        message,
                        runtime::configured_data_protection_mode(),
                    );
                    write!(f, ": {}", safe_message.value)
                } else {
                    write!(f, "api returned {status}")?;
                    if let Some(request_id) = request_id {
                        write!(f, " [trace {request_id}]")?;
                    }
                    write!(f, "; provider response body omitted")
                }
            }
            Self::RetriesExhausted {
                attempts,
                last_error,
            } => write!(f, "api failed after {attempts} attempts: {last_error}"),
            Self::InvalidSseFrame(message) => write!(f, "invalid sse frame: {message}"),
            Self::BackoffOverflow {
                attempt,
                base_delay,
            } => write!(
                f,
                "retry backoff overflowed on attempt {attempt} with base delay {base_delay:?}"
            ),
        }
    }
}

impl std::fmt::Debug for ApiError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiError")
            .field("failure_class", &self.safe_failure_class())
            .field("request_id", &self.request_id())
            .field("summary", &self.to_string())
            .finish()
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json {
            provider: "unknown".to_string(),
            model: "unknown".to_string(),
            body_snippet: String::new(),
            source: value,
        }
    }
}

impl From<VarError> for ApiError {
    fn from(value: VarError) -> Self {
        Self::InvalidApiKeyEnv(value)
    }
}

fn looks_like_generic_fatal_wrapper(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    GENERIC_FATAL_WRAPPER_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn looks_like_context_window_error(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    CONTEXT_WINDOW_ERROR_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn parse_output_token_limit(text: &str) -> Option<u32> {
    let normalized = normalize_token_error_text(text);

    for field in [
        "max_completion_tokens",
        "max_output_tokens",
        "max_tokens",
        "maximum output tokens",
        "output token limit",
    ] {
        let Some(field_offset) = normalized.find(field) else {
            continue;
        };
        let tail = &normalized[field_offset..normalized.len().min(field_offset + 240)];
        for marker in [
            "less than or equal to",
            "must be <=",
            "<=",
            "at most",
            "model limit",
            "limit is",
            "maximum is",
            "max is",
        ] {
            if let Some(marker_offset) = tail.find(marker) {
                if let Some(limit) = first_positive_u32(&tail[marker_offset + marker.len()..]) {
                    return Some(limit);
                }
            }
        }
    }

    if let Some(marker_offset) = normalized.find("supports at most") {
        let tail = &normalized[marker_offset + "supports at most".len()..];
        if tail
            .get(..tail.len().min(80))
            .is_some_and(|window| window.contains("output token"))
        {
            return first_positive_u32(tail);
        }
    }
    None
}

fn parse_context_token_limit(text: &str) -> Option<u32> {
    let normalized = normalize_token_error_text(text);
    for marker in [
        "maximum context length is",
        "maximum context length:",
        "context window limit is",
        "context window is",
        "context length limit is",
        "context length is",
    ] {
        if let Some(offset) = normalized.find(marker) {
            if let Some(limit) = first_positive_u32(&normalized[offset + marker.len()..]) {
                return Some(limit);
            }
        }
    }
    None
}

fn normalize_token_error_text(text: &str) -> String {
    text.to_ascii_lowercase().replace(',', "")
}

fn first_positive_u32(text: &str) -> Option<u32> {
    let start = text.find(|character: char| character.is_ascii_digit())?;
    let digits: String = text[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u32>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::ApiError;

    #[test]
    fn json_deserialize_error_includes_provider_model_but_omits_body() {
        let raw_body = format!("{}{}", "x".repeat(190), "_TAIL_PAST_200_CHARS_MARKER_");
        let source = serde_json::from_str::<serde_json::Value>("{not json")
            .expect_err("invalid json should fail to parse");

        let error = ApiError::json_deserialize("Anthropic", "claude-opus-4-6", &raw_body, source);
        let rendered = error.to_string();

        assert!(
            rendered.starts_with("failed to parse Anthropic response for model claude-opus-4-6: "),
            "rendered error should lead with provider and model: {rendered}"
        );
        assert!(rendered.contains(&format!(
            "response body omitted; chars={}",
            raw_body.chars().count()
        )));
        assert!(!rendered.contains(&"x".repeat(10)));
        assert!(!rendered.contains("_TAIL_PAST_200_CHARS_MARKER_"));
        assert_eq!(error.safe_failure_class(), "runtime_io");
        assert_eq!(error.request_id(), None);
        assert!(!error.is_retryable());
    }

    #[test]
    fn api_error_display_and_debug_never_expose_provider_secret_body() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_GATEWAY,
            error_type: None,
            message: None,
            request_id: Some("request-safe-id".to_string()),
            body: "api_key=sk-1234567890abcdef".to_string(),
            retryable: true,
            retry_after: None,
        };
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("sk-1234567890abcdef"));
            assert!(!rendered.contains("api_key="));
            assert!(rendered.contains("request-safe-id"));
        }
    }

    #[test]
    fn detects_generic_fatal_wrapper_and_classifies_it_as_provider_internal() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            error_type: Some("api_error".to_string()),
            message: Some(
                "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                    .to_string(),
            ),
            request_id: Some("req_jobdori_123".to_string()),
            body: String::new(),
            retryable: true,
            retry_after: None,
        };

        assert!(error.is_generic_fatal_wrapper());
        assert_eq!(error.safe_failure_class(), "provider_internal");
        assert_eq!(error.request_id(), Some("req_jobdori_123"));
        assert!(error.to_string().contains("[trace req_jobdori_123]"));
    }

    #[test]
    fn retries_exhausted_preserves_nested_request_id_and_failure_class() {
        let error = ApiError::RetriesExhausted {
            attempts: 3,
            last_error: Box::new(ApiError::Api {
                status: reqwest::StatusCode::BAD_GATEWAY,
                error_type: Some("api_error".to_string()),
                message: Some(
                    "Something went wrong while processing your request. Please try again, or use /new to start a fresh session."
                        .to_string(),
                ),
                request_id: Some("req_nested_456".to_string()),
                body: String::new(),
                retryable: true,
            retry_after: None,
            }),
        };

        assert!(error.is_generic_fatal_wrapper());
        assert_eq!(error.safe_failure_class(), "provider_retry_exhausted");
        assert_eq!(error.request_id(), Some("req_nested_456"));
    }

    #[test]
    fn classifies_provider_context_window_errors() {
        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: Some("invalid_request_error".to_string()),
            message: Some(
                "This model's maximum context length is 200000 tokens, but your request used 230000 tokens."
                    .to_string(),
            ),
            request_id: Some("req_ctx_123".to_string()),
            body: String::new(),
            retryable: false,
            retry_after: None,
        };

        assert!(error.is_context_window_failure());
        assert_eq!(error.safe_failure_class(), "context_window");
        assert_eq!(error.request_id(), Some("req_ctx_123"));
        assert_eq!(error.context_window_tokens_limit(), Some(200_000));
    }

    #[test]
    fn extracts_output_limit_without_confusing_requested_tokens() {
        for message in [
            "max_tokens must be less than or equal to 4096, but got 16384",
            "max_completion_tokens must be <= 8,192; requested 32768",
            "max_tokens 16384 exceeds model limit 4096",
            "This model supports at most 32768 output tokens",
        ] {
            let error = ApiError::Api {
                status: reqwest::StatusCode::BAD_REQUEST,
                error_type: Some("invalid_request_error".to_string()),
                message: Some(message.to_string()),
                request_id: None,
                body: String::new(),
                retryable: false,
                retry_after: None,
            };
            assert!(error.max_output_tokens_limit().is_some(), "{message}");
        }

        let error = ApiError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            error_type: None,
            message: Some("max_tokens 16384 exceeds model limit 4096".to_string()),
            request_id: None,
            body: String::new(),
            retryable: false,
            retry_after: None,
        };
        assert_eq!(error.max_output_tokens_limit(), Some(4096));
    }

    #[test]
    fn token_limit_extractors_recurse_through_retry_wrapper() {
        let error = ApiError::RetriesExhausted {
            attempts: 2,
            last_error: Box::new(ApiError::Api {
                status: reqwest::StatusCode::UNPROCESSABLE_ENTITY,
                error_type: None,
                message: None,
                request_id: None,
                body: r#"{"error":{"message":"context window limit is 32768 tokens"}}"#.to_string(),
                retryable: false,
                retry_after: None,
            }),
        };
        assert_eq!(error.context_window_tokens_limit(), Some(32_768));
        assert_eq!(error.max_output_tokens_limit(), None);
    }

    #[test]
    fn missing_credentials_without_hint_renders_the_canonical_message() {
        // given
        let error = ApiError::missing_credentials(
            "Anthropic",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
        );

        // when
        let rendered = error.to_string();

        // then
        assert!(
            rendered.starts_with(
                "missing Anthropic credentials; export ANTHROPIC_AUTH_TOKEN or ANTHROPIC_API_KEY before calling the Anthropic API"
            ),
            "rendered error should lead with the canonical missing-credential message: {rendered}"
        );
        assert!(
            !rendered.contains(" — hint: "),
            "no hint should be appended when none is supplied: {rendered}"
        );
    }

    #[test]
    fn missing_credentials_with_hint_appends_the_hint_after_base_message() {
        // given
        let error = ApiError::missing_credentials_with_hint(
            "Anthropic",
            &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
            "I see OPENAI_API_KEY is set — if you meant to use the OpenAI-compat provider, prefix your model name with `openai/` so prefix routing selects it.",
        );

        // when
        let rendered = error.to_string();

        // then
        assert!(
            rendered.starts_with("missing Anthropic credentials;"),
            "hint should be appended, not replace the base message: {rendered}"
        );
        let hint_marker = " — hint: I see OPENAI_API_KEY is set — if you meant to use the OpenAI-compat provider, prefix your model name with `openai/` so prefix routing selects it.";
        assert!(
            rendered.ends_with(hint_marker),
            "rendered error should end with the hint: {rendered}"
        );
        // Classification semantics are unaffected by the presence of a hint.
        assert_eq!(error.safe_failure_class(), "provider_auth");
        assert!(!error.is_retryable());
        assert_eq!(error.request_id(), None);
    }
}
