//! NL2SQL semantic routing — enables AI-driven data source and table selection.
//!
//! Architecture:
//! - **EmbeddingStore**: SQLite-backed vector storage with in-memory LRU cache.
//!   Vector similarity uses cosine similarity implemented in pure Rust (no external deps).
//! - **RoutingEngine**: Given a user question, generates an embedding and finds the
//!   best-matching data source(s) using cosine similarity over column-level descriptions.
//! - **SchemaDescriber**: Uses the LLM to auto-generate semantic descriptions for tables/columns.
//!
//! Storage strategy: physically isolated SQLite files under
//! `$data_dir/nl2sql/embedding-profiles/<tenant-hash>/<profile-id>/embeddings.db`.
//! The stores are local to each AOS instance and are not replicated across nodes.

pub mod domain_discoverer;
pub mod embedding;
pub mod embedding_failover;
pub mod embedding_profiles;
pub mod embedding_reindex_worker;
pub mod query_understanding;
pub mod rate_limiter;
pub mod routing;
pub mod schema_describer;
pub mod schema_monitor;

pub use nl2sql_core::{
    coreference, cross_ds_discovery, datasource_pool, join_path, refresh_lock, requirements,
    result_cache, result_validator, schema_diff, schema_discovery,
};
pub use nl2sql_domain::merge_strategy::{MergeInput, SqlExecutionAttempt, StepResult};
pub use requirements::MissingRequirementReason;

use api;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Number of candidate tables passed to the LLM for tool-calling routing.
/// Tunable via `NL2SQL_TOP_K_TABLES_FOR_LLM` (default: 20).
pub fn top_k_tables_for_llm() -> usize {
    std::env::var("NL2SQL_TOP_K_TABLES_FOR_LLM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

/// Minimum confidence threshold for embedding-only routing (legacy path).
/// Below this threshold the route handler returns None so the LLM can suggest.
pub const MIN_CONFIDENCE: f32 = 0.30;

/// Built-in multilingual model used when a tenant has not configured a remote
/// embedding API. The quantized ONNX model keeps the open-source package small
/// enough for local deployment while retaining Chinese and English retrieval.
pub const LOCAL_EMBEDDING_MODEL: &str = runtime::local_embedding::MODEL;
pub const LOCAL_EMBEDDING_DIMENSIONS: usize = runtime::local_embedding::DIMENSIONS;
pub const LOCAL_EMBEDDING_MODEL_VERSION: &str = runtime::local_embedding::MODEL_VERSION;
pub const LOCAL_EMBEDDING_VECTOR_SIGNATURE: &str = runtime::local_embedding::VECTOR_SIGNATURE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProfileKind {
    Api,
    Local,
}

impl EmbeddingProfileKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Local => "local",
        }
    }
}

