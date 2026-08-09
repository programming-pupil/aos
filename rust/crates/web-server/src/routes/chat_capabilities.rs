use api::ProviderKind;
use axum::{
    extract::{Extension, Query, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::auth::Claims;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatCapabilityResponse {
    reasoning: ReasoningCapability,
    model: ModelCapabilityInfo,
    search: SearchCapability,
    file_context: FileContextCapability,
    streaming: StreamingCapability,
    file_rag: FileRagCapability,
    multimodal: MultimodalCapability,
    memory: MemoryCapability,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningCapability {
    default_budget: &'static str,
    user_selectable: bool,
    supports_reasoning_effort: bool,
    message: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelCapabilityInfo {
    name: String,
    context_window_tokens: u32,
    max_output_tokens: u32,
    source: &'static str,
    conservative_fallback: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchCapability {
    enabled: bool,
    default_mode: &'static str,
    current_provider: Option<String>,
    providers: Vec<SearchProviderStatus>,
    missing_reason: Option<String>,
    builtin: bool,
    native: bool,
    mcp: bool,
    configured_providers: usize,
    rag_local: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchProviderStatus {
    provider: String,
    configured: bool,
    source: String,
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileContextCapability {
    enabled: bool,
    strict_grounding: bool,
    supported_media_types: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamingCapability {
    token_delta: bool,
    fallback_typewriter: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileRagCapability {
    enabled: bool,
    supported_types: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MultimodalCapability {
    native_vision: bool,
    image_summary_fallback: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryCapability {
    enabled: bool,
    default_mode: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatCapabilityQuery {
    model: Option<String>,
}

fn current_search_provider(providers: &[SearchProviderStatus]) -> Option<String> {
    ["db", "builtin", "model", "mcp"]
        .iter()
        .find_map(|source| {
            providers
                .iter()
                .find(|provider| provider.configured && provider.source == *source)
        })
        .map(|provider| provider.provider.clone())
}

fn model_supports_reasoning_effort(model: &str) -> bool {
    let provider = api::metadata_for_model(model)
        .map(|metadata| metadata.provider)
        .unwrap_or_else(|| api::detect_provider_kind(model));
    match provider {
        ProviderKind::Anthropic => true,
        ProviderKind::Xai => false,
        ProviderKind::OpenAi => {
            let model_lower = model
                .trim()
                .strip_prefix("openai/")
                .unwrap_or_else(|| model.trim())
                .to_ascii_lowercase();
            model_lower.starts_with("o3")
                || model_lower.starts_with("o4")
                || model_lower.starts_with("o1")
                || model_lower.starts_with("o-")
        }
    }
}

fn capability_reasoning_effort(value: Option<&Value>) -> Option<bool> {
    let value = value?;
    [
        "reasoningEffort",
        "reasoning_effort",
        "supportsReasoningEffort",
        "supports_reasoning_effort",
        "reasoning",
    ]
    .iter()
    .find_map(|key| match value.get(*key) {
        Some(Value::Bool(v)) => Some(*v),
        Some(Value::String(v)) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

fn capability_u32(value: Option<&Value>, keys: &[&str]) -> Option<u32> {
    let value = value?;
    keys.iter().find_map(|key| {
        let raw = value.get(*key)?.as_u64()?;
        u32::try_from(raw).ok()
    })
}

fn capability_token_override(value: Option<&Value>) -> Option<api::ModelCapabilityOverride> {
    let context_window_tokens = capability_u32(
        value,
        &[
            "contextWindowTokens",
            "context_window_tokens",
            "contextWindow",
            "context_window",
        ],
    );
    let max_output_tokens = capability_u32(
        value,
        &[
            "maxOutputTokens",
            "max_output_tokens",
            "maxOutput",
            "max_output",
        ],
    );
    if context_window_tokens.is_none() && max_output_tokens.is_none() {
        None
    } else {
        Some(api::ModelCapabilityOverride {
            context_window_tokens,
            max_output_tokens,
        })
    }
}

fn model_capability_source_label(source: api::ModelCapabilitiesSource) -> &'static str {
    match source {
        api::ModelCapabilitiesSource::ManualOverride => "manual_override",
        api::ModelCapabilitiesSource::BuiltInRegistry => "built_in_registry",
        api::ModelCapabilitiesSource::ConservativeFallback => "conservative_fallback",
    }
}

async fn configured_model_capability_override(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> Option<api::ModelCapabilityOverride> {
    configured_model_capabilities_json(state, tenant_id, model)
        .await
        .and_then(|value| capability_token_override(Some(&value)))
}

async fn configured_model_supports_reasoning_effort(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> Option<bool> {
    configured_model_capabilities_json(state, tenant_id, model)
        .await
        .and_then(|value| capability_reasoning_effort(Some(&value)))
}

async fn configured_model_capabilities_json(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> Option<Value> {
    let rows = sqlx::query(
        r#"
        SELECT CAST(capabilities_json AS TEXT) AS capabilities_json
        FROM api_keys
        WHERE tenant_id = ?
          AND enabled = 1
          AND model_type = 'chat'
          AND model = ?
        ORDER BY priority ASC, created_at ASC
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .bind(model)
    .fetch_all(&state.db)
    .await
    .ok()?;

    rows.into_iter().find_map(|row| {
        let raw: Option<String> = sqlx::Row::get(&row, "capabilities_json");
        raw.and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .filter(Value::is_object)
    })
}

async fn get_chat_capabilities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ChatCapabilityQuery>,
) -> impl IntoResponse {
    let selected_model = query
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&state.default_model);
    let search_snapshot =
        crate::routes::search_orchestrator_runtime::build_unified_search_capability_snapshot(
            &state,
            &claims.tenant_id,
            Some(selected_model),
            false,
            true,
        )
        .await;
    let mut providers = Vec::new();
    providers.push(SearchProviderStatus {
        provider: "aos_builtin_web_search".to_string(),
        configured: search_snapshot.builtin_web_search.available,
        source: "builtin".to_string(),
        detail: search_snapshot.builtin_web_search.detail.clone(),
    });
    providers.push(SearchProviderStatus {
        provider: "model_native_search".to_string(),
        configured: search_snapshot.native_search.available,
        source: "model".to_string(),
        detail: search_snapshot.native_search.detail.clone(),
    });
    providers.push(SearchProviderStatus {
        provider: "mcp_search".to_string(),
        configured: search_snapshot.mcp_search.available,
        source: "mcp".to_string(),
        detail: search_snapshot.mcp_search.detail.clone(),
    });
    providers.extend(search_snapshot.configured_providers.iter().map(|provider| {
        SearchProviderStatus {
            provider: provider.provider_type.clone(),
            configured: provider.enabled && provider.health_status != "unhealthy",
            source: "db".to_string(),
            detail: format!(
                "{} ({}) priority {}",
                provider.name, provider.health_status, provider.priority
            ),
        }
    }));
    let native_search = search_snapshot.native_search.available;
    let builtin_search = search_snapshot.builtin_web_search.available;
    let mcp_available = search_snapshot.mcp_search.available;
    let configured_provider_count = search_snapshot
        .configured_providers
        .iter()
        .filter(|provider| provider.enabled && provider.health_status != "unhealthy")
        .count();
    let search_enabled =
        builtin_search || native_search || mcp_available || configured_provider_count > 0;
    let current_provider = current_search_provider(&providers);
    let missing_reason = (!search_enabled).then(|| {
        search_snapshot
            .degraded_reason
            .clone()
            .unwrap_or_else(|| "No AOS built-in, model-native, MCP, or configured Search Extension path is available".to_string())
    });

    let supports_reasoning_effort =
        configured_model_supports_reasoning_effort(&state, &claims.tenant_id, selected_model)
            .await
            .unwrap_or_else(|| model_supports_reasoning_effort(selected_model));
    let token_override =
        configured_model_capability_override(&state, &claims.tenant_id, selected_model).await;
    let model_capabilities = api::model_capabilities(selected_model, token_override);

    Json(ChatCapabilityResponse {
        reasoning: ReasoningCapability {
            default_budget: "adaptive_deep",
            user_selectable: false,
            supports_reasoning_effort,
            message: "AOS automatically uses deeper reasoning for complex turns.",
        },
        model: ModelCapabilityInfo {
            name: api::resolve_model_alias(selected_model),
            context_window_tokens: model_capabilities.context_window_tokens,
            max_output_tokens: model_capabilities.max_output_tokens,
            source: model_capability_source_label(model_capabilities.source),
            conservative_fallback: model_capabilities.source
                == api::ModelCapabilitiesSource::ConservativeFallback,
        },
        search: SearchCapability {
            enabled: search_enabled,
            default_mode: "off",
            current_provider,
            providers,
            missing_reason,
            builtin: builtin_search,
            native: native_search,
            mcp: mcp_available,
            configured_providers: configured_provider_count,
            rag_local: true,
        },
        file_context: FileContextCapability {
            enabled: true,
            strict_grounding: true,
            supported_media_types: vec![
                "text/plain",
                "text/markdown",
                "text/csv",
                "text/html",
                "text/css",
                "text/javascript",
                "application/json",
                "application/xml",
                "application/pdf",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ],
        },
        streaming: StreamingCapability {
            token_delta: true,
            fallback_typewriter: true,
        },
        file_rag: FileRagCapability {
            enabled: true,
            supported_types: vec!["txt", "md", "pdf", "csv", "json", "docx", "xlsx", "image"],
        },
        multimodal: MultimodalCapability {
            // The current Chat stream path summarizes images before the main
            // text/tool runtime turn. Model config may declare vision support,
            // but reporting nativeVision=true here would overstate the actual
            // product path until image blocks are persisted end-to-end.
            native_vision: false,
            image_summary_fallback: true,
        },
        memory: MemoryCapability {
            enabled: true,
            default_mode: "auto",
        },
    })
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/capabilities", get(get_chat_capabilities))
        .route("/search-providers", get(get_chat_capabilities))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

#[cfg(test)]
mod tests {
    use super::{current_search_provider, model_supports_reasoning_effort, SearchProviderStatus};

    #[test]
    fn reasoning_effort_capability_is_model_specific() {
        assert!(model_supports_reasoning_effort("anthropic/claude-opus-4-8"));
        assert!(model_supports_reasoning_effort("openai/o4-mini"));
        assert!(model_supports_reasoning_effort("openai/o3"));
        assert!(!model_supports_reasoning_effort("openai/gpt-4o"));
        assert!(!model_supports_reasoning_effort("grok-3"));
    }

    #[test]
    fn current_search_provider_prefers_db_extension_over_builtin_and_native() {
        let providers = vec![
            SearchProviderStatus {
                provider: "aos_builtin_web_search".to_string(),
                configured: true,
                source: "builtin".to_string(),
                detail: "always available".to_string(),
            },
            SearchProviderStatus {
                provider: "model_native_search".to_string(),
                configured: true,
                source: "model".to_string(),
                detail: "available".to_string(),
            },
            SearchProviderStatus {
                provider: "brave".to_string(),
                configured: true,
                source: "db".to_string(),
                detail: "1 enabled config".to_string(),
            },
            SearchProviderStatus {
                provider: "exa".to_string(),
                configured: true,
                source: "db".to_string(),
                detail: "1 enabled config".to_string(),
            },
        ];

        assert_eq!(
            current_search_provider(&providers).as_deref(),
            Some("brave")
        );
    }

    #[test]
    fn current_search_provider_falls_back_to_first_configured_provider() {
        let providers = vec![
            SearchProviderStatus {
                provider: "brave".to_string(),
                configured: false,
                source: "db".to_string(),
                detail: "0 enabled config".to_string(),
            },
            SearchProviderStatus {
                provider: "mcp_search".to_string(),
                configured: true,
                source: "mcp".to_string(),
                detail: "enabled server".to_string(),
            },
        ];

        assert_eq!(
            current_search_provider(&providers).as_deref(),
            Some("mcp_search")
        );
    }
}
