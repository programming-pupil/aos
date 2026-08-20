//! API Key Gateway Management API.

use agent_gateway::crypto;
use axum::{
    extract::{Extension, Path, State},
    routing::{
        delete as routing_delete, get as routing_get, post as routing_post, put as routing_put,
    },
    Json, Router,
};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::state::AppState;

const ALLOWED_MODEL_TYPES: &[&str] = &["chat", "embedding", "image", "video", "audio"];
const ALLOWED_SCENARIOS: &[&str] = &["chat", "nl2sql", "rd", "pm"];
const ALLOWED_REASONING_EFFORTS: &[&str] = &[
    "minimal", "low", "medium", "high", "xhigh", "max", "enabled", "disabled",
];
const MIN_MODEL_TOKEN_LIMIT: u64 = 1_024;
const MAX_MODEL_TOKEN_LIMIT: u64 = 10_000_000;
const EMBEDDING_ALL_SCENARIOS_JSON: &str = r#"["chat","nl2sql","rd","pm"]"#;
const RESERVED_EXTRA_BODY_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "tools",
    "tool_choice",
    "max_tokens",
    "max_completion_tokens",
    "system",
];

fn is_allowed_model_type(model_type: &str) -> bool {
    ALLOWED_MODEL_TYPES.contains(&model_type)
}

#[derive(Debug, PartialEq, Eq)]
struct ApiKeyUpdateClears {
    dimensions: bool,
    audio_paths: bool,
    chat_capabilities: bool,
    model_profile: bool,
}

fn api_key_update_clears(
    explicit_model_type: Option<&str>,
    target_model_type: &str,
    model_identity_changed: bool,
) -> ApiKeyUpdateClears {
    let model_type_changed = explicit_model_type.is_some();
    ApiKeyUpdateClears {
        dimensions: model_type_changed && target_model_type != "embedding",
        audio_paths: model_type_changed && target_model_type != "audio",
        chat_capabilities: model_type_changed && target_model_type != "chat",
        model_profile: model_identity_changed && target_model_type != "chat",
    }
}

fn role_has_apikey_permission(role: &str, permission: &str) -> bool {
    match permission {
        "apikeys:read" => matches!(role, "developer" | "admin" | "superadmin"),
        "apikeys:write" | "apikeys:delete" => matches!(role, "admin" | "superadmin"),
        _ => false,
    }
}