/// Minimum cosine similarity threshold for a table to be considered a valid match.
/// Tables below this threshold are filtered out during global table search and RRF fusion,
/// preventing low-quality matches from polluting results. Tunable via `NL2SQL_MIN_TABLE_SIM`.
pub fn min_table_sim_threshold() -> f32 {
    std::env::var("NL2SQL_MIN_TABLE_SIM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.20)
}

/// Per-tenant embedding model configuration resolved from the `api_keys` table.
#[derive(Debug, Clone)]
pub struct EmbeddingTenantConfig {
    /// The API key value (decrypted) used to authenticate with the embedding endpoint.
    pub api_key: String,
    /// Embedding model name, e.g. "text-embedding-3-small".
    pub model: String,
    /// Custom base URL for the embedding API (None = use OpenAI default).
    pub base_url: Option<String>,
    /// Embedding vector dimensions (None = use provider default, e.g. 1536 for text-embedding-3-small).
    pub dimensions: Option<usize>,
    /// Where the config was resolved from.
    pub configured_via: &'static str,
    /// ID of the API key in DB that was used (None for env-fallback).
    pub key_id: Option<String>,
    /// Stable provider identifier. It is part of the vector-space profile.
    pub provider: String,
    /// API and local vectors always live in separate immutable profiles.
    pub profile_kind: EmbeddingProfileKind,
    /// Provider/model revision used to distinguish model upgrades.
    pub model_version: String,
    /// Stable signature for the vector-space contract or bundled model artifact.
    pub vector_signature: String,
}

impl EmbeddingTenantConfig {
    #[must_use]
    pub fn effective_dimensions(&self) -> usize {
        self.dimensions.unwrap_or_else(|| {
            if self.model == LOCAL_EMBEDDING_MODEL {
                LOCAL_EMBEDDING_DIMENSIONS
            } else {
                default_api_embedding_dimensions(&self.model)
            }
        })
    }

    #[must_use]
    pub fn normalized_base_url(&self) -> String {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.openai.com")
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase()
    }

    /// Profile IDs deliberately exclude the API key ID/value. Rotating a key
    /// for the same provider/model therefore does not trigger a reindex.
    #[must_use]
    pub fn profile_id(&self, tenant_id: &str) -> String {
        let contract = format!(
            "v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            tenant_id,
            self.profile_kind.as_str(),
            self.provider.trim().to_ascii_lowercase(),
            self.normalized_base_url(),
            self.model.trim(),
            self.effective_dimensions(),
            self.model_version,
            self.vector_signature,
        );
        format!("ep_{}", hex::encode(Sha256::digest(contract.as_bytes())))
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddingProfiles {
    pub api: Option<EmbeddingTenantConfig>,
    pub local: EmbeddingTenantConfig,
}

/// Per-tenant chat LLM configuration resolved from the `api_keys` table.
/// Used by NL2SQL schema description (LLM calls to generate column descriptions).
#[derive(Debug, Clone)]
pub struct ChatTenantConfig {
    /// The resolved API client, ready to use.
    pub client: crate::governed_provider::GovernedProviderClient,
    /// The model name used (from DB key's model field or default_model).
    pub model: String,
    /// ID of the API key in DB that was used (None for env-fallback).
    pub key_id: Option<String>,
    /// Provider name for usage logging.
    pub provider: String,
    /// Effective provider/model output ceiling, including an API-key override.
    pub max_output_tokens: u32,
}

fn capability_u32(value: Option<&serde_json::Value>, keys: &[&str]) -> Option<u32> {
    let value = value?;
    keys.iter().find_map(|key| {
        let raw = value.get(*key)?.as_u64()?;
        u32::try_from(raw).ok().filter(|value| *value > 0)
    })
}

fn model_capability_override(
    value: Option<&serde_json::Value>,
) -> Option<api::ModelCapabilityOverride> {
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

fn effective_model_max_output_tokens(model: &str, capabilities: Option<&serde_json::Value>) -> u32 {
    let resolved = api::model_capabilities(model, model_capability_override(capabilities));
    if resolved.source == api::ModelCapabilitiesSource::ConservativeFallback {
        64_000
    } else {
        resolved.max_output_tokens
    }
}

fn authoritative_model_token_limits(
    model: &str,
    capabilities: Option<&serde_json::Value>,
) -> (Option<u32>, Option<u32>) {
    let manual = model_capability_override(capabilities).unwrap_or_default();
    let built_in = api::model_token_limit(model);
    (
        manual
            .context_window_tokens
            .or_else(|| built_in.map(|limit| limit.context_window_tokens)),
        manual
            .max_output_tokens
            .or_else(|| built_in.map(|limit| limit.max_output_tokens)),
    )
}

#[cfg(test)]
mod model_capability_tests {
    use super::effective_model_max_output_tokens;

    #[test]
    fn chat_output_ceiling_uses_key_override_then_model_registry() {
        let camel = serde_json::json!({"maxOutputTokens": 12_345});
        let snake = serde_json::json!({"max_output_tokens": 23_456});

        assert_eq!(
            effective_model_max_output_tokens("deepseek-v4-pro", Some(&camel)),
            12_345
        );
        assert_eq!(
            effective_model_max_output_tokens("custom/model", Some(&snake)),
            23_456
        );
        assert_eq!(
            effective_model_max_output_tokens("deepseek-v4-pro", None),
            384_000
        );
        assert_eq!(
            effective_model_max_output_tokens("future-custom-model", None),
            64_000
        );
    }
}

#[cfg(test)]
mod local_embedding_config_tests {
    use super::{
        default_api_embedding_dimensions, local_embedding_config, EmbeddingProfileKind,
        EmbeddingTenantConfig, LOCAL_EMBEDDING_DIMENSIONS, LOCAL_EMBEDDING_MODEL,
    };

    #[test]
    fn bundled_embedding_is_the_keyless_nl2sql_default() {
        let config = local_embedding_config();

        assert_eq!(config.configured_via, "local");
        assert_eq!(config.model, LOCAL_EMBEDDING_MODEL);
        assert_eq!(config.dimensions, Some(LOCAL_EMBEDDING_DIMENSIONS));
        assert!(config.api_key.is_empty());
        assert!(config.base_url.is_none());
        assert!(config.key_id.is_none());
    }

    fn api_config(api_key: &str, model: &str, dimensions: usize) -> EmbeddingTenantConfig {
        EmbeddingTenantConfig {
            api_key: api_key.to_string(),
            model: model.to_string(),
            base_url: Some("https://embedding.example/v1".to_string()),
            dimensions: Some(dimensions),
            configured_via: "api_key",
            key_id: Some(format!("key-{api_key}")),
            provider: "custom".to_string(),
            profile_kind: EmbeddingProfileKind::Api,
            model_version: "v1".to_string(),
            vector_signature: format!("signature-{model}-{dimensions}"),
        }
    }

    #[test]
    fn api_key_rotation_preserves_profile_identity() {
        let first = api_config("first-secret", "embedding-model", 1024);
        let rotated = api_config("rotated-secret", "embedding-model", 1024);
        assert_eq!(first.profile_id("tenant-a"), rotated.profile_id("tenant-a"));
    }

    #[test]
    fn api_dimension_defaults_match_the_profile_contract() {
        assert_eq!(
            default_api_embedding_dimensions("text-embedding-3-small"),
            1536
        );
        assert_eq!(
            default_api_embedding_dimensions("text-embedding-3-large"),
            3072
        );
        assert_eq!(
            default_api_embedding_dimensions("text-embedding-5-large"),
            4096
        );
        assert_eq!(default_api_embedding_dimensions("custom-model"), 1536);
    }

    #[test]
    fn vector_contract_changes_create_new_profiles() {
        let baseline = api_config("secret", "embedding-model", 1024);
        let changed_model = api_config("secret", "embedding-model-v2", 1024);
        let changed_dimensions = api_config("secret", "embedding-model", 768);
        let mut changed_endpoint = baseline.clone();
        changed_endpoint.base_url = Some("https://other.example/v1".to_string());

        assert_ne!(
            baseline.profile_id("tenant-a"),
            changed_model.profile_id("tenant-a")
        );
        assert_ne!(
            baseline.profile_id("tenant-a"),
            changed_dimensions.profile_id("tenant-a")
        );
        assert_ne!(
            baseline.profile_id("tenant-a"),
            changed_endpoint.profile_id("tenant-a")
        );
    }
}

/// Ordered chat LLM candidates for runtime failover.
/// The first entry is highest priority.
pub type ChatTenantCandidates = Vec<ChatTenantConfig>;

fn canonical_model_name(model: &str) -> &str {
    model
        .trim()
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or_else(|| model.trim())
}

/// Keep tenant/user authorization order intact while moving the model selected
/// for the parent turn ahead of fallback candidates.
pub fn prioritize_chat_candidates(
    candidates: &mut ChatTenantCandidates,
    preferred_model: Option<&str>,
) {
    let Some(preferred) = preferred_model
        .map(canonical_model_name)
        .filter(|model| !model.is_empty())
    else {
        return;
    };
    candidates.sort_by_key(|candidate| {
        !canonical_model_name(&candidate.model).eq_ignore_ascii_case(preferred)
    });
}

/// Resolves the per-tenant embedding model configuration.
///
/// Remote embedding configuration is resolved separately and participates in
/// dual-profile retrieval only after an operator explicitly configures an
/// embedding API key. Single-profile callers always receive the bundled local
/// model so remote configuration cannot silently replace the offline baseline.
async fn resolve_remote_embedding_config(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: Option<&str>,
) -> Option<EmbeddingTenantConfig> {
    // Execute the scenario-scoped query.
    let rows_result = match scenario {
        Some(s) => sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<i32>)>(
            "SELECT id, encrypted_key, provider, model, base_url, CAST(dimensions AS INTEGER) FROM api_keys
                 WHERE tenant_id = ? AND model_type = 'embedding' AND enabled = 1
                   AND (scenarios IS NULL OR EXISTS (SELECT 1 FROM json_each(scenarios) WHERE json_each.value = ?))
                 ORDER BY priority ASC, created_at ASC",
        )
        .bind(tenant_id)
        .bind(s)
        .fetch_all(db)
        .await,
        None => sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<i32>)>(
            "SELECT id, encrypted_key, provider, model, base_url, CAST(dimensions AS INTEGER) FROM api_keys
                 WHERE tenant_id = ? AND model_type = 'embedding' AND enabled = 1
                 ORDER BY priority ASC, created_at ASC",
        )
        .bind(tenant_id)
        .fetch_all(db)
        .await,
    };

    let rows = match rows_result {
        Ok(rows) => {
            tracing::debug!(
                tenant_id = %tenant_id,
                "resolve_embedding_config: found {} embedding key(s) in DB",
                rows.len(),
            );
            rows
        }
        Err(e) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                error = %e,
                "failed to query tenant embedding api_keys",
            );
            return None;
        }
    };

    // Try each key in priority order; skip on decryption failure.
    for (key_id, encrypted_key, provider, model, base_url, dimensions) in &rows {
        if encrypted_key.is_empty() {
            tracing::debug!(
                tenant_id = %tenant_id,
                key_id = %key_id,
                "resolve_embedding_config: empty encrypted_key, skipping",
            );
            continue;
        }

        match crate::routes::apikeys::decrypt_api_key(encrypted_key, tenant_id, key_id) {
            Ok(api_key) => {
                let resolved = EmbeddingTenantConfig {
                    api_key,
                    model: if model.is_empty() {
                        "text-embedding-3-small".to_owned()
                    } else {
                        model.clone()
                    },
                    base_url: base_url.clone(),
                    dimensions: dimensions.map(|d| d as usize),
                    configured_via: "api_key",
                    key_id: Some(key_id.clone()),
                    provider: provider.clone(),
                    profile_kind: EmbeddingProfileKind::Api,
                    model_version: "provider-managed-v1".to_string(),
                    vector_signature: api_vector_signature(
                        provider,
                        base_url.as_deref(),
                        model,
                        dimensions.map(|value| value as usize),
                    ),
                };
                tracing::info!(
                    tenant_id = %tenant_id,
                    key_id = %key_id,
                    model = %resolved.model,
                    "resolve_embedding_config: using DB embedding key '{}'",
                    key_id,
                );
                return Some(resolved);
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    key_id = %key_id,
                    error = %e,
                    "resolve_embedding_config: failed to decrypt key '{}', trying next",
                    key_id,
                );
            }
        }
    }

    tracing::debug!(
        tenant_id = %tenant_id,
        env_fallback_enabled = tenant_embedding_env_fallback_enabled(),
        "resolve_embedding_config: no usable tenant embedding keys",
    );
    tenant_embedding_env_fallback_enabled()
        .then(env_fallback_embedding_config)
        .flatten()
}

