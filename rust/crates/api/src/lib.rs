mod client;
mod error;
mod http_client;
mod model_profile;
mod prompt_cache;
mod providers;
mod sse;
mod types;

pub use client::build_provider;
pub use client::{
    oauth_token_is_expired, read_base_url, read_xai_base_url, resolve_saved_oauth_token,
    resolve_startup_auth_source, MessageStream, OAuthTokenSet, ProviderClient,
};
pub use error::ApiError;
pub use http_client::{
    build_http_client, build_http_client_or_default, build_http_client_with,
    build_http_client_with_opts, ProxyConfig, TimeoutConfig,
};
pub use model_profile::{
    infer_model_profile, ModelFeatureProfile, ModelProfile, ModelProtocol, ReasoningProfile,
    MODEL_CAPABILITY_REGISTRY_VERSION, MODEL_CAPABILITY_SCHEMA_VERSION,
};
pub use prompt_cache::{
    CacheBreakEvent, PromptCache, PromptCacheConfig, PromptCachePaths, PromptCacheRecord,
    PromptCacheStats,
};
pub use providers::anthropic::{AnthropicClient, AnthropicClient as ApiClient, AuthSource};
pub use providers::openai_compat::{
    chat_completions_endpoint, embeddings_endpoint, images_generations_endpoint,
    supports_official_deepseek_responses_web_search,
    supports_official_deepseek_v4_thinking_control, OpenAiCompatClient, OpenAiCompatConfig,
};
pub use providers::{
    detect_provider_kind, max_tokens_for_model, max_tokens_for_model_with_override,
    metadata_for_model, model_capabilities, model_token_limit, resolve_model_alias,
    ModelCapabilities, ModelCapabilitiesSource, ModelCapabilityOverride, ModelTokenLimit,
    ProviderKind,
};
pub use sse::{parse_frame, SseParser};
pub use types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    ImageSourceType, InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent,
    MessageRequest, MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock,
    StreamEvent, ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

pub use telemetry::{
    AnalyticsEvent, AnthropicRequestProfile, ClientIdentity, JsonlTelemetrySink,
    MemoryTelemetrySink, SessionTraceRecord, SessionTracer, TelemetryEvent, TelemetrySink,
    DEFAULT_ANTHROPIC_VERSION,
};