async fn require_apikey_permission(
    state: &AppState,
    claims: &Claims,
    permission: &str,
) -> Result<()> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT role, CAST(menu_permissions_json AS TEXT)
         FROM users WHERE tenant_id = ? AND id = ? AND is_active = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;
    let Some((role, menu_permissions_json)) = row else {
        return Err(AppError::Forbidden);
    };
    let allowed = menu_permissions_json.map_or_else(
        || role_has_apikey_permission(&role, permission),
        |raw| {
            crate::routes::users::parse_permissions_json(Some(raw))
                .iter()
                .any(|value| value == permission)
        },
    );
    if allowed {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn require_chat_model_type(model_type: Option<&str>) -> Result<&str> {
    let model_type = model_type.unwrap_or("chat").trim();
    if model_type != "chat" {
        return Err(AppError::ValidationError(
            "model capability profiles are only supported for chat models".into(),
        ));
    }
    Ok(model_type)
}

fn normalize_api_key_scenarios(scenarios: Option<&Vec<String>>) -> Result<String> {
    let Some(scenarios) = scenarios else {
        return Err(AppError::ValidationError("scenarios is required".into()));
    };
    let mut normalized = Vec::<String>::new();
    for scenario in scenarios {
        let scenario = scenario.trim().to_ascii_lowercase();
        let normalized_scenario = match scenario.as_str() {
            "chat" => "chat",
            "nl2sql" | "data_explorer" | "data_exploration" | "analytics" => "nl2sql",
            "rd" | "agent" | "agent_dev" | "development" | "dev" => "rd",
            "pm" | "operations" | "operations_copilot" | "pm_assistant" => "pm",
            other => {
                return Err(AppError::ValidationError(format!(
                    "unsupported scenario: {other}; allowed scenarios: {}",
                    ALLOWED_SCENARIOS.join(", ")
                )));
            }
        };
        if !normalized.iter().any(|item| item == normalized_scenario) {
            normalized.push(normalized_scenario.to_string());
        }
    }
    if normalized.is_empty() {
        return Err(AppError::ValidationError(
            "at least one scenario is required".into(),
        ));
    }
    serde_json::to_string(&normalized)
        .map_err(|e| AppError::ValidationError(format!("invalid scenarios: {e}")))
}

fn normalize_api_key_scenarios_for_model_type(
    model_type: &str,
    scenarios: Option<&Vec<String>>,
) -> Result<String> {
    if model_type.eq_ignore_ascii_case("embedding") {
        return Ok(EMBEDDING_ALL_SCENARIOS_JSON.to_string());
    }
    normalize_api_key_scenarios(scenarios)
}

fn normalize_api_key_capabilities(capabilities: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = capabilities else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Value::Object(map) = value else {
        return Err(AppError::ValidationError(
            "capabilities_json must be a JSON object".into(),
        ));
    };

    let mut normalized = serde_json::Map::new();
    for key in [
        "reasoningEffort",
        "includeReasoning",
        "useMaxCompletionTokens",
    ] {
        if let Some(v) = map.get(key).and_then(Value::as_bool) {
            normalized.insert(key.to_string(), Value::Bool(v));
        }
    }
    if let Some(policy) = map.get("reasoningPolicy").and_then(Value::as_str) {
        let policy = policy.trim().to_ascii_lowercase();
        if !matches!(
            policy.as_str(),
            "auto" | "fast" | "standard" | "deep" | "maximum"
        ) {
            return Err(AppError::ValidationError(
                "unsupported reasoningPolicy".into(),
            ));
        }
        if policy != "auto" {
            normalized.insert("reasoningPolicy".to_string(), Value::String(policy));
        }
    }
    if let Some(native_raw) = map
        .get("nativeWebSearch")
        .or_else(|| map.get("native_web_search"))
    {
        if let Some(native) = normalize_native_web_search_capability(native_raw)? {
            normalized.insert("nativeWebSearch".to_string(), native);
        }
    }
    if let Some(raw) = map.get("reasoningEffortDefault").and_then(Value::as_str) {
        let effort = raw.trim().to_ascii_lowercase();
        if !effort.is_empty() {
            if !ALLOWED_REASONING_EFFORTS.contains(&effort.as_str()) {
                return Err(AppError::ValidationError(
                    "reasoningEffortDefault is not supported by AOS".into(),
                ));
            }
            normalized.insert("reasoningEffortDefault".to_string(), Value::String(effort));
        }
    }
    if let Some(raw) = map.get("reasoningTransport").and_then(Value::as_str) {
        let transport = raw.trim().to_ascii_lowercase();
        if !matches!(
            transport.as_str(),
            "reasoning_effort" | "anthropic_thinking" | "thinking_level" | "enable_thinking"
        ) {
            return Err(AppError::ValidationError(
                "unsupported reasoningTransport".into(),
            ));
        }
        normalized.insert("reasoningTransport".to_string(), Value::String(transport));
    }
    if let Some(values) = map.get("reasoningEffortValues").and_then(Value::as_array) {
        let mut supported = Vec::new();
        for value in values {
            let value = value
                .as_str()
                .map(str::trim)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !ALLOWED_REASONING_EFFORTS.contains(&value.as_str()) {
                return Err(AppError::ValidationError(
                    "reasoningEffortValues contains an unsupported value".into(),
                ));
            }
            if !supported.iter().any(|item| item == &value) {
                supported.push(value);
            }
        }
        if !supported.is_empty() {
            normalized.insert("reasoningEffortValues".to_string(), json!(supported));
        }
    }
    if let Some(budget_map) = map.get("reasoningBudgetMap").and_then(Value::as_object) {
        let mut supported = serde_json::Map::new();
        for budget in ["fast", "standard", "deep"] {
            if let Some(value) = budget_map.get(budget).and_then(Value::as_str) {
                let value = value.trim().to_ascii_lowercase();
                if !ALLOWED_REASONING_EFFORTS.contains(&value.as_str()) {
                    return Err(AppError::ValidationError(
                        "reasoningBudgetMap contains an unsupported value".into(),
                    ));
                }
                supported.insert(budget.to_string(), Value::String(value));
            }
        }
        if !supported.is_empty() {
            normalized.insert("reasoningBudgetMap".to_string(), Value::Object(supported));
        }
    }
    for key in ["contextWindowTokens", "maxOutputTokens"] {
        if let Some(raw) = map.get(key) {
            let Some(value) = raw.as_u64() else {
                return Err(AppError::ValidationError(format!(
                    "{key} must be an unsigned integer"
                )));
            };
            if !(MIN_MODEL_TOKEN_LIMIT..=MAX_MODEL_TOKEN_LIMIT).contains(&value) {
                return Err(AppError::ValidationError(format!(
                    "{key} must be between {MIN_MODEL_TOKEN_LIMIT} and {MAX_MODEL_TOKEN_LIMIT}"
                )));
            }
            normalized.insert(key.to_string(), Value::Number(value.into()));
        }
    }

    if normalized.is_empty() {
        Ok(None)
    } else {
        serde_json::to_string(&Value::Object(normalized))
            .map(Some)
            .map_err(|e| AppError::ValidationError(format!("invalid capabilities_json: {e}")))
    }
}

fn merge_capability_values(base: Option<Value>, overrides: Option<Value>) -> Option<Value> {
    let mut base = base.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let Some(overrides) = overrides else {
        return base
            .as_object()
            .is_some_and(|value| !value.is_empty())
            .then_some(base);
    };
    let (Some(base_object), Some(override_object)) = (base.as_object_mut(), overrides.as_object())
    else {
        return Some(overrides);
    };
    for (key, value) in override_object {
        base_object.insert(key.clone(), value.clone());
    }
    (!base_object.is_empty()).then_some(base)
}

fn normalize_profile_base_url(base_url: Option<&str>) -> String {
    base_url
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn canonical_profile_provider(provider: &str, base_url: Option<&str>) -> String {
    let base = normalize_profile_base_url(base_url);
    if base.contains("api.deepseek.com") {
        return "deepseek".to_string();
    }
    if base.contains("api.moonshot.cn") {
        return "kimi".to_string();
    }
    if base.contains("open.bigmodel.cn") {
        return "glm".to_string();
    }
    if base.contains("generativelanguage.googleapis.com") {
        return "gemini".to_string();
    }
    if base.contains("api.anthropic.com") {
        return "anthropic".to_string();
    }
    if base.contains("api.openai.com") {
        return "openai".to_string();
    }
    if base.contains("api.x.ai") {
        return "xai".to_string();
    }
    if base.contains("openrouter.ai") {
        return "openrouter".to_string();
    }
    provider.trim().to_ascii_lowercase()
}

fn model_profile_key(
    provider: &str,
    base_url: Option<&str>,
    protocol: &str,
    model: &str,
    model_type: &str,
) -> String {
    let identity = format!(
        "{}\n{}\n{}\n{}\n{}",
        canonical_profile_provider(provider, base_url),
        normalize_profile_base_url(base_url),
        protocol,
        model.trim().to_ascii_lowercase(),
        model_type.trim().to_ascii_lowercase(),
    );
    hex::encode(Sha256::digest(identity.as_bytes()))
}

fn profile_source_and_confidence(profile: &api::ModelProfile) -> (&'static str, &'static str) {
    if profile.context_window_tokens.is_some() || profile.reasoning.is_some() {
        ("built_in_registry", "high")
    } else if profile.features.tools.is_some() {
        ("built_in_registry", "medium")
    } else {
        ("conservative_fallback", "low")
    }
}

fn profile_source_rank(source: &str) -> u8 {
    match source {
        "probe" => 3,
        "provider_metadata" => 2,
        "built_in_registry" => 1,
        _ => 0,
    }
}

fn should_preserve_existing_profile(
    existing_source: &str,
    existing_status: &str,
    existing_registry: &str,
    new_source: &str,
    new_status: &str,
) -> bool {
    existing_status == "verified"
        || profile_source_rank(existing_source) > profile_source_rank(new_source)
        || (new_status == "inferred"
            && existing_source == new_source
            && existing_registry == api::MODEL_CAPABILITY_REGISTRY_VERSION)
}

fn model_profile_expiration(source: &str) -> Option<String> {
    matches!(source, "probe" | "provider_metadata")
        .then(|| (Utc::now() + chrono::Duration::days(30)).to_rfc3339())
}

async fn upsert_model_profile(
    state: &AppState,
    tenant_id: &str,
    provider: &str,
    base_url: Option<&str>,
    model: &str,
    model_type: &str,
    profile: &api::ModelProfile,
    source_override: Option<&str>,
    confidence_override: Option<&str>,
    detection_status: &str,
    observations: Option<&Value>,
    last_error: Option<&str>,
) -> Result<String> {
    let protocol = profile.protocol.as_str();
    let canonical_base_url = effective_provider_base_url(provider, base_url)?;
    let profile_key = model_profile_key(
        provider,
        Some(&canonical_base_url),
        protocol,
        model,
        model_type,
    );
    let (default_source, default_confidence) = profile_source_and_confidence(profile);
    let source = source_override.unwrap_or(default_source);
    let confidence = confidence_override.unwrap_or(default_confidence);
    if detection_status != "verified" {
        if let Some((existing_id, existing_source, existing_status, existing_registry)) =
            sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT id, source, detection_status, registry_version
                 FROM model_capability_profiles
                 WHERE tenant_id = ? AND profile_key = ?",
            )
            .bind(tenant_id)
            .bind(&profile_key)
            .fetch_optional(&state.db)
            .await?
        {
            if should_preserve_existing_profile(
                &existing_source,
                &existing_status,
                &existing_registry,
                source,
                detection_status,
            ) {
                return Ok(existing_id);
            }
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let capabilities = profile.runtime_capabilities().to_string();
    let observations = observations.map(Value::to_string);
    let detected_at = (detection_status == "verified").then(|| Utc::now().to_rfc3339());
    let expires_at = model_profile_expiration(source);

    sqlx::query(
        "INSERT INTO model_capability_profiles
            (id, tenant_id, profile_key, provider, base_url, protocol, model, model_type,
             schema_version, registry_version, source, confidence, capabilities_json,
             observations_json, detection_status, last_error, detected_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(tenant_id, profile_key) DO UPDATE SET
            provider = excluded.provider,
            base_url = excluded.base_url,
            protocol = excluded.protocol,
            model = excluded.model,
            model_type = excluded.model_type,
            schema_version = excluded.schema_version,
            registry_version = excluded.registry_version,
            source = excluded.source,
            confidence = excluded.confidence,
            capabilities_json = excluded.capabilities_json,
            observations_json = COALESCE(excluded.observations_json, model_capability_profiles.observations_json),
            detection_status = excluded.detection_status,
            last_error = excluded.last_error,
            detected_at = COALESCE(excluded.detected_at, model_capability_profiles.detected_at),
            expires_at = excluded.expires_at,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(&profile_key)
    .bind(canonical_profile_provider(provider, Some(&canonical_base_url)))
    .bind(normalize_profile_base_url(Some(&canonical_base_url)))
    .bind(protocol)
    .bind(model.trim())
    .bind(model_type.trim().to_ascii_lowercase())
    .bind(i64::from(api::MODEL_CAPABILITY_SCHEMA_VERSION))
    .bind(api::MODEL_CAPABILITY_REGISTRY_VERSION)
    .bind(source)
    .bind(confidence)
    .bind(capabilities)
    .bind(observations)
    .bind(detection_status)
    .bind(last_error)
    .bind(detected_at)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    sqlx::query_scalar::<_, String>(
        "SELECT id FROM model_capability_profiles WHERE tenant_id = ? AND profile_key = ?",
    )
    .bind(tenant_id)
    .bind(profile_key)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)
}

async fn publish_linked_model_profile_change(
    state: &AppState,
    tenant_id: &str,
    profile_id: &str,
) -> Result<()> {
    let linked: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM api_keys
             WHERE tenant_id = ? AND model_profile_id = ?
         )",
    )
    .bind(tenant_id)
    .bind(profile_id)
    .fetch_one(&state.db)
    .await?;
    if linked == 0 {
        return Ok(());
    }
    sqlx::query("UPDATE tenants SET api_keys_version = api_keys_version + 1 WHERE id = ?")
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(registry) = state.config_registry.as_ref() {
        registry.invalidate_tenant_cache(tenant_id).await;
    }
    Ok(())
}

fn normalize_native_web_search_capability(raw: &Value) -> Result<Option<Value>> {
    match raw {
        Value::Bool(enabled) => {
            if *enabled {
                Ok(Some(serde_json::json!({ "enabled": true })))
            } else {
                Ok(None)
            }
        }
        Value::Object(map) => {
            let enabled = map
                .get("enabled")
                .or_else(|| map.get("enable"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !enabled {
                return Ok(None);
            }
            let mut out = serde_json::Map::new();
            out.insert("enabled".to_string(), Value::Bool(true));
            if let Some(mode) = map
                .get("mode")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                out.insert("mode".to_string(), Value::String(mode.to_string()));
            }
            if let Some(extra_body) = map
                .get("extraBody")
                .or_else(|| map.get("extra_body"))
                .and_then(Value::as_object)
            {
                let mut normalized_extra = serde_json::Map::new();
                for (key, value) in extra_body {
                    let normalized_key = key.trim().to_ascii_lowercase();
                    if RESERVED_EXTRA_BODY_KEYS.contains(&normalized_key.as_str()) {
                        continue;
                    }
                    normalized_extra.insert(key.clone(), value.clone());
                }
                if !normalized_extra.is_empty() {
                    out.insert("extraBody".to_string(), Value::Object(normalized_extra));
                }
            }
            if let Some(tool_template) =
                map.get("toolTemplate").or_else(|| map.get("tool_template"))
            {
                if tool_template.is_object() {
                    out.insert("toolTemplate".to_string(), tool_template.clone());
                }
            }
            Ok(Some(Value::Object(out)))
        }
        _ => Err(AppError::ValidationError(
            "nativeWebSearch must be a boolean or object".into(),
        )),
    }
}

fn parse_json_object(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .filter(Value::is_object)
}

fn normalize_audio_model_value(raw: Option<&str>) -> String {
    let mut current = raw.unwrap_or("").trim().to_string();
    if current.is_empty() {
        return "suno".to_string();
    }
    loop {
        let lowered = current.to_ascii_lowercase();
        if lowered == "suno" {
            return "suno".to_string();
        }
        let mut consumed = false;
        for sep in ["/", ":", "|"] {
            let prefix = format!("suno{sep}");
            if lowered.starts_with(&prefix) {
                current = current[prefix.len()..].trim().to_string();
                consumed = true;
                break;
            }
        }
        if !consumed {
            break;
        }
        if current.is_empty() {
            return "suno".to_string();
        }
    }
    format!("suno/{current}")
}

fn normalize_audio_endpoint_path(raw: Option<&str>) -> Option<String> {
    let trimmed = raw.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with('/') {
        return Some(trimmed.to_string());
    }
    Some(format!("/{trimmed}"))
}

#[derive(Debug, Serialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<i32>,
    pub audio_generate_path: Option<String>,
    pub audio_query_path: Option<String>,
    /// Model key type: 'chat', 'embedding', 'image', 'video', or 'audio'.
    pub model_type: String,
    pub key_hint: String,
    pub daily_limit: Option<i64>,
    pub monthly_limit: Option<i64>,
    pub enabled: bool,
    pub priority: i32,
    pub is_primary: bool,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    /// JSON array of scenario tags, e.g. ["nl2sql", "chat", "agent"].
    /// Null means the key is available to all scenarios.
    pub scenarios: Option<Vec<String>>,
    /// Explicit per-model/key runtime capabilities.
    pub capabilities_json: Option<Value>,
    /// Effective capabilities after merging the detected model profile with
    /// explicit per-key overrides.
    pub resolved_capabilities: Option<Value>,
    pub model_profile: Option<ModelProfileSummary>,
    /// Whether this row can be used by runtime code right now.
    ///
    /// A configured row may be unavailable if it is disabled or if the
    /// encrypted key cannot be decrypted with the current `ENCRYPTION_KEY`.
    pub runtime_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfileSummary {
    pub id: String,
    pub protocol: String,
    pub source: String,
    pub confidence: String,
    pub detection_status: String,
    pub detected_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveModelRequest {
    pub provider: String,
    pub base_url: Option<String>,
    pub model: String,
    pub model_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveModelResponse {
    pub profile: Value,
    pub source: String,
    pub confidence: String,
    pub requires_probe: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverModelsRequest {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub existing_key_id: Option<String>,
    pub model_type: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    pub id: String,
    pub display_name: Option<String>,
    pub created_at: Option<String>,
    pub profile: Value,
    pub source: String,
    pub confidence: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverModelsResponse {
    pub models: Vec<DiscoveredModel>,
    pub endpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeModelRequest {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub existing_key_id: Option<String>,
    pub model: String,
    pub model_type: Option<String>,
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeModelResponse {
    pub ok: bool,
    pub latency_ms: u64,
    pub endpoint: String,
    pub profile: Value,
    pub source: String,
    pub confidence: String,
    pub checks: Vec<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptModelProfileRequest {
    pub provider: String,
    pub base_url: Option<String>,
    pub model: String,
    pub model_type: Option<String>,
    pub profile: Value,
    pub source: String,
    pub confidence: String,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyListResponse {
    pub keys: Vec<ApiKeyRecord>,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub provider: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<i32>,
    pub audio_generate_path: Option<String>,
    pub audio_query_path: Option<String>,
    /// Model key type: 'chat', 'embedding', 'image', 'video', or 'audio'.
    /// Defaults to 'chat' if not provided.
    pub model_type: Option<String>,
    pub key_value: String,
    pub daily_limit: Option<i64>,
    pub monthly_limit: Option<i64>,
    pub priority: Option<i32>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    /// JSON array of scenario tags, e.g. ["chat", "nl2sql", "rd", "pm"].
    /// Required for new keys.
    pub scenarios: Option<Vec<String>>,
    pub capabilities_json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiKeyRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<i32>,
    pub audio_generate_path: Option<String>,
    pub audio_query_path: Option<String>,
    /// 'chat', 'embedding', 'image', 'video', or 'audio'. Omit to leave unchanged.
    pub model_type: Option<String>,
    pub key_value: Option<String>,
    pub daily_limit: Option<i64>,
    pub monthly_limit: Option<i64>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
    pub input_price_per_million: Option<f64>,
    pub output_price_per_million: Option<f64>,
    /// JSON array of scenario tags. Omit to leave unchanged; explicit empty is rejected.
    pub scenarios: Option<Vec<String>>,
    pub capabilities_json: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyCreatedResponse {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub key_hint: String,
}

#[derive(Debug, Serialize)]
pub struct UsageRecord {
    pub date: String,
    pub calls: i64,
    pub tokens_used: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyStats {
    pub key_id: String,
    pub total_calls: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub daily_usage: Vec<UsageRecord>,
}

/// Decrypts an API key value using the same AES-256-GCM scheme used in the API
/// key CRUD handlers. Returns the plaintext key or a string error message.
pub fn decrypt_api_key(
    encrypted: &str,
    tenant_id: &str,
    key_id: &str,
) -> std::result::Result<String, String> {
    crypto::decrypt_scoped(
        encrypted,
        &crypto::scoped_aad("api_keys.encrypted_key", tenant_id, key_id),
    )
    .map_err(|e| e.to_string())
}

fn api_key_runtime_status(
    enabled: bool,
    encrypted_key: &str,
    tenant_id: &str,
    key_id: &str,
) -> (bool, Option<String>) {
    if !enabled {
        return (false, Some("disabled".to_string()));
    }
    if encrypted_key.is_empty() {
        return if runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK")
            && std::env::var("ANTHROPIC_API_KEY").is_ok()
        {
            (true, None)
        } else {
            (false, Some("encrypted tenant API key is empty".to_string()))
        };
    }
    match decrypt_api_key(encrypted_key, tenant_id, key_id) {
        Ok(_) => (true, None),
        Err(e) => (false, Some(format!("decryption failed: {e}"))),
    }
}

#[cfg(feature = "nl2sql")]
async fn reconcile_embedding_profiles(state: &AppState, tenant_id: &str) {
    if let Err(error) =
        crate::nl2sql::embedding_profiles::reconcile_tenant_profiles(&state.db, tenant_id).await
    {
        tracing::error!(tenant_id, error = %error, "failed to reconcile embedding profiles after API key change");
    }
}

#[cfg(not(feature = "nl2sql"))]
async fn reconcile_embedding_profiles(_state: &AppState, _tenant_id: &str) {}

fn effective_provider_base_url(provider: &str, base_url: Option<&str>) -> Result<String> {
    if let Some(base_url) = base_url.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(base_url.trim_end_matches('/').to_string());
    }
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok("https://api.openai.com".to_string()),
        "anthropic" => Ok("https://api.anthropic.com".to_string()),
        "deepseek" => Ok("https://api.deepseek.com".to_string()),
        "kimi" | "moonshot" => Ok("https://api.moonshot.cn".to_string()),
        "glm" | "zhipu" => Ok("https://open.bigmodel.cn/api/paas/v4".to_string()),
        "gemini" | "google" => {
            Ok("https://generativelanguage.googleapis.com/v1beta/openai".to_string())
        }
        "xai" => Ok("https://api.x.ai".to_string()),
        "custom" => Err(AppError::ValidationError(
            "base_url is required for custom providers".into(),
        )),
        other => Err(AppError::ValidationError(format!(
            "unsupported provider: {other}"
        ))),
    }
}

fn strip_provider_endpoint_suffix(base: &str) -> String {
    let mut value = base.trim().trim_end_matches('/').to_string();
    for suffix in [
        "/chat/completions",
        "/responses",
        "/messages",
        "/embeddings",
        "/models",
    ] {
        if value.to_ascii_lowercase().ends_with(suffix) {
            value.truncate(value.len().saturating_sub(suffix.len()));
            return value.trim_end_matches('/').to_string();
        }
    }
    value
}

fn model_list_endpoint_candidates(provider: &str, base: &str) -> Vec<String> {
    let base = strip_provider_endpoint_suffix(base);
    let mut candidates = Vec::new();
    let lower = base.to_ascii_lowercase();
    if lower.ends_with("/v1") || lower.contains("/compatible-mode/v1") {
        candidates.push(format!("{base}/models"));
    } else {
        candidates.push(format!("{base}/v1/models"));
        if !provider.eq_ignore_ascii_case("anthropic") {
            candidates.push(format!("{base}/models"));
        }
    }
    candidates.dedup();
    candidates
}

async fn resolve_probe_api_key(
    state: &AppState,
    tenant_id: &str,
    api_key: Option<&str>,
    existing_key_id: Option<&str>,
) -> Result<String> {
    if let Some(api_key) = api_key.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(api_key.to_string());
    }
    let Some(existing_key_id) = existing_key_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(AppError::ValidationError(
            "api_key or existing_key_id is required".into(),
        ));
    };
    let encrypted: Option<String> =
        sqlx::query_scalar("SELECT encrypted_key FROM api_keys WHERE id = ? AND tenant_id = ?")
            .bind(existing_key_id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await?;
    let encrypted = encrypted.ok_or_else(|| AppError::NotFound("API key not found".into()))?;
    decrypt_api_key(&encrypted, tenant_id, existing_key_id)
        .map_err(|error| AppError::Internal(format!("API key decryption failed: {error}")))
}

fn profile_value_u32(value: &Value, paths: &[&[&str]]) -> Option<u32> {
    for path in paths {
        let mut current = value;
        let mut found = true;
        for key in *path {
            let Some(next) = current.get(*key) else {
                found = false;
                break;
            };
            current = next;
        }
        if found {
            if let Some(number) = current
                .as_u64()
                .and_then(|number| u32::try_from(number).ok())
            {
                return Some(number);
            }
        }
    }
    None
}

fn enrich_profile_from_provider_metadata(
    profile: &mut api::ModelProfile,
    metadata: &Value,
) -> bool {
    let mut changed = false;
    if let Some(context) = profile_value_u32(
        metadata,
        &[
            &["context_length"],
            &["context_window"],
            &["input_token_limit"],
            &["top_provider", "context_length"],
        ],
    ) {
        profile.context_window_tokens = Some(context);
        changed = true;
    }
    if let Some(max_output) = profile_value_u32(
        metadata,
        &[
            &["max_output_tokens"],
            &["output_token_limit"],
            &["top_provider", "max_completion_tokens"],
        ],
    ) {
        profile.max_output_tokens = Some(max_output);
        changed = true;
    }

    let supported_parameters: Vec<String> = metadata
        .get("supported_parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
        .collect();
    if !supported_parameters.is_empty() {
        profile.features.tools = Some(
            supported_parameters
                .iter()
                .any(|value| matches!(value.as_str(), "tools" | "tool_choice")),
        );
        profile.features.structured_output = Some(
            supported_parameters
                .iter()
                .any(|value| matches!(value.as_str(), "response_format" | "structured_outputs")),
        );
        profile.supports_temperature = Some(
            supported_parameters
                .iter()
                .any(|value| value == "temperature"),
        );
        profile.supports_top_p = Some(supported_parameters.iter().any(|value| value == "top_p"));
        if supported_parameters
            .iter()
            .any(|value| value == "max_completion_tokens")
        {
            profile.output_token_parameter = "max_completion_tokens".to_string();
        }
        changed = true;
    }
    let input_modalities = metadata
        .get("architecture")
        .and_then(|value| value.get("input_modalities"))
        .or_else(|| metadata.get("input_modalities"))
        .and_then(Value::as_array);
    if let Some(input_modalities) = input_modalities {
        profile.features.vision = Some(input_modalities.iter().any(|value| {
            value.as_str().is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "image" | "vision")
            })
        }));
        changed = true;
    }
    changed
}

fn discovered_model_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("id").and_then(Value::as_str))
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| value.get("model").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

async fn resolve_model(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ResolveModelRequest>,
) -> Result<Json<ResolveModelResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:read").await?;
    let model_type = require_chat_model_type(req.model_type.as_deref())?;
    if req.model.trim().is_empty() {
        return Err(AppError::ValidationError("model is required".into()));
    }
    let profile = api::infer_model_profile(&req.provider, req.base_url.as_deref(), &req.model);
    let canonical_base = effective_provider_base_url(&req.provider, req.base_url.as_deref())?;
    let profile_key = model_profile_key(
        &req.provider,
        Some(&canonical_base),
        profile.protocol.as_str(),
        &req.model,
        model_type,
    );
    let stored: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT CAST(capabilities_json AS TEXT), source, confidence,
                CASE WHEN expires_at IS NOT NULL AND expires_at < CURRENT_TIMESTAMP
                     THEN 1 ELSE 0 END AS expired
         FROM model_capability_profiles
         WHERE tenant_id = ? AND profile_key = ?",
    )
    .bind(&claims.tenant_id)
    .bind(profile_key)
    .fetch_optional(&state.db)
    .await?;
    if let Some((capabilities, source, confidence, expired)) = stored {
        let profile = serde_json::from_str(&capabilities).map_err(|error| {
            AppError::Internal(format!("stored model profile is invalid: {error}"))
        })?;
        return Ok(Json(ResolveModelResponse {
            profile,
            source,
            confidence: confidence.clone(),
            requires_probe: confidence != "high" || expired != 0,
        }));
    }
    let (source, confidence) = profile_source_and_confidence(&profile);
    Ok(Json(ResolveModelResponse {
        profile: profile.runtime_capabilities(),
        source: source.to_string(),
        confidence: confidence.to_string(),
        requires_probe: confidence != "high",
    }))
}

async fn discover_models(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<DiscoverModelsRequest>,
) -> Result<Json<DiscoverModelsResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:write").await?;
    let requested_model_type = req.model_type.as_deref().unwrap_or("chat");
    if !is_allowed_model_type(requested_model_type) {
        return Err(AppError::ValidationError(
            "unsupported model_type for discovery".into(),
        ));
    }
    let api_key = resolve_probe_api_key(
        &state,
        &claims.tenant_id,
        req.api_key.as_deref(),
        req.existing_key_id.as_deref(),
    )
    .await?;
    let base = effective_provider_base_url(&req.provider, req.base_url.as_deref())?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| AppError::Internal(format!("HTTP client error: {error}")))?;
    let mut last_error = None;
    for endpoint in model_list_endpoint_candidates(&req.provider, &base) {
        let mut request = client.get(&endpoint).header("accept", "application/json");
        if req.provider.eq_ignore_ascii_case("anthropic") {
            request = request
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01");
        } else {
            request = request.bearer_auth(&api_key);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(format!("connection failed: {error}"));
                continue;
            }
        };
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            last_error = Some(format!(
                "model discovery returned {status}: {}",
                body_excerpt(&body, 240)
            ));
            continue;
        }
        let payload: Value = serde_json::from_str(&body).map_err(|error| {
            AppError::ValidationError(format!("model discovery returned invalid JSON: {error}"))
        })?;
        let items = payload
            .get("data")
            .or_else(|| payload.get("models"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::ValidationError(
                    "model discovery response has no data/models array".into(),
                )
            })?;
        let mut models = Vec::new();
        for item in items.iter().take(500) {
            let Some(id) = discovered_model_id(item) else {
                continue;
            };
            let mut profile = api::infer_model_profile(&req.provider, Some(&base), &id);
            let metadata_enriched = enrich_profile_from_provider_metadata(&mut profile, item);
            let (registry_source, registry_confidence) = profile_source_and_confidence(&profile);
            let source = if metadata_enriched {
                "provider_metadata"
            } else {
                registry_source
            };
            let confidence = if metadata_enriched {
                "high"
            } else {
                registry_confidence
            };
            let display_name = item
                .get("display_name")
                .or_else(|| item.get("displayName"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let created_at = item
                .get("created_at")
                .or_else(|| item.get("created"))
                .and_then(|value| {
                    value.as_str().map(ToString::to_string).or_else(|| {
                        value.as_i64().and_then(|timestamp| {
                            DateTime::<Utc>::from_timestamp(timestamp, 0)
                                .map(|value| value.to_rfc3339())
                        })
                    })
                });
            models.push(DiscoveredModel {
                id,
                display_name,
                created_at,
                profile: profile.runtime_capabilities(),
                source: source.to_string(),
                confidence: confidence.to_string(),
            });
        }
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models.dedup_by(|left, right| left.id == right.id);
        return Ok(Json(DiscoverModelsResponse { models, endpoint }));
    }
    Err(AppError::ValidationError(last_error.unwrap_or_else(|| {
        "model discovery failed without a provider response".to_string()
    })))
}

fn validate_accepted_model_profile(
    provider: &str,
    base_url: Option<&str>,
    model: &str,
    value: Value,
) -> Result<api::ModelProfile> {
    let profile: api::ModelProfile = serde_json::from_value(value).map_err(|error| {
        AppError::ValidationError(format!("invalid model capability profile: {error}"))
    })?;
    if profile.schema_version != api::MODEL_CAPABILITY_SCHEMA_VERSION {
        return Err(AppError::ValidationError(
            "unsupported model capability schema version".into(),
        ));
    }
    if profile.registry_version.trim().is_empty() || profile.registry_version.len() > 64 {
        return Err(AppError::ValidationError(
            "invalid model capability registry version".into(),
        ));
    }
    let expected = api::infer_model_profile(provider, base_url, model);
    if profile.protocol != expected.protocol {
        return Err(AppError::ValidationError(
            "model capability protocol does not match the provider".into(),
        ));
    }
    for (label, value) in [
        ("contextWindowTokens", profile.context_window_tokens),
        ("maxOutputTokens", profile.max_output_tokens),
    ] {
        if value.is_some_and(|value| {
            !(MIN_MODEL_TOKEN_LIMIT..=MAX_MODEL_TOKEN_LIMIT).contains(&u64::from(value))
        }) {
            return Err(AppError::ValidationError(format!(
                "{label} is outside the supported range"
            )));
        }
    }
    if !matches!(
        profile.output_token_parameter.as_str(),
        "max_tokens" | "max_completion_tokens" | "max_output_tokens"
    ) {
        return Err(AppError::ValidationError(
            "unsupported output token parameter".into(),
        ));
    }
    if let Some(reasoning) = &profile.reasoning {
        if !matches!(
            reasoning.transport.as_str(),
            "reasoning_effort" | "anthropic_thinking" | "thinking_level" | "enable_thinking"
        ) || reasoning.supported_values.len() > 16
            || reasoning.supported_values.is_empty()
            || reasoning.supported_values.iter().any(|value| {
                value.len() > 32 || !ALLOWED_REASONING_EFFORTS.contains(&value.as_str())
            })
            || reasoning.budget_map.len() > 3
            || reasoning.budget_map.iter().any(|(budget, value)| {
                !matches!(budget.as_str(), "fast" | "standard" | "deep")
                    || !reasoning.supported_values.iter().any(|item| item == value)
            })
            || reasoning
                .default
                .as_ref()
                .is_some_and(|value| !reasoning.supported_values.iter().any(|item| item == value))
        {
            return Err(AppError::ValidationError(
                "invalid reasoning capability profile".into(),
            ));
        }
    }
    Ok(profile)
}

async fn accept_model_profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AcceptModelProfileRequest>,
) -> Result<Json<ResolveModelResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:write").await?;
    let model_type = require_chat_model_type(req.model_type.as_deref())?;
    if req.model.trim().is_empty() {
        return Err(AppError::ValidationError("model is required".into()));
    }
    if !matches!(
        req.source.as_str(),
        "provider_metadata" | "built_in_registry" | "conservative_fallback"
    ) {
        return Err(AppError::ValidationError(
            "unsupported model profile source".into(),
        ));
    }
    if !matches!(req.confidence.as_str(), "low" | "medium" | "high") {
        return Err(AppError::ValidationError(
            "unsupported model profile confidence".into(),
        ));
    }
    let profile = validate_accepted_model_profile(
        &req.provider,
        req.base_url.as_deref(),
        &req.model,
        req.profile,
    )?;
    let profile_id = upsert_model_profile(
        &state,
        &claims.tenant_id,
        &req.provider,
        req.base_url.as_deref(),
        &req.model,
        model_type,
        &profile,
        Some(&req.source),
        Some(&req.confidence),
        "metadata",
        None,
        None,
    )
    .await?;
    publish_linked_model_profile_change(&state, &claims.tenant_id, &profile_id).await?;
    Ok(Json(ResolveModelResponse {
        profile: profile.runtime_capabilities(),
        source: req.source,
        confidence: req.confidence.clone(),
        requires_probe: req.confidence != "high",
    }))
}

fn chat_completion_endpoint_candidates(base: &str) -> Vec<String> {
    let base = strip_provider_endpoint_suffix(base);
    let lower = base.to_ascii_lowercase();
    let mut endpoints = if lower.ends_with("/v1") || lower.contains("/compatible-mode/v1") {
        vec![format!("{base}/chat/completions")]
    } else {
        vec![
            format!("{base}/v1/chat/completions"),
            format!("{base}/chat/completions"),
        ]
    };
    endpoints.dedup();
    endpoints
}

async fn send_probe_request(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    profile: &api::ModelProfile,
    include_reasoning: bool,
) -> std::result::Result<Value, String> {
    let mut body = if provider.eq_ignore_ascii_case("anthropic") {
        serde_json::json!({
            "model": model.trim_start_matches("anthropic/"),
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "Reply OK"}]
        })
    } else {
        serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "Reply OK"}],
            "stream": false
        })
    };
    if !provider.eq_ignore_ascii_case("anthropic") {
        body[profile.output_token_parameter.as_str()] = serde_json::json!(8);
    }
    if include_reasoning {
        if let Some(reasoning) = &profile.reasoning {
            let fast = reasoning
                .budget_map
                .get("fast")
                .or(reasoning.default.as_ref());
            match (reasoning.transport.as_str(), fast) {
                ("reasoning_effort", Some(value)) => body["reasoning_effort"] = json!(value),
                ("anthropic_thinking", _) => {
                    body["max_tokens"] = json!(1_032);
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": 1024});
                }
                ("thinking_level", Some(value)) => {
                    body["thinking_config"] = json!({"thinking_level": value});
                }
                ("enable_thinking", Some(value)) => {
                    body["enable_thinking"] = json!(value != "disabled");
                }
                _ => {}
            }
        }
    }
    send_probe_body(client, provider, endpoint, api_key, &body).await
}