pub async fn resolve_embedding_config(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: Option<&str>,
) -> Option<EmbeddingTenantConfig> {
    let _ = (db, tenant_id, scenario);
    Some(local_embedding_config())
}

pub async fn resolve_embedding_profiles(
    db: &SqlitePool,
    tenant_id: &str,
    scenario: Option<&str>,
) -> EmbeddingProfiles {
    EmbeddingProfiles {
        api: resolve_remote_embedding_config(db, tenant_id, scenario).await,
        local: local_embedding_config(),
    }
}

fn api_vector_signature(
    provider: &str,
    base_url: Option<&str>,
    model: &str,
    dimensions: Option<usize>,
) -> String {
    let canonical = format!(
        "api-vector-contract-v1\0{}\0{}\0{}\0{}",
        provider.trim().to_ascii_lowercase(),
        base_url
            .unwrap_or("https://api.openai.com")
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase(),
        model.trim(),
        dimensions.unwrap_or_else(|| default_api_embedding_dimensions(model)),
    );
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    )
}

pub(crate) fn default_api_embedding_dimensions(model: &str) -> usize {
    match model {
        "text-embedding-3-large" => 3072,
        "text-embedding-5-large" => 4096,
        _ => 1536,
    }
}

pub(crate) fn local_embedding_config_for_runtime() -> EmbeddingTenantConfig {
    EmbeddingTenantConfig {
        api_key: String::new(),
        model: LOCAL_EMBEDDING_MODEL.to_owned(),
        base_url: None,
        dimensions: Some(LOCAL_EMBEDDING_DIMENSIONS),
        configured_via: "local",
        key_id: None,
        provider: "aos-local-onnx".to_string(),
        profile_kind: EmbeddingProfileKind::Local,
        model_version: LOCAL_EMBEDDING_MODEL_VERSION.to_string(),
        vector_signature: LOCAL_EMBEDDING_VECTOR_SIGNATURE.to_string(),
    }
}

