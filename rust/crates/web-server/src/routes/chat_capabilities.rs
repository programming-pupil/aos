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
    canonical_surface: RuntimeCapabilityStatus,
    sandbox: RuntimeCapabilityStatus,
    agent_team: RuntimeCapabilityStatus,
    provider_compaction: ProviderCompactionCapability,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeCapabilityStatus {
    configured: bool,
    supported: bool,
    active: bool,
    enforcement: &'static str,
    unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderCompactionCapability {
    configured: bool,
    supported: bool,
    active: bool,
    protocol: Option<String>,
    endpoint_called: bool,
    output_applied: bool,
    fallback_reason: Option<String>,
    unavailable_reason: Option<String>,
}

fn compaction_attempt_is_active(
    configured: bool,
    supported: bool,
    status: &str,
    output_applied: bool,
) -> bool {
    configured && supported && status == "completed" && output_applied
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

async fn configured_provider_supports_responses_compact_v1(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> bool {
    let row = sqlx::query(
        r#"
        SELECT provider, base_url
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
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    row.is_some_and(|row| {
        let provider = sqlx::Row::get::<String, _>(&row, "provider");
        let base_url = sqlx::Row::get::<Option<String>, _>(&row, "base_url");
        provider.trim().to_ascii_lowercase() != "anthropic"
            || base_url.is_some_and(|value| !value.trim().is_empty())
    })
}

fn configured_compaction_protocol(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let capability = [
        "providerNativeCompaction",
        "provider_native_compaction",
        "nativeCompaction",
        "native_compaction",
        "responsesCompaction",
        "responses_compaction",
    ]
    .iter()
    .find_map(|key| value.get(*key))?;
    fn canonical_protocol(protocol: &str) -> Option<String> {
        match protocol.trim().to_ascii_lowercase().as_str() {
            "model_summary" | "summary" | "chat_summary" => Some("model_summary".to_string()),
            "responses_compact_v1" | "responses_v1" | "v1" => {
                Some("responses_compact_v1".to_string())
            }
            "responses_compact_v2" | "responses_v2" | "v2" => {
                Some("responses_compact_v2".to_string())
            }
            _ => None,
        }
    }
    match capability {
        Value::Bool(true) => Some("model_summary".to_string()),
        Value::String(protocol) => canonical_protocol(protocol),
        Value::Object(object) => {
            let enabled = object
                .get("enabled")
                .or_else(|| object.get("enable"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !enabled {
                return None;
            }
            canonical_protocol(
                object
                    .get("protocol")
                    .or_else(|| object.get("mode"))
                    .or_else(|| object.get("strategy"))
                    .and_then(Value::as_str)
                    .unwrap_or("model_summary"),
            )
        }
        _ => None,
    }
}

async fn provider_compaction_capability(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> ProviderCompactionCapability {
    let capabilities = configured_model_capabilities_json(state, tenant_id, model).await;
    let protocol = configured_compaction_protocol(capabilities.as_ref());
    let configured = protocol.is_some();
    let protocol_supported = protocol.as_deref() == Some("responses_compact_v1");
    let provider_supported =
        configured_provider_supports_responses_compact_v1(state, tenant_id, model).await;
    let supported = protocol_supported && provider_supported;
    let provider_model = model
        .split_once('/')
        .map_or(model, |(_, provider_model)| provider_model);
    let latest = sqlx::query(
        "SELECT status, output_applied, fallback_reason
         FROM provider_compaction_attempts
         WHERE tenant_id = ? AND model IN (?, ?) AND protocol = 'responses_compact_v1'
         ORDER BY created_at DESC, attempt_index DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(model)
    .bind(provider_model)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let endpoint_called = latest.is_some();
    let output_applied = latest
        .as_ref()
        .is_some_and(|row| sqlx::Row::get::<i64, _>(row, "output_applied") != 0);
    let active = latest.as_ref().is_some_and(|row| {
        compaction_attempt_is_active(
            configured,
            supported,
            &sqlx::Row::get::<String, _>(row, "status"),
            output_applied,
        )
    });
    let fallback_reason = latest
        .as_ref()
        .and_then(|row| sqlx::Row::get::<Option<String>, _>(row, "fallback_reason"));
    let unavailable_reason = if !configured {
        Some("model capability does not declare provider compaction".to_string())
    } else if !protocol_supported {
        Some("only responses_compact_v1 has a verified AOS adapter".to_string())
    } else if !provider_supported {
        Some("the configured provider does not expose a responses compact v1 adapter".to_string())
    } else if !active {
        Some(
            fallback_reason.clone().unwrap_or_else(|| {
                "the configured /responses/compact endpoint has not completed a verified, applied attempt"
                    .to_string()
            }),
        )
    } else {
        None
    };
    ProviderCompactionCapability {
        configured,
        supported,
        active,
        protocol,
        endpoint_called,
        output_applied,
        fallback_reason,
        unavailable_reason,
    }
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
    let provider_compaction =
        provider_compaction_capability(&state, &claims.tenant_id, selected_model).await;
    let sandbox_supported =
        runtime::sandbox_backend_capability() == runtime::sandbox::EnforcementCapability::Full;

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
        canonical_surface: RuntimeCapabilityStatus {
            configured: true,
            supported: true,
            active: true,
            enforcement: "append_only_fold_and_dispatch_hash",
            unavailable_reason: None,
        },
        sandbox: RuntimeCapabilityStatus {
            configured: true,
            supported: sandbox_supported,
            active: sandbox_supported,
            enforcement: if sandbox_supported {
                "bwrap_prlimit_full"
            } else {
                "fail_closed"
            },
            unavailable_reason: (!sandbox_supported).then(|| {
                "bwrap+prlimit filesystem/network/resource probe failed; shell execution is unavailable"
                    .to_string()
            }),
        },
        agent_team: RuntimeCapabilityStatus {
            configured: true,
            supported: true,
            // Ordinary Chat does not expose the control tools. The unified
            // Super Assistant parent/child runtime is the active surface.
            active: false,
            enforcement: "super_assistant_parent_scope",
            unavailable_reason: Some(
                "Agent Team tools are active only in the unified Super Assistant runtime"
                    .to_string(),
            ),
        },
        provider_compaction,
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
    use super::{
        compaction_attempt_is_active, configured_compaction_protocol, current_search_provider,
        model_supports_reasoning_effort, SearchProviderStatus,
    };
    use serde_json::json;

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

    #[test]
    fn provider_compaction_aliases_are_canonicalized_and_only_applied_output_is_active() {
        assert_eq!(
            configured_compaction_protocol(Some(&json!({
                "providerNativeCompaction": {"enabled": true, "protocol": "responses_v1"}
            }))),
            Some("responses_compact_v1".to_string())
        );
        assert_eq!(
            configured_compaction_protocol(Some(&json!({
                "providerNativeCompaction": {"enabled": true, "protocol": "unknown_v9"}
            }))),
            None
        );
        assert!(!compaction_attempt_is_active(
            true,
            true,
            "completed",
            false
        ));
        assert!(compaction_attempt_is_active(true, true, "completed", true));
        assert!(!compaction_attempt_is_active(true, true, "failed", true));
        assert!(!compaction_attempt_is_active(
            false,
            true,
            "completed",
            true
        ));
        assert!(!compaction_attempt_is_active(
            true,
            false,
            "completed",
            true
        ));
    }
}