async fn send_probe_body(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    body: &Value,
) -> std::result::Result<Value, String> {
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if provider.eq_ignore_ascii_case("anthropic") {
        request = request
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .json(body)
        .send()
        .await
        .map_err(|error| format!("connection failed: {error}"))?;
    let status = response.status();
    let response_body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "provider returned {status}: {}",
            body_excerpt(&response_body, 300)
        ));
    }
    serde_json::from_str(&response_body)
        .map_err(|error| format!("provider returned invalid JSON: {error}"))
}

fn probe_error_rejects_parameter(error: &str, parameter: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains(parameter)
        && [
            "unsupported",
            "does not support",
            "unknown parameter",
            "unrecognized parameter",
            "not permitted",
            "not allowed",
            "unavailable",
            "unexpected field",
        ]
        .iter()
        .any(|marker| error.contains(marker))
}

fn probe_error_is_capability_unavailable(error: &str, parameter: &str) -> bool {
    probe_error_rejects_parameter(error, parameter) || {
        let error = error.to_ascii_lowercase();
        error.contains(parameter)
            && ["not implemented", "cannot be used", "is disabled"]
                .iter()
                .any(|marker| error.contains(marker))
    }
}

async fn send_probe_with_output_fallback(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    profile: &mut api::ModelProfile,
    include_reasoning: bool,
) -> std::result::Result<Value, String> {
    match send_probe_request(
        client,
        provider,
        endpoint,
        api_key,
        model,
        profile,
        include_reasoning,
    )
    .await
    {
        Ok(value) => Ok(value),
        Err(error)
            if profile.output_token_parameter == "max_completion_tokens"
                && probe_error_rejects_parameter(&error, "max_completion_tokens") =>
        {
            profile.output_token_parameter = "max_tokens".to_string();
            send_probe_request(
                client,
                provider,
                endpoint,
                api_key,
                model,
                profile,
                include_reasoning,
            )
            .await
        }
        Err(error)
            if profile.output_token_parameter == "max_tokens"
                && probe_error_rejects_parameter(&error, "max_tokens") =>
        {
            profile.output_token_parameter = "max_completion_tokens".to_string();
            send_probe_request(
                client,
                provider,
                endpoint,
                api_key,
                model,
                profile,
                include_reasoning,
            )
            .await
        }
        Err(error) => Err(error),
    }
}