fn local_embedding_config() -> EmbeddingTenantConfig {
    local_embedding_config_for_runtime()
}

fn tenant_embedding_env_fallback_enabled() -> bool {
    runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_EMBEDDING_ENV_FALLBACK")
}

fn env_fallback_embedding_config() -> Option<EmbeddingTenantConfig> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .ok()?;
    let model = std::env::var("EMBEDDING_MODEL")
        .ok()
        .unwrap_or_else(|| "text-embedding-3-small".to_owned());
    let base_url = std::env::var("EMBEDDING_BASE_URL").ok();
    let provider = std::env::var("EMBEDDING_PROVIDER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "openai-compatible".to_string());
    let dimensions = None;
    Some(EmbeddingTenantConfig {
        api_key,
        vector_signature: api_vector_signature(&provider, base_url.as_deref(), &model, dimensions),
        model,
        base_url,
        dimensions,
        configured_via: "env",
        key_id: None,
        provider,
        profile_kind: EmbeddingProfileKind::Api,
        model_version: "provider-managed-v1".to_string(),
    })
}

/// Resolves a per-tenant chat LLM configuration for use in NL2SQL schema descriptions.
///
/// Resolution order:
/// 1. Query `api_keys` for rows where `model_type = 'chat'` and `enabled = 1`
///    and `scenarios` matches (NULL = all scenarios, or JSON_CONTAINS).
/// 2. If all DB keys fail, fall back to `ProviderClient::from_model`.
/// 3. If that also fails, return an error.
pub async fn resolve_chat_config(
    config_registry: &agent_gateway::TenantConfigRegistry,
    tenant_id: &str,
    user_id: &str,
    default_model: &str,
    scenario: Option<&str>,
) -> Result<ChatTenantConfig, String> {
    let candidates = resolve_chat_config_candidates(
        config_registry,
        tenant_id,
        user_id,
        default_model,
        scenario,
    )
    .await?;
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| "no chat config candidates available".to_string())
}

/// Resolves a per-tenant chat LLM configuration from DB keys only (no env fallback).
///
/// Used by background tasks (e.g. scheduler) that must strictly follow
/// tenant/user API key configuration in `api_keys`.
pub async fn resolve_chat_config_db_only(
    config_registry: &agent_gateway::TenantConfigRegistry,
    tenant_id: &str,
    user_id: &str,
    default_model: &str,
    scenario: Option<&str>,
) -> Result<ChatTenantConfig, String> {
    let candidates = resolve_chat_config_candidates_db_only(
        config_registry,
        tenant_id,
        user_id,
        default_model,
        scenario,
    )
    .await?;
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| "no usable chat API key in database".to_string())
}

/// Resolves ordered chat candidates from DB keys only (no env fallback).
pub async fn resolve_chat_config_candidates_db_only(
    config_registry: &agent_gateway::TenantConfigRegistry,
    tenant_id: &str,
    user_id: &str,
    default_model: &str,
    scenario: Option<&str>,
) -> Result<ChatTenantCandidates, String> {
    let runtime_config = config_registry
        .load_user_config(tenant_id, user_id, scenario)
        .await
        .map_err(|e| format!("failed to load config for tenant {}: {}", tenant_id, e))?;

    let mut candidates: ChatTenantCandidates = Vec::new();
    for entry in &runtime_config.api_keys {
        let model_fallback = default_model.to_string();
        let effective_model = entry.model.as_ref().unwrap_or(&model_fallback);
        let result = api::build_provider(
            &entry.provider,
            effective_model,
            &entry.key,
            entry.base_url.as_deref(),
        );
        match result {
            Ok(client) => {
                let (context_limit, output_limit) = authoritative_model_token_limits(
                    effective_model,
                    entry.capabilities_json.as_ref(),
                );
                let client = crate::governed_provider::GovernedProviderClient::new(
                    client.with_token_limits(context_limit, output_limit),
                    config_registry.database(),
                    tenant_id,
                    user_id,
                    format!("nl2sql:{}", scenario.unwrap_or("default")),
                );
                candidates.push(ChatTenantConfig {
                    client,
                    model: effective_model.clone(),
                    key_id: Some(entry.id.clone()),
                    provider: entry.provider.clone(),
                    max_output_tokens: effective_model_max_output_tokens(
                        effective_model,
                        entry.capabilities_json.as_ref(),
                    ),
                });
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    key_id = %entry.id,
                    error = %e,
                    "resolve_chat_config_db_only: DB key '{}' failed, trying next",
                    entry.id
                );
            }
        }
    }

    Ok(candidates)
}