async fn probe_tool_calling(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    profile: &api::ModelProfile,
) -> std::result::Result<bool, String> {
    let mut body = if provider.eq_ignore_ascii_case("anthropic") {
        json!({
            "model": model.trim_start_matches("anthropic/"),
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "Call the probe tool."}],
            "tools": [{
                "name": "aos_capability_probe",
                "description": "Return a fixed capability probe result",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }],
            "tool_choice": {"type": "tool", "name": "aos_capability_probe"}
        })
    } else {
        let mut body = json!({
            "model": model,
            "messages": [{"role": "user", "content": "Call the probe tool."}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "aos_capability_probe",
                    "description": "Return a fixed capability probe result",
                    "parameters": {
                        "type": "object",
                        "properties": {},
                        "additionalProperties": false
                    }
                }
            }],
            "tool_choice": {
                "type": "function",
                "function": {"name": "aos_capability_probe"}
            }
        });
        body[profile.output_token_parameter.as_str()] = json!(64);
        body
    };
    let response = match send_probe_body(client, provider, endpoint, api_key, &body).await {
        Ok(response) => response,
        Err(error)
            if !provider.eq_ignore_ascii_case("anthropic")
                && probe_error_is_capability_unavailable(&error, "tool_choice") =>
        {
            // Reasoning models such as DeepSeek's thinking variants can support
            // tools while rejecting a forced tool choice. Probe the actual tool
            // capability again with automatic selection instead of incorrectly
            // marking the entire model validation as failed.
            if let Some(object) = body.as_object_mut() {
                object.remove("tool_choice");
            }
            if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
                if let Some(message) = messages.first_mut() {
                    message["content"] =
                        json!("You must call aos_capability_probe now. Do not answer with text.");
                }
            }
            send_probe_body(client, provider, endpoint, api_key, &body).await?
        }
        Err(error) => return Err(error),
    };
    let has_openai_tool_call = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    let has_anthropic_tool_call = response
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| {
            content
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        });
    Ok(has_openai_tool_call || has_anthropic_tool_call)
}

#[derive(Debug, Default)]
struct StructuredOutputProbe {
    json_object: Option<bool>,
    json_schema: Option<bool>,
    strict_json_schema: Option<bool>,
    warnings: Vec<String>,
}

fn structured_probe_content_is_valid(response: &Value) -> bool {
    response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .and_then(|content| serde_json::from_str::<Value>(content).ok())
        .is_some_and(|value| value.get("ok").and_then(Value::as_bool) == Some(true))
}

async fn probe_structured_format(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    profile: &api::ModelProfile,
    format_type: &str,
) -> std::result::Result<bool, String> {
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Return exactly one JSON object with a boolean field named ok set to true."}],
    });
    body["response_format"] = if format_type == "json_schema" {
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "aos_capability_probe",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {"ok": {"type": "boolean"}},
                    "required": ["ok"],
                    "additionalProperties": false
                }
            }
        })
    } else {
        json!({"type": "json_object"})
    };
    body[profile.output_token_parameter.as_str()] = json!(32);
    match send_probe_body(client, provider, endpoint, api_key, &body).await {
        Ok(response) => Ok(structured_probe_content_is_valid(&response)),
        Err(error)
            if provider.eq_ignore_ascii_case("deepseek")
                && error.to_ascii_lowercase().contains("thinking") =>
        {
            body["thinking"] = json!({"type": "disabled"});
            match send_probe_body(client, provider, endpoint, api_key, &body).await {
                Ok(response) => Ok(structured_probe_content_is_valid(&response)),
                Err(disabled_error)
                    if probe_error_rejects_parameter(&disabled_error, "thinking") =>
                {
                    body.as_object_mut()
                        .expect("probe body object")
                        .remove("thinking");
                    body["enable_thinking"] = json!(false);
                    send_probe_body(client, provider, endpoint, api_key, &body)
                        .await
                        .map(|response| structured_probe_content_is_valid(&response))
                }
                Err(disabled_error) => Err(disabled_error),
            }
        }
        Err(error) => Err(error),
    }
}

async fn probe_structured_output(
    client: &Client,
    provider: &str,
    endpoint: &str,
    api_key: &str,
    model: &str,
    profile: &api::ModelProfile,
) -> StructuredOutputProbe {
    if provider.eq_ignore_ascii_case("anthropic") {
        return StructuredOutputProbe::default();
    }
    let mut result = StructuredOutputProbe::default();
    match probe_structured_format(
        client,
        provider,
        endpoint,
        api_key,
        model,
        profile,
        "json_schema",
    )
    .await
    {
        Ok(true) => {
            result.json_schema = Some(true);
            result.strict_json_schema = Some(true);
            result.json_object = Some(true);
            return result;
        }
        Ok(false) => result
            .warnings
            .push("json_schema probe returned invalid structured content".to_string()),
        Err(error) if probe_error_is_capability_unavailable(&error, "response_format") => {
            result.json_schema = Some(false);
            result.strict_json_schema = Some(false);
        }
        Err(error) => {
            result
                .warnings
                .push(format!("json_schema probe failed transiently: {error}"));
        }
    }
    match probe_structured_format(
        client,
        provider,
        endpoint,
        api_key,
        model,
        profile,
        "json_object",
    )
    .await
    {
        Ok(true) => result.json_object = Some(true),
        Ok(false) => result
            .warnings
            .push("json_object probe returned invalid structured content".to_string()),
        Err(error) if probe_error_is_capability_unavailable(&error, "response_format") => {
            result.json_object = Some(false);
        }
        Err(error) => result
            .warnings
            .push(format!("json_object probe failed transiently: {error}")),
    }
    result
}

async fn probe_model(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ProbeModelRequest>,
) -> Result<Json<ProbeModelResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:write").await?;
    let model_type = require_chat_model_type(req.model_type.as_deref())?;
    if req.model.trim().is_empty() {
        return Err(AppError::ValidationError("model is required".into()));
    }
    let started = std::time::Instant::now();
    let api_key = resolve_probe_api_key(
        &state,
        &claims.tenant_id,
        req.api_key.as_deref(),
        req.existing_key_id.as_deref(),
    )
    .await?;
    let base = effective_provider_base_url(&req.provider, req.base_url.as_deref())?;
    let mut profile = api::infer_model_profile(&req.provider, Some(&base), &req.model);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(if req.full {
            45
        } else {
            20
        }))
        .build()
        .map_err(|error| AppError::Internal(format!("HTTP client error: {error}")))?;
    let endpoints = if req.provider.eq_ignore_ascii_case("anthropic") {
        let base = strip_provider_endpoint_suffix(&base);
        if base.to_ascii_lowercase().ends_with("/v1") {
            vec![format!("{base}/messages")]
        } else {
            vec![format!("{base}/v1/messages")]
        }
    } else {
        chat_completion_endpoint_candidates(&base)
    };
    let mut last_error = String::new();
    let mut successful_endpoint = None;
    for endpoint in endpoints {
        match send_probe_with_output_fallback(
            &client,
            &req.provider,
            &endpoint,
            &api_key,
            &req.model,
            &mut profile,
            false,
        )
        .await
        {
            Ok(_) => {
                successful_endpoint = Some(endpoint);
                break;
            }
            Err(error) => last_error = error,
        }
    }
    let endpoint = successful_endpoint.ok_or_else(|| {
        AppError::ValidationError(if last_error.is_empty() {
            "model probe failed without a provider response".to_string()
        } else {
            last_error
        })
    })?;
    let mut checks = vec!["base_request".to_string()];
    let mut warnings = Vec::new();
    if req.full && profile.reasoning.is_some() {
        match send_probe_with_output_fallback(
            &client,
            &req.provider,
            &endpoint,
            &api_key,
            &req.model,
            &mut profile,
            true,
        )
        .await
        {
            Ok(_) => checks.push("reasoning_parameter".to_string()),
            Err(error) => {
                let rejected_parameter =
                    profile.reasoning.as_ref().map(|reasoning| {
                        match reasoning.transport.as_str() {
                            "anthropic_thinking" => "thinking",
                            "thinking_level" => "thinking_config",
                            "enable_thinking" => "enable_thinking",
                            _ => "reasoning_effort",
                        }
                    });
                if rejected_parameter
                    .is_some_and(|parameter| probe_error_rejects_parameter(&error, parameter))
                {
                    profile.reasoning = None;
                    profile.features.reasoning_output = Some(false);
                }
                warnings.push(format!(
                    "base request passed, reasoning probe failed: {error}"
                ));
            }
        }
    }
    if req.full {
        match probe_tool_calling(
            &client,
            &req.provider,
            &endpoint,
            &api_key,
            &req.model,
            &profile,
        )
        .await
        {
            Ok(true) => {
                profile.features.tools = Some(true);
                checks.push("tool_calling".to_string());
            }
            Ok(false) => warnings
                .push("tool probe returned no tool call; capability remains unchanged".to_string()),
            Err(error) => {
                if probe_error_is_capability_unavailable(&error, "tools")
                    || probe_error_is_capability_unavailable(&error, "tool_choice")
                {
                    profile.features.tools = Some(false);
                    checks.push("tool_calling_unsupported".to_string());
                } else {
                    warnings.push(format!("tool probe failed: {error}"));
                }
            }
        }
        let structured_probe = probe_structured_output(
            &client,
            &req.provider,
            &endpoint,
            &api_key,
            &req.model,
            &profile,
        )
        .await;
        profile.features.json_object = structured_probe.json_object;
        profile.features.json_schema = structured_probe.json_schema;
        profile.features.strict_json_schema = structured_probe.strict_json_schema;
        profile.features.structured_output = if structured_probe.json_object == Some(true)
            || structured_probe.json_schema == Some(true)
        {
            Some(true)
        } else if structured_probe.json_object == Some(false)
            && structured_probe.json_schema == Some(false)
        {
            Some(false)
        } else {
            None
        };
        profile.features.structured_output_verified =
            Some(structured_probe.json_object.is_some() || structured_probe.json_schema.is_some());
        profile.features.structured_output_verified_mode = Some("non_thinking".to_string());
        profile.features.structured_output_verified_at = Some(chrono::Utc::now().to_rfc3339());
        if profile.features.json_schema == Some(true) {
            checks.push("strict_json_schema".to_string());
        } else if profile.features.json_schema == Some(false) {
            checks.push("strict_json_schema_unsupported".to_string());
        }
        if profile.features.json_object == Some(true) {
            checks.push("json_object".to_string());
        } else if profile.features.json_object == Some(false) {
            checks.push("json_object_unsupported".to_string());
        }
        warnings.extend(structured_probe.warnings);
    }
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    let confidence = if req.full && warning.is_none() {
        "high"
    } else {
        "medium"
    };
    let observations = json!({
        "checks": checks,
        "endpoint": endpoint,
        "full": req.full,
        "warning": warning,
    });
    let profile_id = upsert_model_profile(
        &state,
        &claims.tenant_id,
        &req.provider,
        Some(&base),
        &req.model,
        model_type,
        &profile,
        Some("probe"),
        Some(confidence),
        "verified",
        Some(&observations),
        warning.as_deref(),
    )
    .await?;
    publish_linked_model_profile_change(&state, &claims.tenant_id, &profile_id).await?;
    Ok(Json(ProbeModelResponse {
        ok: true,
        latency_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        endpoint,
        profile: profile.runtime_capabilities(),
        source: "probe".to_string(),
        confidence: confidence.to_string(),
        checks,
        warning,
    }))
}