/// Resolves ordered chat LLM candidates for NL2SQL runtime failover.
///
/// Resolution order:
/// 1. Query `api_keys` for rows where `model_type = 'chat'` and `enabled = 1`
///    and `scenarios` matches (NULL = all scenarios, or JSON_CONTAINS).
/// 2. Build provider clients for all usable DB keys (priority order).
/// 3. If none are usable, fail closed. Development-only environment fallback
///    requires `AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK=1`.
pub async fn resolve_chat_config_candidates(
    config_registry: &agent_gateway::TenantConfigRegistry,
    tenant_id: &str,
    user_id: &str,
    default_model: &str,
    scenario: Option<&str>,
) -> Result<ChatTenantCandidates, String> {
    let runtime_config = config_registry
        .load_user_config(tenant_id, user_id, scenario)
        .await
        .map_err(|e| format!("failed to load config for tenant {}: {}", tenant_id, e))?;

    let mut candidates: ChatTenantCandidates = Vec::new();

    if runtime_config.api_keys.is_empty() {
        tracing::debug!(
            tenant_id = %tenant_id,
            "resolve_chat_config: no chat keys in DB, using env fallback"
        );
    } else {
        for entry in &runtime_config.api_keys {
            let model_fallback = default_model.to_string();
            let effective_model = entry.model.as_ref().unwrap_or(&model_fallback);
            // Use build_provider so the decrypted DB key is injected directly, bypassing
            // env-var heuristics. This correctly handles custom/third-party providers
            // (e.g. OpenRouter, NovitaAI) that store their key in the database.
            let result = api::build_provider(
                &entry.provider,
                effective_model,
                &entry.key,
                entry.base_url.as_deref(),
            );
            match result {
                Ok(client) => {
                    let (context_limit, output_limit) = authoritative_model_token_limits(
                        effective_model,
                        entry.capabilities_json.as_ref(),
                    );
                    let client = crate::governed_provider::GovernedProviderClient::new(
                        client.with_token_limits(context_limit, output_limit),
                        config_registry.database(),
                        tenant_id,
                        user_id,
                        format!("nl2sql:{}", scenario.unwrap_or("default")),
                    );
                    tracing::info!(
                        tenant_id = %tenant_id,
                        key_id = %entry.id,
                        model = %effective_model,
                        provider = %entry.provider,
                        "resolve_chat_config: accepted DB chat key '{}'",
                        entry.id
                    );
                    candidates.push(ChatTenantConfig {
                        client,
                        model: effective_model.clone(),
                        key_id: Some(entry.id.clone()),
                        provider: entry.provider.clone(),
                        max_output_tokens: effective_model_max_output_tokens(
                            effective_model,
                            entry.capabilities_json.as_ref(),
                        ),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        key_id = %entry.id,
                        error = %e,
                        "resolve_chat_config: DB key '{}' failed, trying next",
                        entry.id
                    );
                }
            }
        }
    }

    if candidates.is_empty() {
        if !runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK") {
            return Err(
                "no approved tenant chat model is configured for NL2SQL; configure an enabled chat model key"
                    .to_string(),
            );
        }
        // Explicit development compatibility fallback.
        let client = api::ProviderClient::from_model(default_model)
            .map_err(|e| format!("failed to create LLM client from env: {}", e))?;
        let (context_limit, output_limit) = authoritative_model_token_limits(default_model, None);
        let client = crate::governed_provider::GovernedProviderClient::new(
            client.with_token_limits(context_limit, output_limit),
            config_registry.database(),
            tenant_id,
            user_id,
            format!("nl2sql:{}", scenario.unwrap_or("default")),
        );
        tracing::info!(
            tenant_id = %tenant_id,
            model = %default_model,
            "resolve_chat_config: using env fallback"
        );
        candidates.push(ChatTenantConfig {
            client,
            model: default_model.to_owned(),
            key_id: None,
            provider: "env-fallback".to_string(),
            max_output_tokens: effective_model_max_output_tokens(default_model, None),
        });
    }

    Ok(candidates)
}