async fn backfill_model_profiles_for_tenant(state: &AppState, tenant_id: &str) -> Result<()> {
    let rows = sqlx::query(
        "SELECT id, provider, base_url, model
         FROM api_keys
         WHERE tenant_id = ? AND model_type = 'chat'
           AND model IS NOT NULL AND TRIM(model) <> ''
           AND model_profile_id IS NULL",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;
    let mut linked_any = false;
    for row in rows {
        let key_id: String = sqlx::Row::get(&row, "id");
        let provider: String = sqlx::Row::get(&row, "provider");
        let base_url: Option<String> = sqlx::Row::get(&row, "base_url");
        let model: String = sqlx::Row::get(&row, "model");
        let profile = api::infer_model_profile(&provider, base_url.as_deref(), &model);
        let profile_id = upsert_model_profile(
            state,
            tenant_id,
            &provider,
            base_url.as_deref(),
            &model,
            "chat",
            &profile,
            None,
            None,
            "inferred",
            None,
            None,
        )
        .await?;
        let result = sqlx::query(
            "UPDATE api_keys SET model_profile_id = ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ? AND tenant_id = ? AND model_profile_id IS NULL",
        )
        .bind(profile_id)
        .bind(key_id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
        linked_any |= result.rows_affected() > 0;
    }
    if linked_any {
        sqlx::query("UPDATE tenants SET api_keys_version = api_keys_version + 1 WHERE id = ?")
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
        if let Some(registry) = state.config_registry.as_ref() {
            registry.invalidate_tenant_cache(tenant_id).await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiKeyListResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:read").await?;
    backfill_model_profiles_for_tenant(&state, &claims.tenant_id).await?;
    let rows = sqlx::query(
        "SELECT k.id, k.name, k.provider, k.base_url, k.model,
                CAST(k.dimensions AS INTEGER) AS dimensions,
                k.audio_generate_path, k.audio_query_path, k.model_type, k.key_hint,
                k.encrypted_key, k.daily_limit, k.monthly_limit, k.enabled, k.priority,
                k.is_primary,
                CAST(k.input_price_per_million AS DOUBLE) AS input_price_per_million,
                CAST(k.output_price_per_million AS DOUBLE) AS output_price_per_million,
                CAST(k.scenarios AS TEXT) AS scenarios_json,
                CAST(k.capabilities_json AS TEXT) AS capabilities_json,
                k.created_at,
                p.id AS profile_id, p.protocol AS profile_protocol,
                p.source AS profile_source, p.confidence AS profile_confidence,
                p.detection_status AS profile_detection_status,
                p.detected_at AS profile_detected_at, p.expires_at AS profile_expires_at,
                CAST(p.capabilities_json AS TEXT) AS profile_capabilities_json
         FROM api_keys k
         LEFT JOIN model_capability_profiles p
           ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
         WHERE k.tenant_id = ?
         ORDER BY k.priority ASC, k.created_at ASC",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;

    let records: Vec<ApiKeyRecord> = rows
        .into_iter()
        .map(|row| {
            let scenarios_json: Option<String> = sqlx::Row::get(&row, "scenarios_json");
            let scenarios: Option<Vec<String>> = scenarios_json
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok());
            let capabilities_json = parse_json_object(sqlx::Row::get(&row, "capabilities_json"));
            let profile_capabilities =
                parse_json_object(sqlx::Row::get(&row, "profile_capabilities_json"));
            let resolved_capabilities =
                merge_capability_values(profile_capabilities, capabilities_json.clone());
            let profile_id: Option<String> = sqlx::Row::get(&row, "profile_id");
            let model_profile = profile_id.map(|id| ModelProfileSummary {
                id,
                protocol: sqlx::Row::get(&row, "profile_protocol"),
                source: sqlx::Row::get(&row, "profile_source"),
                confidence: sqlx::Row::get(&row, "profile_confidence"),
                detection_status: sqlx::Row::get(&row, "profile_detection_status"),
                detected_at: sqlx::Row::get(&row, "profile_detected_at"),
                expires_at: sqlx::Row::get(&row, "profile_expires_at"),
            });
            let created_at: DateTime<Utc> = sqlx::Row::get(&row, "created_at");
            let id: String = sqlx::Row::get(&row, "id");
            let enabled: bool = sqlx::Row::get(&row, "enabled");
            let encrypted_key: String = sqlx::Row::get(&row, "encrypted_key");
            let (runtime_available, runtime_error) =
                api_key_runtime_status(enabled, &encrypted_key, &claims.tenant_id, &id);
            ApiKeyRecord {
                id,
                name: sqlx::Row::get(&row, "name"),
                provider: sqlx::Row::get(&row, "provider"),
                base_url: sqlx::Row::get(&row, "base_url"),
                model: sqlx::Row::get(&row, "model"),
                dimensions: sqlx::Row::get(&row, "dimensions"),
                audio_generate_path: sqlx::Row::get(&row, "audio_generate_path"),
                audio_query_path: sqlx::Row::get(&row, "audio_query_path"),
                model_type: sqlx::Row::get(&row, "model_type"),
                key_hint: sqlx::Row::get(&row, "key_hint"),
                daily_limit: sqlx::Row::get(&row, "daily_limit"),
                monthly_limit: sqlx::Row::get(&row, "monthly_limit"),
                enabled,
                priority: sqlx::Row::get(&row, "priority"),
                is_primary: sqlx::Row::get(&row, "is_primary"),
                input_price_per_million: sqlx::Row::get(&row, "input_price_per_million"),
                output_price_per_million: sqlx::Row::get(&row, "output_price_per_million"),
                scenarios,
                capabilities_json,
                resolved_capabilities,
                model_profile,
                runtime_available,
                runtime_error,
                created_at: created_at.to_rfc3339(),
            }
        })
        .collect();

    let total = i64::try_from(records.len()).unwrap_or(i64::MAX);
    Ok(Json(ApiKeyListResponse {
        keys: records,
        total,
    }))
}

async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateApiKeyRequest>,
) -> Result<Json<ApiKeyCreatedResponse>> {
    require_apikey_permission(&state, &claims, "apikeys:write").await?;
    if req.name.is_empty() {
        return Err(AppError::ValidationError("name is required".into()));
    }
    if req.key_value.is_empty() {
        return Err(AppError::ValidationError("key_value is required".into()));
    }
    if req.provider == "custom" && req.base_url.as_deref().map_or(true, str::is_empty) {
        return Err(AppError::ValidationError(
            "base_url is required when provider is 'custom'".into(),
        ));
    }

    // Normalise model_type: default to 'chat' if not provided or invalid.
    let model_type = match req.model_type.as_deref().map(str::trim) {
        Some(mt) if is_allowed_model_type(mt) => mt.to_string(),
        _ => "chat".to_owned(),
    };
    if req
        .dimensions
        .is_some_and(|value| !(1..=32_768).contains(&value))
    {
        return Err(AppError::ValidationError(
            "dimensions must be between 1 and 32768".into(),
        ));
    }
    if model_type != "embedding" && req.dimensions.is_some() {
        return Err(AppError::ValidationError(
            "dimensions is only valid for embedding model_type".into(),
        ));
    }
    let normalized_model = if model_type == "audio" {
        Some(normalize_audio_model_value(req.model.as_deref()))
    } else {
        req.model
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
    };
    let normalized_audio_generate_path = if model_type == "audio" {
        normalize_audio_endpoint_path(req.audio_generate_path.as_deref())
    } else {
        None
    };
    let normalized_audio_query_path = if model_type == "audio" {
        normalize_audio_endpoint_path(req.audio_query_path.as_deref())
    } else {
        None
    };
    if model_type == "audio"
        && (normalized_audio_generate_path.is_none() || normalized_audio_query_path.is_none())
    {
        return Err(AppError::ValidationError(
            "audio_generate_path and audio_query_path are required for audio model_type".into(),
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let priority = req.priority.unwrap_or(0);
    let is_primary = true; // First key is always primary
    let key_hint: String = req
        .key_value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let key_hash = bcrypt::hash(&req.key_value, bcrypt::DEFAULT_COST)?;
    let encrypted_key = crypto::encrypt_scoped(
        &req.key_value,
        &crypto::scoped_aad("api_keys.encrypted_key", &claims.tenant_id, &id),
    )
    .map_err(|e| AppError::Internal(format!("encryption failed: {e}")))?;

    let scenarios_json =
        normalize_api_key_scenarios_for_model_type(&model_type, req.scenarios.as_ref())?;
    if model_type != "chat"
        && req
            .capabilities_json
            .as_ref()
            .is_some_and(|value| !value.is_null())
    {
        return Err(AppError::ValidationError(
            "capabilities_json is only valid for chat model_type".into(),
        ));
    }
    let capabilities_json = if model_type == "chat" {
        normalize_api_key_capabilities(req.capabilities_json.as_ref())?
    } else {
        None
    };
    let model_profile_id = if model_type == "chat" {
        if let Some(model) = normalized_model.as_deref() {
            let profile = api::infer_model_profile(&req.provider, req.base_url.as_deref(), model);
            Some(
                upsert_model_profile(
                    &state,
                    &claims.tenant_id,
                    &req.provider,
                    req.base_url.as_deref(),
                    model,
                    &model_type,
                    &profile,
                    None,
                    None,
                    "inferred",
                    None,
                    None,
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };

    sqlx::query(
        "INSERT INTO api_keys
            (id, tenant_id, name, provider, base_url, model, dimensions,
             audio_generate_path, audio_query_path, model_type, key_hash, key_hint,
             encrypted_key, daily_limit, monthly_limit, priority, is_primary,
             input_price_per_million, output_price_per_million, scenarios,
             capabilities_json, model_profile_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&req.name)
    .bind(&req.provider)
    .bind(&req.base_url)
    .bind(&normalized_model)
    .bind(req.dimensions)
    .bind(&normalized_audio_generate_path)
    .bind(&normalized_audio_query_path)
    .bind(&model_type)
    .bind(&key_hash)
    .bind(&key_hint)
    .bind(&encrypted_key)
    .bind(req.daily_limit)
    .bind(req.monthly_limit)
    .bind(priority)
    .bind(is_primary)
    .bind(req.input_price_per_million)
    .bind(req.output_price_per_million)
    .bind(Some(scenarios_json))
    .bind(capabilities_json)
    .bind(model_profile_id)
    .execute(&state.db)
    .await?;

    // Increment the tenant's api_keys_version so existing sessions detect the change
    // on their next turn and hot-reload with the fresh key set.
    sqlx::query("UPDATE tenants SET api_keys_version = api_keys_version + 1 WHERE id = ?")
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(registry) = state.config_registry.as_ref() {
        registry.invalidate_tenant_cache(&claims.tenant_id).await;
    }
    reconcile_embedding_profiles(&state, &claims.tenant_id).await;

    Ok(Json(ApiKeyCreatedResponse {
        id,
        name: req.name,
        provider: req.provider,
        key_hint,
    }))
}

async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    require_apikey_permission(&state, &claims, "apikeys:delete").await?;
    let result = sqlx::query("DELETE FROM api_keys WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("api key not found".into()));
    }

    // Increment the tenant's api_keys_version so existing sessions detect the change
    // on their next turn and hot-reload with the fresh key set.
    sqlx::query("UPDATE tenants SET api_keys_version = api_keys_version + 1 WHERE id = ?")
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(registry) = state.config_registry.as_ref() {
        registry.invalidate_tenant_cache(&claims.tenant_id).await;
    }
    reconcile_embedding_profiles(&state, &claims.tenant_id).await;

    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[allow(clippy::too_many_lines)]
async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateApiKeyRequest>,
) -> Result<Json<ApiKeyRecord>> {
    require_apikey_permission(&state, &claims, "apikeys:write").await?;
    // Validate model_type value.
    if let Some(ref mt) = req.model_type {
        if !is_allowed_model_type(mt) {
            return Err(AppError::ValidationError(
                "model_type must be one of: 'chat', 'embedding', 'image', 'video', 'audio'".into(),
            ));
        }
    }

    // Validate: for custom providers, reject only explicit empty base_url updates.
    // This keeps partial updates (e.g. {"enabled": false}) working without forcing
    // callers to resend unrelated fields.
    let existing_provider: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT provider, model_type, audio_generate_path, audio_query_path, base_url, model
         FROM api_keys WHERE id = ? AND tenant_id = ?",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;
    let mut existing_model_type = None::<String>;
    let mut existing_audio_generate_path = None::<String>;
    let mut existing_audio_query_path = None::<String>;
    let mut existing_provider_name = None::<String>;
    let mut existing_base_url = None::<String>;
    let mut existing_model = None::<String>;
    if let Some((
        provider,
        model_type_existing,
        audio_generate_path,
        audio_query_path,
        base_url,
        model,
    )) = existing_provider
    {
        existing_provider_name = Some(provider.clone());
        existing_model_type = Some(model_type_existing.clone());
        existing_audio_generate_path = audio_generate_path;
        existing_audio_query_path = audio_query_path;
        existing_base_url = base_url;
        existing_model = model;
        if provider == "custom" {
            let base_url_sent = req.base_url.as_deref();
            if base_url_sent.is_some_and(str::is_empty) {
                return Err(AppError::ValidationError(
                    "base_url is required when provider is 'custom'".into(),
                ));
            }
        }
    }

    // Process key_value early so we can borrow req afterwards.
    // If the caller sends an empty string it means "clear the key" — treat it as a real
    // new value. If it sends undefined/null it means "leave the existing key unchanged".
    let (key_hash, key_hint, encrypted_key) = if let Some(ref key_value) = req.key_value {
        let hint: String = key_value
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let hash = bcrypt::hash(key_value, bcrypt::DEFAULT_COST)?;
        let encrypted = crypto::encrypt_scoped(
            key_value,
            &crypto::scoped_aad("api_keys.encrypted_key", &claims.tenant_id, &id),
        )
        .map_err(|e| AppError::Internal(format!("encryption failed: {e}")))?;
        (Some(hash), Some(hint), Some(encrypted))
    } else {
        (None, None, None)
    };

    let normalized_model_type = req.model_type.clone();
    let target_model_type = normalized_model_type
        .as_deref()
        .map(str::to_string)
        .or(existing_model_type);
    let target_model_type_value = target_model_type.as_deref().unwrap_or("chat");
    let target_is_audio = target_model_type.as_deref() == Some("audio");
    let target_is_embedding = target_model_type.as_deref() == Some("embedding");
    if req
        .dimensions
        .is_some_and(|value| !(1..=32_768).contains(&value))
    {
        return Err(AppError::ValidationError(
            "dimensions must be between 1 and 32768".into(),
        ));
    }
    if !target_is_embedding && req.dimensions.is_some() {
        return Err(AppError::ValidationError(
            "dimensions is only valid for embedding model_type".into(),
        ));
    }
    let normalized_model = if target_model_type.as_deref() == Some("audio") {
        req.model
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| normalize_audio_model_value(Some(v)))
            .or_else(|| {
                if req.model_type.is_some() {
                    Some(normalize_audio_model_value(None))
                } else {
                    None
                }
            })
    } else {
        req.model
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
    };
    let normalized_audio_generate_path = if target_is_audio {
        req.audio_generate_path
            .as_deref()
            .and_then(|v| normalize_audio_endpoint_path(Some(v)))
    } else {
        None
    };
    let normalized_audio_query_path = if target_is_audio {
        req.audio_query_path
            .as_deref()
            .and_then(|v| normalize_audio_endpoint_path(Some(v)))
    } else {
        None
    };
    if target_is_audio {
        let effective_generate = normalized_audio_generate_path
            .clone()
            .or_else(|| existing_audio_generate_path.clone());
        let effective_query = normalized_audio_query_path
            .clone()
            .or_else(|| existing_audio_query_path.clone());
        if effective_generate.is_none() || effective_query.is_none() {
            return Err(AppError::ValidationError(
                "audio_generate_path and audio_query_path are required for audio model_type".into(),
            ));
        }
    }
    let should_refresh_model_profile =
        req.model.is_some() || req.base_url.is_some() || req.model_type.is_some();
    let clear_fields = api_key_update_clears(
        req.model_type.as_deref(),
        target_model_type_value,
        should_refresh_model_profile,
    );
    if target_model_type_value != "chat"
        && req
            .capabilities_json
            .as_ref()
            .is_some_and(|value| !value.is_null())
    {
        return Err(AppError::ValidationError(
            "capabilities_json is only valid for chat model_type".into(),
        ));
    }
    let model_profile_id = if should_refresh_model_profile
        && target_model_type.as_deref() == Some("chat")
    {
        let effective_provider = existing_provider_name
            .as_deref()
            .ok_or_else(|| AppError::NotFound("api key not found".into()))?;
        let effective_model = normalized_model
            .as_deref()
            .or(existing_model.as_deref())
            .ok_or_else(|| AppError::ValidationError("model is required for chat keys".into()))?;
        let effective_base_url = req.base_url.as_deref().or(existing_base_url.as_deref());
        let profile =
            api::infer_model_profile(effective_provider, effective_base_url, effective_model);
        Some(
            upsert_model_profile(
                &state,
                &claims.tenant_id,
                effective_provider,
                effective_base_url,
                effective_model,
                "chat",
                &profile,
                None,
                None,
                "inferred",
                None,
                None,
            )
            .await?,
        )
    } else {
        None
    };
    let (
        name,
        base_url,
        model,
        dimensions,
        audio_generate_path,
        audio_query_path,
        model_type,
        daily_limit,
        monthly_limit,
        enabled,
        priority,
        input_price,
        output_price,
        scenarios,
        capabilities_json,
    ) = (
        req.name.clone(),
        req.base_url.clone(),
        normalized_model,
        req.dimensions,
        normalized_audio_generate_path,
        normalized_audio_query_path,
        normalized_model_type,
        req.daily_limit,
        req.monthly_limit,
        req.enabled,
        req.priority,
        req.input_price_per_million,
        req.output_price_per_million,
        req.scenarios.clone(),
        req.capabilities_json.clone(),
    );

    let mut set_clauses: Vec<&'static str> = Vec::new();

    if name.is_some() {
        set_clauses.push("name = ?");
    }
    if base_url.is_some() {
        set_clauses.push("base_url = ?");
    }
    if model.is_some() {
        set_clauses.push("model = ?");
    }
    if clear_fields.dimensions {
        set_clauses.push("dimensions = NULL");
    } else if dimensions.is_some() {
        set_clauses.push("dimensions = ?");
    }
    if model_type.is_some() {
        set_clauses.push("model_type = ?");
    }
    if clear_fields.audio_paths {
        set_clauses.push("audio_generate_path = NULL");
    } else if audio_generate_path.is_some() {
        set_clauses.push("audio_generate_path = ?");
    }
    if clear_fields.audio_paths {
        set_clauses.push("audio_query_path = NULL");
    } else if audio_query_path.is_some() {
        set_clauses.push("audio_query_path = ?");
    }
    if daily_limit.is_some() {
        set_clauses.push("daily_limit = ?");
    }
    if monthly_limit.is_some() {
        set_clauses.push("monthly_limit = ?");
    }
    if enabled.is_some() {
        set_clauses.push("enabled = ?");
    }
    if priority.is_some() {
        set_clauses.push("priority = ?");
    }
    if input_price.is_some() {
        set_clauses.push("input_price_per_million = ?");
    }
    if output_price.is_some() {
        set_clauses.push("output_price_per_million = ?");
    }
    if key_hash.is_some() {
        set_clauses.push("key_hash = ?");
    }
    if key_hint.is_some() {
        set_clauses.push("key_hint = ?");
    }
    if encrypted_key.is_some() {
        set_clauses.push("encrypted_key = ?");
    }
    if scenarios.is_some() || target_is_embedding {
        set_clauses.push("scenarios = ?");
    }
    if clear_fields.chat_capabilities {
        set_clauses.push("capabilities_json = NULL");
    } else if capabilities_json.is_some() {
        set_clauses.push("capabilities_json = ?");
    }
    if clear_fields.model_profile {
        set_clauses.push("model_profile_id = NULL");
    } else if model_profile_id.is_some() {
        set_clauses.push("model_profile_id = ?");
    }

    if set_clauses.is_empty() {
        return Err(AppError::ValidationError("no fields to update".into()));
    }

    let set_sql = set_clauses.join(", ");
    let query = format!("UPDATE api_keys SET {set_sql} WHERE id = ? AND tenant_id = ?");

    let mut q = sqlx::query(&query);

    if let Some(v) = &name {
        q = q.bind(v);
    }
    if let Some(v) = &base_url {
        q = q.bind(v);
    }
    if let Some(v) = &model {
        q = q.bind(v);
    }
    if !clear_fields.dimensions {
        if let Some(v) = dimensions {
            q = q.bind(v);
        }
    }
    if let Some(v) = &model_type {
        q = q.bind(v);
    }
    if !clear_fields.audio_paths {
        if let Some(v) = &audio_generate_path {
            q = q.bind(v);
        }
        if let Some(v) = &audio_query_path {
            q = q.bind(v);
        }
    }
    if let Some(v) = daily_limit {
        q = q.bind(v);
    }
    if let Some(v) = monthly_limit {
        q = q.bind(v);
    }
    if let Some(v) = enabled {
        q = q.bind(v);
    }
    if let Some(v) = priority {
        q = q.bind(v);
    }
    if let Some(v) = input_price {
        q = q.bind(v);
    }
    if let Some(v) = output_price {
        q = q.bind(v);
    }
    if let Some(v) = key_hash {
        q = q.bind(v);
    }
    if let Some(v) = key_hint {
        q = q.bind(v);
    }
    if let Some(v) = encrypted_key {
        q = q.bind(v);
    }
    if target_is_embedding {
        q = q.bind(Some(EMBEDDING_ALL_SCENARIOS_JSON.to_string()));
    } else if let Some(scenarios_vec) = scenarios {
        let json = normalize_api_key_scenarios(Some(&scenarios_vec))?;
        q = q.bind(Some(json));
    }
    if !clear_fields.chat_capabilities {
        if let Some(capabilities) = capabilities_json {
            let json = normalize_api_key_capabilities(Some(&capabilities))?;
            q = q.bind(json);
        }
    }
    if !clear_fields.model_profile {
        if let Some(model_profile_id) = model_profile_id {
            q = q.bind(model_profile_id);
        }
    }

    q = q.bind(&id).bind(&claims.tenant_id);

    let result = q.execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("api key not found".into()));
    }

    // Increment the tenant's api_keys_version so existing sessions detect the change
    // on their next turn and hot-reload with the fresh key set.
    sqlx::query("UPDATE tenants SET api_keys_version = api_keys_version + 1 WHERE id = ?")
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(registry) = state.config_registry.as_ref() {
        registry.invalidate_tenant_cache(&claims.tenant_id).await;
    }
    reconcile_embedding_profiles(&state, &claims.tenant_id).await;

    // Fetch updated record
    let key = sqlx::query(
        "SELECT k.id, k.name, k.provider, k.base_url, k.model,
                CAST(k.dimensions AS INTEGER) AS dimensions,
                k.audio_generate_path, k.audio_query_path, k.model_type, k.key_hint,
                k.encrypted_key, k.daily_limit, k.monthly_limit, k.enabled, k.priority,
                k.is_primary,
                CAST(k.input_price_per_million AS DOUBLE) AS input_price_per_million,
                CAST(k.output_price_per_million AS DOUBLE) AS output_price_per_million,
                CAST(k.scenarios AS TEXT) AS scenarios_json,
                CAST(k.capabilities_json AS TEXT) AS capabilities_json,
                k.created_at,
                p.id AS profile_id, p.protocol AS profile_protocol,
                p.source AS profile_source, p.confidence AS profile_confidence,
                p.detection_status AS profile_detection_status,
                p.detected_at AS profile_detected_at, p.expires_at AS profile_expires_at,
                CAST(p.capabilities_json AS TEXT) AS profile_capabilities_json
         FROM api_keys k
         LEFT JOIN model_capability_profiles p
           ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
         WHERE k.id = ? AND k.tenant_id = ?",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;

    match key {
        Some(row) => {
            let scenarios_json: Option<String> = sqlx::Row::get(&row, "scenarios_json");
            let scenarios: Option<Vec<String>> = scenarios_json
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok());
            let capabilities_json = parse_json_object(sqlx::Row::get(&row, "capabilities_json"));
            let resolved_capabilities = merge_capability_values(
                parse_json_object(sqlx::Row::get(&row, "profile_capabilities_json")),
                capabilities_json.clone(),
            );
            let profile_id: Option<String> = sqlx::Row::get(&row, "profile_id");
            let model_profile = profile_id.map(|profile_id| ModelProfileSummary {
                id: profile_id,
                protocol: sqlx::Row::get(&row, "profile_protocol"),
                source: sqlx::Row::get(&row, "profile_source"),
                confidence: sqlx::Row::get(&row, "profile_confidence"),
                detection_status: sqlx::Row::get(&row, "profile_detection_status"),
                detected_at: sqlx::Row::get(&row, "profile_detected_at"),
                expires_at: sqlx::Row::get(&row, "profile_expires_at"),
            });
            let created_at: DateTime<Utc> = sqlx::Row::get(&row, "created_at");
            let row_id: String = sqlx::Row::get(&row, "id");
            let enabled: bool = sqlx::Row::get(&row, "enabled");
            let encrypted_key: String = sqlx::Row::get(&row, "encrypted_key");
            let (runtime_available, runtime_error) =
                api_key_runtime_status(enabled, &encrypted_key, &claims.tenant_id, &row_id);
            Ok(Json(ApiKeyRecord {
                id: row_id,
                name: sqlx::Row::get(&row, "name"),
                provider: sqlx::Row::get(&row, "provider"),
                base_url: sqlx::Row::get(&row, "base_url"),
                model: sqlx::Row::get(&row, "model"),
                dimensions: sqlx::Row::get(&row, "dimensions"),
                audio_generate_path: sqlx::Row::get(&row, "audio_generate_path"),
                audio_query_path: sqlx::Row::get(&row, "audio_query_path"),
                model_type: sqlx::Row::get(&row, "model_type"),
                key_hint: sqlx::Row::get(&row, "key_hint"),
                daily_limit: sqlx::Row::get(&row, "daily_limit"),
                monthly_limit: sqlx::Row::get(&row, "monthly_limit"),
                enabled,
                priority: sqlx::Row::get(&row, "priority"),
                is_primary: sqlx::Row::get(&row, "is_primary"),
                input_price_per_million: sqlx::Row::get(&row, "input_price_per_million"),
                output_price_per_million: sqlx::Row::get(&row, "output_price_per_million"),
                scenarios,
                capabilities_json,
                resolved_capabilities,
                model_profile,
                runtime_available,
                runtime_error,
                created_at: created_at.to_rfc3339(),
            }))
        }
        None => Err(AppError::NotFound("api key not found".into())),
    }
}

async fn stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key_id): Path<String>,
) -> Result<Json<ApiKeyStats>> {
    require_apikey_permission(&state, &claims, "apikeys:read").await?;
    // Verify the key belongs to the tenant
    let key_exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM api_keys WHERE id = ? AND tenant_id = ?")
            .bind(&key_id)
            .bind(&claims.tenant_id)
            .fetch_optional(&state.db)
            .await?;

    if key_exists.is_none() {
        return Err(AppError::NotFound("api key not found".into()));
    }

    let row = sqlx::query_as::<_, (i64, i64, f64)>(
        "SELECT COUNT(*), COALESCE(CAST(SUM(input_tokens + output_tokens) AS INTEGER), 0), COALESCE(CAST(SUM(estimated_cost_usd) AS DOUBLE), 0.0) FROM token_usage WHERE api_key_id = ?",
    )
    .bind(&key_id)
    .fetch_one(&state.db)
    .await?;

    let daily_usage = sqlx::query_as::<_, (String, i64, i64, f64)>(
        "
        SELECT CAST(DATE(created_at) AS TEXT) as date, COUNT(*),
               COALESCE(CAST(SUM(input_tokens + output_tokens) AS INTEGER), 0) as tokens_used,
               COALESCE(CAST(SUM(estimated_cost_usd) AS DOUBLE), 0.0) as cost_usd
        FROM token_usage
        WHERE api_key_id = ? AND created_at >= datetime(CURRENT_TIMESTAMP, '-30 days')
        GROUP BY CAST(DATE(created_at) AS TEXT)
        ORDER BY CAST(DATE(created_at) AS TEXT)
        ",
    )
    .bind(&key_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(ApiKeyStats {
        key_id,
        total_calls: row.0,
        total_tokens: row.1,
        total_cost_usd: row.2,
        daily_usage: daily_usage
            .into_iter()
            .map(|(date, calls, tokens, cost)| UsageRecord {
                date,
                calls,
                tokens_used: tokens,
                cost_usd: cost,
            })
            .collect(),
    }))
}

#[derive(Debug, Serialize)]
pub struct HealthTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// Test API key connectivity by making a lightweight API call.
pub async fn test_health(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key_id): Path<String>,
) -> Result<Json<HealthTestResult>> {
    require_apikey_permission(&state, &claims, "apikeys:read").await?;
    let start = std::time::Instant::now();

    let row: Option<(
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<i32>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT name, provider, base_url, encrypted_key, model_type, model, CAST(dimensions AS INTEGER), audio_generate_path, audio_query_path FROM api_keys WHERE id = ? AND tenant_id = ?",
    )
    .bind(&key_id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;

    let Some((
        _name,
        provider,
        base_url,
        encrypted_key,
        model_type,
        model,
        dimensions,
        audio_generate_path,
        audio_query_path,
    )) = row
    else {
        return Err(AppError::NotFound("API key not found".into()));
    };
    let resolved_model = model.unwrap_or_default();

    let api_key = match decrypt_api_key(&encrypted_key, &claims.tenant_id, &key_id) {
        Ok(k) => k,
        Err(e) => {
            return Ok(Json(HealthTestResult {
                ok: false,
                latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                error: Some(format!("decryption failed: {e}")),
            }));
        }
    };

    if model_type == "image" {
        let base = base_url.as_deref().unwrap_or("https://api.openai.com/v1");
        let mut test_ok = false;
        let mut error = Some("unknown image health check failure".to_string());
        for url in openai_endpoint_candidates(base, models_endpoint) {
            let (ok, err) = test_models_http_get(&url, &api_key, "image", &resolved_model).await;
            if ok {
                test_ok = true;
                error = None;
                break;
            }
            error = err;
        }
        return Ok(Json(HealthTestResult {
            ok: test_ok,
            latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            error,
        }));
    }

    if model_type == "audio" {
        let base = base_url.as_deref().unwrap_or("https://api.openai.com");
        if is_suno_audio_model(&resolved_model) {
            let (ok, err) = test_suno_audio_health(
                base,
                audio_generate_path.as_deref(),
                audio_query_path.as_deref(),
                &api_key,
            )
            .await;
            return Ok(Json(HealthTestResult {
                ok,
                latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                error: err,
            }));
        }
        let body = serde_json::json!({
            "model": resolved_model,
            "input": "health check",
            "voice": "alloy",
            "response_format": "mp3"
        });
        let mut test_ok = false;
        let mut error = Some("unknown audio health check failure".to_string());
        for endpoint in [
            audio_speech_endpoint as fn(&str) -> String,
            audio_generations_endpoint,
        ] {
            for url in openai_endpoint_candidates(base, endpoint) {
                let (ok, err) = test_http_post(&url, &body, &api_key, "audio", false, 15).await;
                if ok {
                    test_ok = true;
                    error = None;
                    break;
                }
                error = err;
            }
            if test_ok {
                break;
            }
        }
        return Ok(Json(HealthTestResult {
            ok: test_ok,
            latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            error,
        }));
    }

    // For embedding-type keys, build the URL using the same endpoint helper as real calls.
    // If base_url is empty, fall back to the OpenAI default.
    if model_type == "embedding" {
        #[cfg(feature = "nl2sql")]
        {
            let expected_dimensions = dimensions
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_else(|| {
                    crate::nl2sql::default_api_embedding_dimensions(&resolved_model)
                });
            let model = crate::nl2sql::embedding::EmbeddingModel::new_with_dimensions(
                &resolved_model,
                base_url.clone(),
                Some(api_key.clone()),
                Some(expected_dimensions),
            );
            let result = model.embed_batch(&["health check".to_string()]).await;
            return Ok(Json(HealthTestResult {
                ok: result.is_ok(),
                latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                error: result.err().map(|error| error.to_string()),
            }));
        }

        #[cfg(not(feature = "nl2sql"))]
        {
            let base = base_url.as_deref().unwrap_or("https://api.openai.com");
            let mut body = serde_json::json!({
                "model": resolved_model,
                "input": "health check"
            });
            if let Some(dimensions) = dimensions {
                body["dimensions"] = serde_json::json!(dimensions);
            }
            let mut test_ok = false;
            let mut error = Some("unknown embedding health check failure".to_string());
            for url in openai_endpoint_candidates(base, api::embeddings_endpoint) {
                let (ok, err) = test_http_post(&url, &body, &api_key, "embedding", true, 10).await;
                if ok {
                    test_ok = true;
                    error = None;
                    break;
                }
                error = err;
            }
            return Ok(Json(HealthTestResult {
                ok: test_ok,
                latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                error,
            }));
        }
    }

    let base = effective_provider_base_url(&provider, base_url.as_deref())?;
    let mut profile = api::infer_model_profile(&provider, Some(&base), &resolved_model);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| AppError::Internal(format!("HTTP client error: {error}")))?;
    let endpoints = if provider.eq_ignore_ascii_case("anthropic") {
        let base = strip_provider_endpoint_suffix(&base);
        if base.to_ascii_lowercase().ends_with("/v1") {
            vec![format!("{base}/messages")]
        } else {
            vec![format!("{base}/v1/messages")]
        }
    } else {
        chat_completion_endpoint_candidates(&base)
    };
    let mut test_ok = false;
    let mut error = None;
    for endpoint in endpoints {
        match send_probe_with_output_fallback(
            &client,
            &provider,
            &endpoint,
            &api_key,
            &resolved_model,
            &mut profile,
            false,
        )
        .await
        {
            Ok(_) => {
                test_ok = true;
                error = None;
                break;
            }
            Err(current_error) => error = Some(current_error),
        }
    }

    Ok(Json(HealthTestResult {
        ok: test_ok,
        latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        error,
    }))
}

fn openai_endpoint_candidates(base: &str, endpoint: fn(&str) -> String) -> Vec<String> {
    let trimmed = base.trim_end_matches('/');
    let mut candidates = vec![endpoint(trimmed)];
    let lower = trimmed.to_ascii_lowercase();
    let has_version_segment = lower.contains("/v1")
        || lower.contains("/v2")
        || lower.contains("/v3")
        || lower.contains("/v4")
        || lower.contains("/v5")
        || lower.contains("/compatible-mode/");
    if !has_version_segment {
        let with_v1 = format!("{trimmed}/v1");
        let v1_endpoint = endpoint(&with_v1);
        if !candidates.iter().any(|it| it == &v1_endpoint) {
            candidates.push(v1_endpoint);
        }
    }
    candidates
}

fn models_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/models")
    }
}

fn audio_speech_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/audio/speech") {
        trimmed.to_string()
    } else if trimmed.ends_with("/audio") {
        format!("{trimmed}/speech")
    } else {
        format!("{trimmed}/audio/speech")
    }
}

fn audio_generations_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/audio/generations") {
        trimmed.to_string()
    } else if trimmed.ends_with("/audio") {
        format!("{trimmed}/generations")
    } else {
        format!("{trimmed}/audio/generations")
    }
}

fn is_suno_audio_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized == "suno" || normalized.starts_with("suno/")
}

fn suno_endpoint_candidates(base: &str, paths: &[&str]) -> Vec<String> {
    let trimmed = base.trim_end_matches('/');
    let mut candidates = Vec::new();
    for path in paths {
        let normalized_path = path.trim();
        if normalized_path.is_empty() {
            continue;
        }
        if trimmed.ends_with(normalized_path.trim_start_matches('/')) {
            let endpoint = trimmed.to_string();
            if !candidates.iter().any(|it| it == &endpoint) {
                candidates.push(endpoint);
            }
            continue;
        }
        let endpoint = format!("{trimmed}/{}", normalized_path.trim_start_matches('/'));
        if !candidates.iter().any(|it| it == &endpoint) {
            candidates.push(endpoint);
        }
    }
    candidates
}

fn resolve_audio_endpoint(base: &str, path: &str) -> String {
    let trimmed_path = path.trim();
    if trimmed_path.starts_with("http://") || trimmed_path.starts_with("https://") {
        return trimmed_path.to_string();
    }
    let trimmed_base = base.trim_end_matches('/');
    let normalized_path = trimmed_path.trim_start_matches('/');
    format!("{trimmed_base}/{normalized_path}")
}