/// A foreign key for injection into NL2SQL prompts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ForeignKeyPrompt {
    pub source_table: String,
    pub source_column: String,
    pub source_type: String,
    pub target_table: String,
    pub target_column: String,
    pub target_type: String,
}

/// Load user-defined FKs for a specific datasource from the nl2sql_foreign_keys table.
/// These take precedence over auto-detected FKs.
pub async fn load_user_defined_fks_for_datasource(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Vec<ForeignKeyPrompt> {
    let rows: Vec<(String, String, String, String, String, String)> =
        sqlx::query_as::<_, (String, String, String, String, String, String)>(
            "SELECT source_table, source_column, source_type, target_table, target_column, target_type \
             FROM nl2sql_foreign_keys \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

    rows.into_iter()
        .map(
            |(
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
            )| {
                ForeignKeyPrompt {
                    source_table,
                    source_column: source_column.clone(),
                    source_type,
                    target_table,
                    target_column: target_column.clone(),
                    target_type,
                }
            },
        )
        .collect()
}

/// Metadata for a matched table during routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedTable {
    pub table_name: String,
    pub best_column: String,
    /// Semantic description of the best-matching column (AI + user combined).
    pub column_description: String,
    /// Raw cosine similarity before fusion [-1, 1].
    pub similarity_score: f32,
}

/// Result of routing a user question to a data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingResult {
    /// The recommended data source ID.
    pub data_source_id: String,
    /// Matched tables with their similarity scores.
    pub matched_tables: Vec<MatchedTable>,
    /// Normalized confidence score [0.0, 1.0].
    pub confidence: f32,
    /// How the routing decision was made.
    pub method: RoutingMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMethod {
    /// Automatically selected by embedding similarity.
    Auto,
    /// User manually selected the data source.
    Manual,
    /// Suggested by AI (from `/suggest` endpoint) and accepted by user.
    Suggested,
    /// Selected by the LLM tool-calling routing engine.
    LLM,
    /// Awaiting user clarification to resolve ambiguity.
    Clarification,
}

impl std::fmt::Display for RoutingMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Manual => write!(f, "manual"),
            Self::Suggested => write!(f, "suggested"),
            Self::LLM => write!(f, "llm"),
            Self::Clarification => write!(f, "clarification"),
        }
    }
}

/// A clarification option presented to the user when the LLM detects ambiguity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationOption {
    #[serde(alias = "optionIndex")]
    pub option_index: usize,
    #[serde(alias = "dataSourceId")]
    pub data_source_id: String,
    #[serde(alias = "tableName")]
    pub table_name: String,
    #[serde(alias = "columnName")]
    pub column_name: String,
    pub reason: String,
    #[serde(alias = "simScore")]
    pub sim_score: f32,
    #[serde(default, alias = "businessMeaning")]
    pub business_meaning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClarificationHistoryItem {
    pub round: u32,
    pub user_input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_after: Option<Vec<String>>,
}