fn resolve_audio_query_endpoint_for_task(base: &str, path: &str, task_id: &str) -> String {
    let endpoint = resolve_audio_endpoint(base, path);
    endpoint
        .replace("{task_id}", task_id)
        .replace("{taskId}", task_id)
}

async fn test_suno_audio_health(
    base: &str,
    audio_generate_path: Option<&str>,
    audio_query_path: Option<&str>,
    api_key: &str,
) -> (bool, Option<String>) {
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, Some(format!("client error: {e}"))),
    };

    let snapshot_candidates = audio_query_path
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|path| {
            vec![resolve_audio_query_endpoint_for_task(
                base,
                path,
                "health-check-task-id",
            )]
        })
        .unwrap_or_else(|| {
            suno_endpoint_candidates(
                base,
                &[
                    "api/v1/generate/record-info",
                    "v1/generate/record-info",
                    "generate/record-info",
                ],
            )
        });
    let generate_candidates = audio_generate_path
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|path| vec![resolve_audio_endpoint(base, path)])
        .unwrap_or_else(|| {
            suno_endpoint_candidates(base, &["api/v1/generate", "v1/generate", "generate"])
        });

    let mut last_error = Some("unknown suno health check failure".to_string());
    for endpoint in snapshot_candidates {
        let response = match client
            .get(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .query(&[("taskId", "health-check-task-id")])
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                last_error = Some(format!("connection failed for {endpoint}: {e}"));
                continue;
            }
        };
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        if looks_like_html(&body_text) {
            last_error = Some(format!(
                "expected JSON from Suno endpoint, got HTML at {endpoint}: {}",
                body_excerpt(&body_text, 180)
            ));
            continue;
        }
        if status.as_u16() == 401 {
            return (
                false,
                Some("unauthorized — key may be invalid for Suno provider".to_string()),
            );
        }
        if status.is_success() || status.as_u16() == 400 || status.as_u16() == 422 {
            return (true, None);
        }
        if status.as_u16() != 404 {
            last_error = Some(format!(
                "unexpected status {status} for {endpoint}; body: {}",
                body_excerpt(&body_text, 180)
            ));
        }
    }

    for endpoint in generate_candidates {
        let response = match client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "prompt": "health check",
                "customMode": false,
                "instrumental": true,
                "model": "V4_5ALL"
            }))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                last_error = Some(format!("connection failed for {endpoint}: {e}"));
                continue;
            }
        };
        let status = response.status();
        let body_text = response.text().await.unwrap_or_default();
        if looks_like_html(&body_text) {
            last_error = Some(format!(
                "expected JSON from Suno endpoint, got HTML at {endpoint}: {}",
                body_excerpt(&body_text, 180)
            ));
            continue;
        }
        if status.as_u16() == 401 {
            return (
                false,
                Some("unauthorized — key may be invalid for Suno provider".to_string()),
            );
        }
        if status.is_success() || status.as_u16() == 400 || status.as_u16() == 422 {
            return (true, None);
        }
        last_error = Some(format!(
            "unexpected status {status} for {endpoint}; body: {}",
            body_excerpt(&body_text, 180)
        ));
    }

    (false, last_error)
}

fn body_excerpt(body: &str, max_chars: usize) -> String {
    body.chars().take(max_chars).collect()
}

fn looks_like_html(body: &str) -> bool {
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
}

async fn test_models_http_get(
    url: &str,
    api_key: &str,
    provider: &str,
    expected_model: &str,
) -> (bool, Option<String>) {
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, Some(format!("client error: {e}"))),
    };
    let resp = match client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (false, Some(format!("connection failed: {e}"))),
    };
    let status = resp.status();
    let body_text = match resp.text().await {
        Ok(text) => text,
        Err(e) => return (false, Some(format!("failed to read response body: {e}"))),
    };
    if status.as_u16() == 401 {
        return (
            false,
            Some(format!("unauthorized — key may be invalid for {provider}")),
        );
    }
    if !status.is_success() {
        return (
            false,
            Some(format!(
                "unexpected status {status} for {provider}; body: {}",
                body_excerpt(&body_text, 180)
            )),
        );
    }
    if looks_like_html(&body_text) {
        return (
            false,
            Some(format!(
                "expected JSON from {provider} endpoint, got HTML: {}",
                body_excerpt(&body_text, 180)
            )),
        );
    }
    let parsed = match serde_json::from_str::<Value>(&body_text) {
        Ok(value) => value,
        Err(e) => {
            return (
                false,
                Some(format!(
                    "expected JSON from {provider} endpoint but parse failed: {e}; body: {}",
                    body_excerpt(&body_text, 180)
                )),
            )
        }
    };
    if let Some(error_obj) = parsed.get("error") {
        return (
            false,
            Some(format!(
                "provider returned error object: {}",
                body_excerpt(&error_obj.to_string(), 180)
            )),
        );
    }
    let Some(data) = parsed.get("data").and_then(Value::as_array) else {
        return (true, None);
    };
    let expected = expected_model.trim().to_ascii_lowercase();
    if expected.is_empty() {
        return (true, None);
    }
    let found = data.iter().any(|item| {
        item.get("id")
            .and_then(Value::as_str)
            .map(|id| id.eq_ignore_ascii_case(&expected))
            .unwrap_or(false)
    });
    if found {
        (true, None)
    } else {
        (
            false,
            Some(format!(
                "model '{expected_model}' not found in /models list"
            )),
        )
    }
}

/// POST request for health check — used for embedding endpoints that don't
/// support GET /v1/models.
async fn test_http_post(
    url: &str,
    body: &serde_json::Value,
    api_key: &str,
    provider: &str,
    expect_json: bool,
    timeout_secs: u64,
) -> (bool, Option<String>) {
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, Some(format!("client error: {e}"))),
    };

    let resp = match client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (false, Some(format!("connection failed: {e}"))),
    };

    let status = resp.status();
    let body_text = match resp.text().await {
        Ok(text) => text,
        Err(e) => return (false, Some(format!("failed to read response body: {e}"))),
    };
    if status.is_success() {
        if !expect_json {
            return (true, None);
        }
        if looks_like_html(&body_text) {
            return (
                false,
                Some(format!(
                    "expected JSON from {provider} endpoint, got HTML: {}",
                    body_excerpt(&body_text, 180)
                )),
            );
        }
        let parsed = match serde_json::from_str::<Value>(&body_text) {
            Ok(value) => value,
            Err(e) => {
                return (
                    false,
                    Some(format!(
                        "expected JSON from {provider} endpoint but parse failed: {e}; body: {}",
                        body_excerpt(&body_text, 180)
                    )),
                )
            }
        };
        if let Some(error_obj) = parsed.get("error") {
            return (
                false,
                Some(format!(
                    "provider returned error object: {}",
                    body_excerpt(&error_obj.to_string(), 180)
                )),
            );
        }
        (true, None)
    } else if status.as_u16() == 401 {
        // 401 means the endpoint is reachable but the key is invalid/unauthorized
        (
            false,
            Some(format!("unauthorized — key may be invalid for {provider}")),
        )
    } else {
        (
            false,
            Some(format!(
                "unexpected status {status} for {provider}; body: {}",
                body_excerpt(&body_text, 180)
            )),
        )
    }
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/", routing_post(create))
        .route("/models/resolve", routing_post(resolve_model))
        .route("/models/discover", routing_post(discover_models))
        .route("/models/accept", routing_post(accept_model_profile))
        .route("/models/probe", routing_post(probe_model))
        .route("/{id}", routing_delete(delete))
        .route("/{id}", routing_put(update))
        .route("/{key_id}/stats", routing_get(stats))
        .route("/{key_id}/test", routing_post(test_health))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_candidates_cover_versioned_and_unversioned_openai_compatible_bases() {
        let base = "https://llm-proxy.example.com";
        let candidates = chat_completion_endpoint_candidates(base);
        assert!(candidates
            .iter()
            .any(|url| url == "https://llm-proxy.example.com/v1/chat/completions"));
        assert!(candidates
            .iter()
            .any(|url| url == "https://llm-proxy.example.com/chat/completions"));
        assert_eq!(
            chat_completion_endpoint_candidates("https://llm-proxy.example.com/v1"),
            ["https://llm-proxy.example.com/v1/chat/completions"]
        );
        assert!(
            chat_completion_endpoint_candidates("https://open.bigmodel.cn/api/paas/v4")
                .contains(&"https://open.bigmodel.cn/api/paas/v4/chat/completions".to_string())
        );
        assert!(chat_completion_endpoint_candidates(
            "https://generativelanguage.googleapis.com/v1beta/openai"
        )
        .contains(
            &"https://generativelanguage.googleapis.com/v1beta/openai/chat/completions".to_string()
        ));
    }

    #[test]
    fn provider_presets_resolve_official_openai_compatible_endpoints() {
        assert_eq!(
            effective_provider_base_url("kimi", None).expect("Kimi endpoint"),
            "https://api.moonshot.cn"
        );
        assert_eq!(
            effective_provider_base_url("glm", None).expect("GLM endpoint"),
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(
            effective_provider_base_url("gemini", None).expect("Gemini endpoint"),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert_eq!(
            effective_provider_base_url("xai", None).expect("xAI endpoint"),
            "https://api.x.ai"
        );
        assert_eq!(
            canonical_profile_provider("custom", Some("https://api.moonshot.cn/v1")),
            "kimi"
        );
        assert_eq!(
            canonical_profile_provider(
                "custom",
                Some("https://generativelanguage.googleapis.com/v1beta/openai")
            ),
            "gemini"
        );
        assert_eq!(
            canonical_profile_provider("custom", Some("https://api.x.ai/v1")),
            "xai"
        );
    }

    #[test]
    fn runtime_profile_round_trips_through_accept_validation() {
        let inferred = api::infer_model_profile(
            "custom",
            Some("https://api.deepseek.com/v1"),
            "deepseek-v4-pro",
        );
        let accepted = validate_accepted_model_profile(
            "custom",
            Some("https://api.deepseek.com/v1"),
            "deepseek-v4-pro",
            inferred.runtime_capabilities(),
        )
        .expect("runtime capability JSON must be accepted");
        assert_eq!(accepted.protocol, api::ModelProtocol::OpenAiChatCompletions);
        assert_eq!(
            accepted
                .reasoning
                .expect("reasoning profile")
                .supported_values,
            ["low", "high", "max"]
        );
    }

    #[test]
    fn explicit_capability_overrides_win_without_erasing_detected_values() {
        let merged = merge_capability_values(
            Some(json!({
                "contextWindowTokens": 128000,
                "reasoningEffortDefault": "high"
            })),
            Some(json!({"reasoningEffortDefault": "low"})),
        )
        .expect("merged capabilities");
        assert_eq!(merged["contextWindowTokens"], json!(128000));
        assert_eq!(merged["reasoningEffortDefault"], json!("low"));
    }

    #[test]
    fn api_key_roles_and_chat_profile_scope_are_conservative() {
        assert!(role_has_apikey_permission("developer", "apikeys:read"));
        assert!(!role_has_apikey_permission("developer", "apikeys:write"));
        assert!(role_has_apikey_permission("admin", "apikeys:delete"));
        assert!(require_chat_model_type(Some("chat")).is_ok());
        assert!(require_chat_model_type(Some("embedding")).is_err());
    }

    #[test]
    fn verified_profiles_win_and_registry_updates_refresh_inference() {
        assert!(should_preserve_existing_profile(
            "probe",
            "verified",
            "old",
            "provider_metadata",
            "metadata"
        ));
        assert!(should_preserve_existing_profile(
            "provider_metadata",
            "metadata",
            "old",
            "built_in_registry",
            "inferred"
        ));
        assert!(should_preserve_existing_profile(
            "built_in_registry",
            "inferred",
            api::MODEL_CAPABILITY_REGISTRY_VERSION,
            "built_in_registry",
            "inferred"
        ));
        assert!(!should_preserve_existing_profile(
            "built_in_registry",
            "inferred",
            "old",
            "built_in_registry",
            "inferred"
        ));
    }

    #[test]
    fn only_live_model_profiles_expire() {
        assert!(model_profile_expiration("probe").is_some());
        assert!(model_profile_expiration("provider_metadata").is_some());
        assert!(model_profile_expiration("built_in_registry").is_none());
        assert!(model_profile_expiration("conservative_fallback").is_none());
    }

    #[test]
    fn accepted_profiles_reject_unbounded_reasoning_values() {
        let inferred = api::infer_model_profile(
            "custom",
            Some("https://api.deepseek.com/v1"),
            "deepseek-v4-pro",
        );
        let mut value = inferred.runtime_capabilities();
        value["reasoning"]["supportedValues"] = json!(["low", "arbitrary-provider-value"]);
        assert!(validate_accepted_model_profile(
            "custom",
            Some("https://api.deepseek.com/v1"),
            "deepseek-v4-pro",
            value
        )
        .is_err());
    }

    #[test]
    fn model_type_changes_clear_fields_owned_by_the_previous_type() {
        assert_eq!(
            api_key_update_clears(Some("embedding"), "embedding", true),
            ApiKeyUpdateClears {
                dimensions: false,
                audio_paths: true,
                chat_capabilities: true,
                model_profile: true,
            }
        );
        assert_eq!(
            api_key_update_clears(Some("audio"), "audio", true),
            ApiKeyUpdateClears {
                dimensions: true,
                audio_paths: false,
                chat_capabilities: true,
                model_profile: true,
            }
        );
        assert_eq!(
            api_key_update_clears(Some("chat"), "chat", true),
            ApiKeyUpdateClears {
                dimensions: true,
                audio_paths: true,
                chat_capabilities: false,
                model_profile: false,
            }
        );
    }

    #[test]
    fn key_rotation_keeps_model_specific_fields_and_profile() {
        assert_eq!(
            api_key_update_clears(None, "chat", false),
            ApiKeyUpdateClears {
                dimensions: false,
                audio_paths: false,
                chat_capabilities: false,
                model_profile: false,
            }
        );
    }

    #[test]
    fn deepseek_thinking_probe_rejections_are_capability_results_not_key_failures() {
        let tool_error = r#"provider returned 400 Bad Request: {"error":{"message":"Thinking mode does not support this tool_choice","code":"invalid_request_error"}}"#;
        let structured_error = r#"provider returned 400 Bad Request: {"error":{"message":"This response_format type is unavailable now","code":"invalid_request_error"}}"#;

        assert!(probe_error_is_capability_unavailable(
            tool_error,
            "tool_choice"
        ));
        assert!(probe_error_is_capability_unavailable(
            structured_error,
            "response_format"
        ));
        assert!(!probe_error_is_capability_unavailable(
            "provider returned 401 unauthorized",
            "tool_choice"
        ));
    }
}