/// The resolved decision from the LLM routing step.
#[derive(Debug, Clone)]
pub enum LlmRoutingDecision {
    /// High-confidence routing — proceed directly to SQL generation.
    HighConfidence(RoutingResult),
    /// The LLM detected ambiguity and generated a clarification question.
    NeedsClarification {
        clarification_question: String,
        options: Vec<ClarificationOption>,
        domain_context: Option<String>,
    },
    /// Very low confidence — fall back to best embedding match.
    LowConfidence(RoutingResult),
    /// Cross-datasource query detected — requires multi-step execution via AgentExecutor.
    CrossDatasource {
        datasources: Vec<RoutingResult>,
        reason: String,
    },
}

/// P3-1: Multi-turn clarification context persisted in the session JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ClarificationContext {
    #[serde(alias = "originalQuestion")]
    pub original_question: String,
    #[serde(alias = "clarificationQuestion")]
    pub clarification_question: String,
    pub options: Vec<ClarificationOption>,
    /// 已经识别/确认的业务约束（用于前端展示，帮助非技术用户理解当前语义）。
    #[serde(default)]
    #[serde(alias = "confirmedRequirements")]
    pub confirmed_requirements: Vec<String>,
    /// 仍缺失的关键业务约束（用于继续追问，直到需求完整）。
    #[serde(default)]
    #[serde(alias = "missingRequirements")]
    pub missing_requirements: Vec<String>,
    /// 对每个缺失约束的可解释原因（why + how + examples）。
    #[serde(default)]
    #[serde(alias = "missingRequirementReasons")]
    pub missing_requirement_reasons: Vec<MissingRequirementReason>,
    /// Rolling list of user补充内容 for each clarification round.
    #[serde(default)]
    #[serde(alias = "clarificationHistory")]
    pub clarification_history: Vec<ClarificationHistoryItem>,
    /// Which clarification turn this is (1 = first round of clarification).
    pub turn: u32,
    #[serde(alias = "conversationId")]
    pub conversation_id: String,
}

/// Default maximum number of clarification rounds that require user input.
/// On the next round, we enter a soft fallback path instead of hard rejection.
pub const DEFAULT_MAX_CLARIFICATION_TURNS: u32 = 5;

/// Runtime-configurable cap for clarification rounds.
/// Env: `NL2SQL_MAX_CLARIFICATION_TURNS` (default: 5, clamp: 1..=20)
pub fn max_clarification_turns() -> u32 {
    std::env::var("NL2SQL_MAX_CLARIFICATION_TURNS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.clamp(1, 20))
        .unwrap_or(DEFAULT_MAX_CLARIFICATION_TURNS)
}

/// Runtime-configurable fallback granularity used when clarification reaches
/// the soft-limit and we continue generation with defaults.
/// Env: `NL2SQL_SOFT_FALLBACK_GRANULARITY` (default: `daily`)
pub fn soft_fallback_granularity() -> String {
    let raw = std::env::var("NL2SQL_SOFT_FALLBACK_GRANULARITY")
        .unwrap_or_else(|_| "daily".to_string())
        .to_ascii_lowercase();
    match raw.as_str() {
        "daily" | "weekly" | "monthly" | "quarterly" | "yearly" => raw,
        _ => "daily".to_string(),
    }
}

// ── P0-2: Multi-step execution plan ──────────────────────────────────────────

/// A single step in a multi-step execution plan generated by the LLM Planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ExecutionStep {
    /// Execute a SQL query on a specific datasource.
    Query {
        step_id: usize,
        datasource_id: String,
        sql: String,
        description: String,
        output_name: String,
        max_rows: Option<usize>,
    },
    /// Merge results from multiple previous steps.
    Merge {
        step_id: usize,
        strategy: MergeStrategy,
        inputs: Vec<MergeInput>,
        output_name: String,
        description: String,
    },
}

/// How to combine intermediate results from multiple QUERY steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MergeStrategy {
    InnerJoin { on: Vec<String> },
    LeftJoin { on: Vec<String> },
    RightJoin { on: Vec<String> },
    FullOuterJoin { on: Vec<String> },
    CrossJoin,
    UnionAll,
    UnionDistinct,
}

/// The complete execution plan generated by the LLM Planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiStepPlan {
    pub steps: Vec<ExecutionStep>,
    pub estimated_total_rows: Option<usize>,
    pub description: String,
}
