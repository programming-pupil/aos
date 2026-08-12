//! Tenant/User configuration registry — loads per-user settings from the database.
//!
//! ## Responsibilities
//!
//! - Fetch user quotas (max concurrent sessions, monthly token limits)
//! - Load per-user MCP server configurations
//! - Load per-user API keys
//! - Load per-user permission mode
//! - Cache loaded configs with LRU eviction

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::Row;
use tokio::sync::RwLock;

/// Maximum number of cached user configs.
const CONFIG_CACHE_SIZE: usize = 256;
/// Cache TTL: 5 minutes.
const CACHE_TTL_SECS: u64 = 300;
const MAX_TOKEN_USAGE_REQUEST_ID_CHARS: usize = 255;

fn merge_model_capabilities(
    profile: Option<serde_json::Value>,
    overrides: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut merged = profile.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(overrides) = overrides {
        let (Some(merged_object), Some(override_object)) =
            (merged.as_object_mut(), overrides.as_object())
        else {
            return Some(overrides);
        };
        for (key, value) in override_object {
            merged_object.insert(key.clone(), value.clone());
        }
    }
    merged
        .as_object()
        .is_some_and(|value| !value.is_empty())
        .then_some(merged)
}

#[derive(Debug, Clone)]
pub struct UserQuota {
    /// Maximum number of concurrent agent sessions for this user.
    pub max_concurrent: usize,
    /// Maximum number of workspaces per user.
    pub max_workspaces: usize,
    /// Monthly token budget (None = unlimited).
    pub monthly_tokens_limit: Option<i64>,
    /// Current month's token usage.
    pub current_month_tokens: i64,
    /// When the monthly budget resets.
    pub reset_at: Option<chrono::NaiveDate>,
}

impl Default for UserQuota {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            max_workspaces: 10,
            monthly_tokens_limit: None,
            current_month_tokens: 0,
            reset_at: None,
        }
    }
}

/// Per-user runtime configuration loaded from the database.
/// This is passed to `RuntimeBuilder` to construct a `ConversationRuntime`.
#[derive(Debug, Clone)]
pub struct UserRuntimeConfig {
    /// Shared DB pool used by runtime-scoped adapters such as DB-backed memory tools.
    pub db: Option<sqlx::SqlitePool>,
    /// The authenticated user ID.
    pub user_id: String,
    /// The tenant ID.
    pub tenant_id: String,
    /// API keys for the LLM provider (resolved from user's `api_keys`), ordered by priority.
    pub api_keys: Vec<ApiKeyEntry>,
    /// Provider name (anthropic / openai / xai).
    pub provider: String,
    /// Default model for this user.
    pub model: String,
    /// Permission mode for the agent.
    pub permission_mode: runtime::PermissionMode,
    /// List of enabled tool names. None = all tools.
    pub allowed_tools: Option<Vec<String>>,
    /// Tool names that should be hidden from the model for this scenario.
    /// This is a deny-list applied after allow-list filtering.
    pub blocked_tools: Vec<String>,
    /// MCP server configs for this user (from `mcp_server_registry`).
    pub mcp_servers: Vec<McpServerEntry>,
    /// Skill entries for this user (from `skills_registry`).
    pub skills: Vec<SkillEntry>,
    /// Hook command lists for tool lifecycle events.
    pub hooks: runtime::RuntimeHookConfig,
    /// Tenant-scoped PM search providers, loaded only for PM scenario runtimes.
    pub pm_search_providers: Vec<tools::WebSearchProviderConfig>,
    /// Whether this runtime config was loaded for an explicit product scenario.
    /// Explicit scenarios should not fall back to environment API keys because
    /// that would bypass tenant-scoped `api_keys` routing.
    pub scenario_scoped: bool,
    /// Product scenario used to load this runtime config.
    pub scenario: Option<String>,
}

/// An API key entry with metadata for failover support.
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    /// Unique key ID.
    pub id: String,
    /// The decrypted API key value.
    pub key: String,
    /// Provider name (anthropic / openai / xai).
    pub provider: String,
    /// Custom API base URL (e.g. `<https://xxx.com/v1>`).
    pub base_url: Option<String>,
    /// Specific model to use with this key.
    pub model: Option<String>,
    /// Explicit audio submit endpoint path (relative to base_url) for music models.
    pub audio_generate_path: Option<String>,
    /// Explicit audio query endpoint path (relative to base_url) for music models.
    pub audio_query_path: Option<String>,
    /// Priority for failover ordering (lower = higher priority).
    pub priority: i32,
    /// Whether this is the primary key for this tenant.
    pub is_primary: bool,
    /// Custom input price: USD per 1M tokens. None = use default pricing.
    pub input_price_per_million: Option<f64>,
    /// Custom output price: USD per 1M tokens. None = use default pricing.
    pub output_price_per_million: Option<f64>,
    /// Explicit model/key capabilities declared by operators.
    pub capabilities_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServerEntry {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Environment passed to a local stdio MCP process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub enabled: bool,
    /// Authentication type: "none", "`bearer_token`", "oauth".
    pub auth_type: String,
    /// Decrypted auth token (only present for `bearer_token` auth).
    pub auth_token: Option<String>,
    /// Extra HTTP headers as a JSON object.
    pub extra_headers: Option<serde_json::Value>,
    /// Request timeout in milliseconds.
    pub timeout_ms: Option<u32>,
}

/// A skill entry loaded from the `skills_registry` table.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill name (unique within a tenant).
    pub name: String,
    /// Optional description from SKILL.md frontmatter.
    pub description: Option<String>,
    /// Absolute path to the SKILL.md file on disk.
    pub path: String,
    /// Optional tags for categorization.
    pub tags: Vec<String>,
}

fn rd_diff_first_blocked_tools() -> Vec<String> {
    [
        // Direct file mutation must go through rd_file_changes + explicit user apply.
        "write_file",
        "edit_file",
        "NotebookEdit",
        // These tools mutate local AOS/runtime state rather than project code.
        "Config",
        "EnterPlanMode",
        "ExitPlanMode",
        // Background agents/tasks can bypass the RD task audit and Diff approval flow.
        "Agent",
        "TaskCreate",
        "RunTaskPacket",
        "TaskStop",
        "TaskUpdate",
        // Arbitrary execution surfaces are intentionally exposed through the RD test
        // command endpoint, where they can be confirmed, timed, and audited.
        "REPL",
        "PowerShell",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

/// LRU cache entry with expiry timestamp.
#[derive(Debug)]
#[expect(dead_code)]
struct CacheEntry<T> {
    value: T,
    expires_at: std::time::Instant,
}

impl<T> CacheEntry<T> {
    #[expect(dead_code)]
    fn is_expired(&self) -> bool {
        std::time::Instant::now() > self.expires_at
    }
}

/// Thread-safe config registry with in-memory caching.
pub struct TenantConfigRegistry {
    db: sqlx::SqlitePool,
    quota_cache: Arc<RwLock<LruCache<String, UserQuota>>>,
    config_cache: Arc<RwLock<LruCache<String, (UserRuntimeConfig, std::time::Instant)>>>,
    #[expect(dead_code)]
    api_key_cache: Arc<RwLock<LruCache<String, String>>>,
}

/// Simple LRU cache with fixed capacity.
#[derive(Debug)]
struct LruCache<K, V> {
    data: HashMap<K, V>,
    order: Vec<K>,
    capacity: usize,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> LruCache<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            data: HashMap::new(),
            order: Vec::new(),
            capacity,
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        if let Some(v) = self.data.get(key) {
            // Move to end (most recently used)
            self.order.retain(|k| k != key);
            self.order.push(key.clone());
            Some(v)
        } else {
            None
        }
    }

    fn insert(&mut self, key: K, value: V) {
        if self.data.contains_key(&key) {
            self.data.insert(key.clone(), value);
            self.order.retain(|k| k != &key);
            self.order.push(key);
        } else {
            if self.data.len() >= self.capacity {
                // Evict least recently used
                if let Some(lru) = self.order.first().cloned() {
                    self.data.remove(&lru);
                    self.order.remove(0);
                }
            }
            self.data.insert(key.clone(), value);
            self.order.push(key);
        }
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        self.order.retain(|k| k != key);
        self.data.remove(key)
    }

    /// Remove all entries whose key starts with the given prefix.
    fn remove_by_prefix(&mut self, prefix: &str)
    where
        K: AsRef<str>,
    {
        let keys: Vec<K> = self
            .order
            .iter()
            .filter(|k| k.as_ref().starts_with(prefix))
            .cloned()
            .collect();
        for k in keys {
            self.remove(&k);
        }
    }
}

/// Token usage parameters for recording.
#[derive(Debug, Clone)]
pub struct TokenUsageParams {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: Option<String>,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub api_key_id: Option<String>,
    pub provider: String,
    pub custom_input_price: Option<f64>,
    pub custom_output_price: Option<f64>,
}

/// Context compaction continuity record.
#[derive(Debug, Clone)]
pub struct SessionCompactionParams {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub trigger: String,
    pub strategy: String,
    pub summary_tokens: i64,
    pub removed_message_count: i64,
    pub retained_tail_tokens: i64,
    pub used_memory_refs_json: Option<serde_json::Value>,
    pub metadata_json: Option<serde_json::Value>,
}

/// Raw context entries archived when session compaction removes recoverable
/// history from the model-visible prompt.
#[derive(Debug, Clone)]
pub struct ContextArchiveParams {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub window_id: String,
    pub source: String,
    pub role: String,
    pub ordinal: i64,
    pub content: String,
    pub content_hash: String,
    pub content_kind: String,
    pub metadata_json: Option<serde_json::Value>,
}

impl TenantConfigRegistry {
    #[must_use]
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self {
            db,
            quota_cache: Arc::new(RwLock::new(LruCache::new(CONFIG_CACHE_SIZE))),
            config_cache: Arc::new(RwLock::new(LruCache::new(CONFIG_CACHE_SIZE))),
            api_key_cache: Arc::new(RwLock::new(LruCache::new(CONFIG_CACHE_SIZE))),
        }
    }

    /// Get user quota (max concurrent, token limits).
    pub async fn get_user_quota(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<UserQuota, sqlx::Error> {
        let cache_key = format!("{tenant_id}:{user_id}");

        // Check cache first
        {
            let mut cache = self.quota_cache.write().await;
            if let Some(quota) = cache.get(&cache_key) {
                return Ok(quota.clone());
            }
        }

        // Load from DB
        let quota = sqlx::query_as::<_, (Option<i32>, Option<i32>, Option<i64>, Option<i64>, Option<chrono::NaiveDate>)>(
            "SELECT max_concurrent, max_workspaces, monthly_tokens_limit, current_tokens, reset_at FROM user_quotas WHERE tenant_id = ? AND user_id = ?",
        )
        .bind(tenant_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        let quota = quota.unwrap_or((
            Some(3),  // default max_concurrent
            Some(10), // default max_workspaces
            None,
            Some(0),
            None,
        ));

        let result = UserQuota {
            #[allow(clippy::cast_sign_loss)]
            max_concurrent: quota.0.unwrap_or(3).max(0) as usize,
            #[allow(clippy::cast_sign_loss)]
            max_workspaces: quota.1.unwrap_or(10).max(0) as usize,
            monthly_tokens_limit: quota.2,
            current_month_tokens: quota.3.unwrap_or(0),
            reset_at: quota.4,
        };

        // Cache it
        {
            let mut cache = self.quota_cache.write().await;
            cache.insert(cache_key, result.clone());
        }

        Ok(result)
    }

    /// Load full per-user runtime configuration.
    ///
    /// `scenario` is the usage context (e.g. `"agent"`, `"chat"`, `"nl2sql"`) used to filter
    /// API keys. A key is included if its `scenarios` field is NULL (available to all) or
    /// contains the requested scenario. If `scenario` is None, all chat-model keys are returned.
    pub async fn load_user_config(
        &self,
        tenant_id: &str,
        user_id: &str,
        scenario: Option<&str>,
    ) -> Result<UserRuntimeConfig, sqlx::Error> {
        let cache_key = format!("{tenant_id}:{user_id}:{scenario:?}");

        // Check cache
        {
            let mut cache = self.config_cache.write().await;
            if let Some((config, expiry)) = cache.get(&cache_key) {
                if std::time::Instant::now() < *expiry {
                    tracing::debug!(
                        "load_user_config: CACHE HIT for tenant '{}' user '{}' scenario={:?} ({} MCP servers)",
                        tenant_id,
                        user_id,
                        scenario,
                        config.mcp_servers.len(),
                    );
                    return Ok(config.clone());
                }
                cache.remove(&cache_key);
            }
        }

        tracing::debug!(
            "load_user_config: CACHE MISS for tenant '{}' user '{}' scenario={:?} — loading from DB",
            tenant_id,
            user_id,
            scenario,
        );

        // Load API keys (ordered by priority for failover), scoped to the requested scenario.
        let api_keys = self.resolve_api_keys(tenant_id, scenario).await?;

        // Load MCP servers
        let mcp_servers = self.load_mcp_servers(tenant_id, user_id).await?;

        // Load skills
        let skills = self.load_skills(tenant_id).await?;

        // Load tool lifecycle hooks scoped to the requested scenario.
        let hooks = self.load_hooks(tenant_id, scenario).await?;

        // Load DB-configured search providers for PM and Chat runtimes. They are
        // passed down to WebSearch as a per-turn override, keeping tenant/provider
        // state out of process-wide environment variables.
        let pm_search_providers = if scenario
            .is_some_and(|s| s.eq_ignore_ascii_case("pm") || s.eq_ignore_ascii_case("chat"))
        {
            self.load_pm_search_providers(tenant_id).await?
        } else {
            Vec::new()
        };

        // Load permission mode (from users table or default)
        let permission_mode = self.load_permission_mode(tenant_id, user_id).await?;

        // Load default model: first API key's model takes precedence,
        // then the DEFAULT_MODEL env var, then a hardcoded fallback.
        let model = api_keys
            .first()
            .and_then(|k| k.model.clone())
            .unwrap_or_else(|| {
                std::env::var("DEFAULT_MODEL")
                    .unwrap_or_else(|_| "anthropic/claude-opus-4-8".to_string())
            });

        // Determine primary provider from first key
        let provider = api_keys
            .first()
            .map_or_else(|| "anthropic".to_string(), |k| k.provider.clone());

        let config = UserRuntimeConfig {
            db: Some(self.db.clone()),
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            api_keys,
            provider,
            model,
            permission_mode,
            allowed_tools: None,
            blocked_tools: if scenario.is_some_and(|s| s.eq_ignore_ascii_case("pm")) {
                vec!["ToolSearch".to_string(), "ListMcpResources".to_string()]
            } else if scenario.is_some_and(|s| s.eq_ignore_ascii_case("rd")) {
                // R&D Code Studio is diff-first: agents may inspect code but must not
                // silently mutate files before the user reviews and applies a patch.
                rd_diff_first_blocked_tools()
            } else {
                Vec::new()
            },
            mcp_servers,
            skills,
            hooks,
            pm_search_providers,
            scenario_scoped: scenario.is_some(),
            scenario: scenario.map(ToOwned::to_owned),
        };

        // Cache it
        let expiry = std::time::Instant::now() + Duration::from_secs(CACHE_TTL_SECS);
        {
            let mut cache = self.config_cache.write().await;
            cache.insert(cache_key, (config.clone(), expiry));
        }

        Ok(config)
    }

    /// Resolve API keys for this user/tenant, scoped to the given scenario.
    ///
    /// Returns all enabled chat-model keys that match the scenario tag, ordered by priority.
    /// A key matches if its `scenarios` field is NULL (available to all) or contains `scenario`.
    /// Embedding-type keys (`model_type = 'embedding'`) are excluded.
    async fn resolve_api_keys(
        &self,
        tenant_id: &str,
        scenario: Option<&str>,
    ) -> Result<Vec<ApiKeyEntry>, sqlx::Error> {
        self.resolve_api_keys_by_model_type(tenant_id, scenario, "chat")
            .await
    }

    fn normalize_api_key_model_type(model_type: &str) -> &'static str {
        match model_type.trim().to_ascii_lowercase().as_str() {
            "embedding" => "embedding",
            "image" => "image",
            "video" => "video",
            "audio" => "audio",
            _ => "chat",
        }
    }

    /// Resolve API keys for this tenant, scoped by scenario + model type.
    ///
    /// - `scenario`: key scope tag (NULL / empty scenarios are treated as available to all).
    /// - `model_type`: one of `chat`, `embedding`, `image`, `video`, `audio` (invalid values default to `chat`).
    ///
    /// Ordering follows `priority ASC, created_at ASC` to support deterministic failover.
    pub async fn resolve_api_keys_by_model_type(
        &self,
        tenant_id: &str,
        scenario: Option<&str>,
        model_type: &str,
    ) -> Result<Vec<ApiKeyEntry>, sqlx::Error> {
        let model_type = Self::normalize_api_key_model_type(model_type);
        let rows = match scenario {
            Some(s) => {
                // Only return keys whose scenarios JSON array contains the requested scenario
                // (NULL scenarios = available to all scenarios).
                let mut rows = sqlx::query(
                    "SELECT k.id, k.encrypted_key, k.provider, k.base_url, k.model,
                            k.priority, k.is_primary, k.audio_generate_path, k.audio_query_path,
                            CAST(k.input_price_per_million AS DOUBLE),
                            CAST(k.output_price_per_million AS DOUBLE),
                            CAST(k.capabilities_json AS TEXT),
                            CAST(p.capabilities_json AS TEXT)
                     FROM api_keys k
                     LEFT JOIN model_capability_profiles p
                       ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
                     WHERE k.tenant_id = ? AND k.enabled = 1 AND k.model_type = ?
                       AND (k.scenarios IS NULL OR json_array_length(k.scenarios) = 0
                            OR EXISTS (SELECT 1 FROM json_each(k.scenarios)
                                       WHERE json_each.value = ?))
                     ORDER BY k.priority ASC, k.created_at ASC",
                )
                .bind(tenant_id)
                .bind(model_type)
                .bind(s)
                .fetch_all(&self.db)
                .await?;

                // Product scenarios should work out of the box when the tenant
                // only configured a generic "chat" model key. Keep
                // scenario-scoped keys first, then append chat-scoped keys, then
                // any remaining chat-model keys as last-resort failover.
                if model_type == "chat" && !s.eq_ignore_ascii_case("chat") {
                    let mut seen: HashSet<String> = rows
                        .iter()
                        .map(|row| sqlx::Row::get::<String, _>(row, 0))
                        .collect();
                    for fallback_rows in [
                        sqlx::query(
                            "SELECT k.id, k.encrypted_key, k.provider, k.base_url, k.model,
                                    k.priority, k.is_primary, k.audio_generate_path, k.audio_query_path,
                                    CAST(k.input_price_per_million AS DOUBLE),
                                    CAST(k.output_price_per_million AS DOUBLE),
                                    CAST(k.capabilities_json AS TEXT),
                                    CAST(p.capabilities_json AS TEXT)
                             FROM api_keys k
                             LEFT JOIN model_capability_profiles p
                               ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
                             WHERE k.tenant_id = ? AND k.enabled = 1 AND k.model_type = ?
                               AND EXISTS (SELECT 1 FROM json_each(k.scenarios)
                                           WHERE json_each.value = ?)
                             ORDER BY k.priority ASC, k.created_at ASC",
                        )
                        .bind(tenant_id)
                        .bind(model_type)
                        .bind("chat")
                        .fetch_all(&self.db)
                        .await?,
                        sqlx::query(
                            "SELECT k.id, k.encrypted_key, k.provider, k.base_url, k.model,
                                    k.priority, k.is_primary, k.audio_generate_path, k.audio_query_path,
                                    CAST(k.input_price_per_million AS DOUBLE),
                                    CAST(k.output_price_per_million AS DOUBLE),
                                    CAST(k.capabilities_json AS TEXT),
                                    CAST(p.capabilities_json AS TEXT)
                             FROM api_keys k
                             LEFT JOIN model_capability_profiles p
                               ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
                             WHERE k.tenant_id = ? AND k.enabled = 1 AND k.model_type = ?
                             ORDER BY k.priority ASC, k.created_at ASC",
                        )
                        .bind(tenant_id)
                        .bind(model_type)
                        .fetch_all(&self.db)
                        .await?,
                    ] {
                        for row in fallback_rows {
                            let id: String = sqlx::Row::get(&row, 0);
                            if seen.insert(id) {
                                rows.push(row);
                            }
                        }
                    }
                }
                Ok(rows)
            }
            None => {
                // No scenario filter — return all keys for the selected model type.
                sqlx::query(
                    "SELECT k.id, k.encrypted_key, k.provider, k.base_url, k.model,
                            k.priority, k.is_primary, k.audio_generate_path, k.audio_query_path,
                            CAST(k.input_price_per_million AS DOUBLE),
                            CAST(k.output_price_per_million AS DOUBLE),
                            CAST(k.capabilities_json AS TEXT),
                            CAST(p.capabilities_json AS TEXT)
                     FROM api_keys k
                     LEFT JOIN model_capability_profiles p
                       ON p.id = k.model_profile_id AND p.tenant_id = k.tenant_id
                     WHERE k.tenant_id = ? AND k.enabled = 1 AND k.model_type = ?
                     ORDER BY k.priority ASC, k.created_at ASC",
                )
                .bind(tenant_id)
                .bind(model_type)
                .fetch_all(&self.db)
                .await
            }
        }?;

        let matched_row_count = rows.len();
        let mut decrypt_error_count = 0usize;
        let mut last_decrypt_error: Option<String> = None;
        let mut entries = Vec::new();

        for row in rows {
            let id: String = sqlx::Row::get(&row, 0);
            let encrypted_key: String = sqlx::Row::get(&row, 1);
            let provider: String = sqlx::Row::get(&row, 2);
            let base_url: Option<String> = sqlx::Row::get(&row, 3);
            let model: Option<String> = sqlx::Row::get(&row, 4);
            let priority: i32 = sqlx::Row::get(&row, 5);
            let is_primary: bool = sqlx::Row::get(&row, 6);
            let audio_generate_path: Option<String> = sqlx::Row::get(&row, 7);
            let audio_query_path: Option<String> = sqlx::Row::get(&row, 8);
            let input_price_per_million: Option<f64> = sqlx::Row::get(&row, 9);
            let output_price_per_million: Option<f64> = sqlx::Row::get(&row, 10);
            let explicit_capabilities: Option<serde_json::Value> =
                sqlx::Row::get::<Option<String>, _>(&row, 11)
                    .and_then(|raw| serde_json::from_str(&raw).ok());
            let profile_capabilities: Option<serde_json::Value> =
                sqlx::Row::get::<Option<String>, _>(&row, 12)
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .or_else(|| {
                        model.as_deref().map(|model| {
                            api::infer_model_profile(&provider, base_url.as_deref(), model)
                                .runtime_capabilities()
                        })
                    });
            let capabilities_json =
                merge_model_capabilities(profile_capabilities, explicit_capabilities);

            let key: Option<String> = if encrypted_key.is_empty() {
                runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK")
                    .then(|| std::env::var("ANTHROPIC_API_KEY").ok())
                    .flatten()
            } else {
                match crate::crypto::decrypt(&encrypted_key) {
                    Ok(k) => Some(k),
                    Err(e) => {
                        decrypt_error_count += 1;
                        last_decrypt_error = Some(e.to_string());
                        tracing::warn!(
                            tenant_id = %tenant_id,
                            scenario = ?scenario,
                            model_type = %model_type,
                            key_id = %id,
                            "failed to decrypt API key {}, skipping this key: {}",
                            id,
                            e
                        );
                        // Do NOT fall back to ANTHROPIC_API_KEY here — that would cause
                        // the wrong API key to be used for the user's configured provider.
                        None
                    }
                }
            };

            if let Some(key) = key {
                entries.push(ApiKeyEntry {
                    id,
                    key,
                    provider,
                    base_url,
                    model,
                    audio_generate_path,
                    audio_query_path,
                    priority,
                    is_primary,
                    input_price_per_million,
                    output_price_per_million,
                    capabilities_json,
                });
            }
        }

        if entries.is_empty() && matched_row_count > 0 {
            tracing::warn!(
                tenant_id = %tenant_id,
                scenario = ?scenario,
                model_type = %model_type,
                matched_key_rows = matched_row_count,
                decrypt_error_count,
                last_decrypt_error = last_decrypt_error.as_deref().unwrap_or("none"),
                "matched API key rows exist but no usable runtime keys were loaded"
            );
        }

        // Development-only compatibility path. Tenant workloads must not select
        // a provider merely because the process has an unrelated environment key.
        if entries.is_empty()
            && model_type == "chat"
            && runtime::explicit_env_opt_in_enabled("AOS_ALLOW_TENANT_MODEL_ENV_FALLBACK")
        {
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                entries.push(ApiKeyEntry {
                    id: "env-fallback".to_string(),
                    key,
                    provider: "anthropic".to_string(),
                    base_url: None,
                    model: None,
                    audio_generate_path: None,
                    audio_query_path: None,
                    priority: 0,
                    is_primary: true,
                    input_price_per_million: None,
                    output_price_per_million: None,
                    capabilities_json: None,
                });
            }
        }

        Ok(entries)
    }

    async fn load_pm_search_providers(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<tools::WebSearchProviderConfig>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, provider_type, enabled, priority, base_url, method, auth_type,
                   auth_secret_ciphertext, CAST(headers_json AS TEXT) AS headers_json,
                   CAST(query_template_json AS TEXT) AS query_template_json,
                   CAST(response_mapping_json AS TEXT) AS response_mapping_json,
                   timeout_secs, max_results, fetch_content_enabled, content_extract_mode,
                   CAST(domain_allowlist_json AS TEXT) AS domain_allowlist_json,
                   CAST(domain_blocklist_json AS TEXT) AS domain_blocklist_json,
                   CAST(rate_limit_json AS TEXT) AS rate_limit_json
            FROM pm_search_provider_configs
            WHERE tenant_id = ? AND enabled = 1 AND health_status <> 'unhealthy'
            ORDER BY priority ASC, created_at ASC
            "#,
        )
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await?;

        let mut providers = Vec::new();
        for row in rows {
            let provider_type_raw: String = Row::get(&row, "provider_type");
            let Some(provider_type) = parse_pm_search_provider_type(&provider_type_raw) else {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    provider_type = %provider_type_raw,
                    "skipping PM search provider with unsupported provider_type"
                );
                continue;
            };
            let auth_secret = Row::get::<Option<String>, _>(&row, "auth_secret_ciphertext")
                .and_then(|ciphertext| {
                    if ciphertext.trim().is_empty() {
                        None
                    } else {
                        match crate::crypto::decrypt(&ciphertext) {
                            Ok(secret) => Some(secret),
                            Err(error) => {
                                tracing::warn!(
                                    tenant_id = %tenant_id,
                                    provider_id = %Row::get::<String, _>(&row, "id"),
                                    "failed to decrypt PM search provider secret: {}",
                                    error
                                );
                                None
                            }
                        }
                    }
                });
            providers.push(tools::WebSearchProviderConfig {
                id: Row::get(&row, "id"),
                name: Row::get(&row, "name"),
                provider_type,
                enabled: Row::get(&row, "enabled"),
                priority: Row::get(&row, "priority"),
                base_url: Row::get(&row, "base_url"),
                method: Row::get(&row, "method"),
                auth_type: Row::get(&row, "auth_type"),
                auth_secret,
                headers_json: parse_json_value(Row::get(&row, "headers_json")),
                query_template_json: parse_json_value(Row::get(&row, "query_template_json")),
                response_mapping_json: parse_json_value(Row::get(&row, "response_mapping_json")),
                timeout_secs: Row::get::<Option<i32>, _>(&row, "timeout_secs")
                    .and_then(|v| u64::try_from(v.max(0)).ok()),
                max_results: Row::get::<Option<i32>, _>(&row, "max_results")
                    .and_then(|v| usize::try_from(v.max(0)).ok()),
                fetch_content_enabled: Row::get(&row, "fetch_content_enabled"),
                content_extract_mode: Row::get(&row, "content_extract_mode"),
                domain_allowlist: parse_json_string_vec(Row::get(&row, "domain_allowlist_json")),
                domain_blocklist: parse_json_string_vec(Row::get(&row, "domain_blocklist_json")),
                rate_limit_json: parse_json_value(Row::get(&row, "rate_limit_json")),
            });
        }
        Ok(providers)
    }

    /// Load MCP server configs for a user.
    async fn load_mcp_servers(
        &self,
        tenant_id: &str,
        _user_id: &str,
    ) -> Result<Vec<McpServerEntry>, sqlx::Error> {
        let rows = sqlx::query(
            r"
            SELECT id, name, transport, command,
                   CAST(args AS TEXT), url, enabled,
                   COALESCE(auth_type, 'none'), COALESCE(auth_token, ''),
                   CAST(COALESCE(extra_headers, '') AS TEXT),
                   CAST(COALESCE(timeout_ms, 60000) AS INTEGER),
                   CAST(COALESCE(env, '') AS TEXT)
            FROM mcp_server_registry
            WHERE tenant_id = ? AND enabled = 1
            ORDER BY created_at ASC
            ",
        )
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await?;

        tracing::info!(
            "load_mcp_servers: loaded {} rows from DB for tenant '{}'",
            rows.len(),
            tenant_id,
        );

        if rows.is_empty() {
            tracing::debug!(
                "load_mcp_servers: no enabled MCP servers found for tenant '{}'",
                tenant_id,
            );
        }

        let entries = rows
            .into_iter()
            .map(|row| {
                let id: String = sqlx::Row::get(&row, 0);
                let name: String = sqlx::Row::get(&row, 1);
                let transport: String = sqlx::Row::get(&row, 2);
                let command: Option<String> = sqlx::Row::get(&row, 3);
                let args_str: Option<String> = sqlx::Row::get(&row, 4);
                let url: Option<String> = sqlx::Row::get(&row, 5);
                let enabled: bool = sqlx::Row::get(&row, 6);
                let auth_type: String = sqlx::Row::get(&row, 7);
                let auth_token: String = sqlx::Row::get(&row, 8);
                let extra_headers_str: Option<String> = sqlx::Row::get(&row, 9);
                let timeout_ms: u32 = sqlx::Row::get(&row, 10);
                let env_str: Option<String> = sqlx::Row::get(&row, 11);

                let args: Vec<String> = args_str
                    .as_ref()
                    .and_then(|a| serde_json::from_str(a).ok())
                    .unwrap_or_default();

                let extra_headers_val: Option<serde_json::Value> = extra_headers_str
                    .as_ref()
                    .and_then(|h| serde_json::from_str(h).ok());
                let env: BTreeMap<String, String> = env_str
                    .as_ref()
                    .and_then(|value| serde_json::from_str(value).ok())
                    .unwrap_or_default();

                McpServerEntry {
                    id,
                    name,
                    transport,
                    command,
                    args,
                    env,
                    url,
                    enabled,
                    auth_type,
                    auth_token: if auth_token.is_empty() {
                        None
                    } else {
                        Some(auth_token)
                    },
                    extra_headers: extra_headers_val,
                    timeout_ms: if timeout_ms == 60000 {
                        None
                    } else {
                        Some(timeout_ms)
                    },
                }
            })
            .collect();

        Ok(entries)
    }

    /// Load enabled skills for a tenant from the `skills_registry` table.
    async fn load_skills(&self, tenant_id: &str) -> Result<Vec<SkillEntry>, sqlx::Error> {
        let rows = sqlx::query(
            r"
            SELECT name, description, path, tags
            FROM skills_registry
            WHERE tenant_id = ? AND enabled = 1
            ORDER BY name ASC
            ",
        )
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await?;

        let entries = rows
            .into_iter()
            .map(|row| {
                let name: String = sqlx::Row::get(&row, 0);
                let description: Option<String> = sqlx::Row::get(&row, 1);
                let path: String = sqlx::Row::get(&row, 2);
                let tags_str: Option<String> = sqlx::Row::get(&row, 3);
                let tags: Vec<String> = tags_str
                    .as_ref()
                    .and_then(|t| serde_json::from_str(t).ok())
                    .unwrap_or_default();
                SkillEntry {
                    name,
                    description,
                    path,
                    tags,
                }
            })
            .collect();

        Ok(entries)
    }

    /// Resolve a hook's execution command from language + code + command fields.
    ///
    /// - If `code` is set and `language = 'python'`: wraps as `python3 -c '...'`
    /// - If `code` is set and `language = 'shell'`: uses `code` directly
    /// - Otherwise: falls back to the legacy `command` column
    fn resolve_hook_command(language: &str, code: Option<&String>, command: &str) -> String {
        match language {
            "python" => {
                if let Some(c) = code {
                    if !c.is_empty() {
                        let escaped = c.replace('\'', "'\\''");
                        return format!("python3 -c '{escaped}'");
                    }
                }
                command.to_string()
            }
            "shell" | "bash" | "sh" => {
                if let Some(c) = code {
                    if !c.is_empty() {
                        return c.clone();
                    }
                }
                command.to_string()
            }
            _ => command.to_string(),
        }
    }

    /// Load enabled tool lifecycle hooks for a tenant from the `tenant_hooks` table.
    ///
    /// Commands are built as follows:
    /// - If `code` is set and `language = 'python'`: wraps as `python3 -c '...'`
    /// - If `code` is set and `language = 'shell'`: uses `code` directly
    /// - Otherwise: falls back to the legacy `command` column
    async fn load_hooks(
        &self,
        tenant_id: &str,
        scenario: Option<&str>,
    ) -> Result<runtime::RuntimeHookConfig, sqlx::Error> {
        let rows = match scenario {
            Some(s) => {
                sqlx::query(
                    r"
                    SELECT id, tenant_id, event_type, language,
                           CAST(code AS TEXT),
                           CAST(command AS TEXT),
                           timeout_seconds, fail_fast
                    FROM tenant_hooks
                    WHERE tenant_id = ? AND enabled = 1
                      AND (scenarios IS NULL OR json_array_length(scenarios) = 0 OR EXISTS (SELECT 1 FROM json_each(scenarios) WHERE json_each.value = ?))
                    ORDER BY priority ASC
                    ",
                )
                .bind(tenant_id)
                .bind(s)
                .fetch_all(&self.db)
                .await?
            }
            None => {
                sqlx::query(
                    r"
                    SELECT id, tenant_id, event_type, language,
                           CAST(code AS TEXT),
                           CAST(command AS TEXT),
                           timeout_seconds, fail_fast
                    FROM tenant_hooks
                    WHERE tenant_id = ? AND enabled = 1
                    ORDER BY priority ASC
                    ",
                )
                .bind(tenant_id)
                .fetch_all(&self.db)
                .await?
            }
        };

        let mut pre_tool_use = Vec::new();
        let mut post_tool_use = Vec::new();
        let mut post_tool_use_failure = Vec::new();
        let mut lifecycle: BTreeMap<String, Vec<runtime::RuntimeHookEntry>> = BTreeMap::new();

        for row in rows {
            let id: String = row.get(0);
            let row_tenant_id: String = row.get(1);
            let event_type: String = row.get(2);
            let language: String = row.get(3);
            let code: Option<String> = row.get(4);
            let command: String = row.get(5);
            let timeout_seconds: u32 = row.get(6);
            let fail_fast: bool = row.get(7);

            let resolved = Self::resolve_hook_command(&language, code.as_ref(), &command);
            let entry = runtime::RuntimeHookEntry::db(
                id,
                row_tenant_id,
                resolved,
                (timeout_seconds > 0).then_some(timeout_seconds),
                fail_fast,
            );
            match event_type.as_str() {
                "pre_tool_use" => pre_tool_use.push(entry),
                "post_tool_use" => post_tool_use.push(entry),
                "post_tool_use_failure" => post_tool_use_failure.push(entry),
                other if runtime::HookEvent::from_db_key(other).is_some() => {
                    lifecycle.entry(other.to_string()).or_default().push(entry);
                }
                _ => {}
            }
        }

        Ok(runtime::RuntimeHookConfig::new_entries_with_lifecycle(
            pre_tool_use,
            post_tool_use,
            post_tool_use_failure,
            lifecycle,
        ))
    }

    /// Load permission mode for a user.
    async fn load_permission_mode(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<runtime::PermissionMode, sqlx::Error> {
        let mode: Option<String> =
            sqlx::query_scalar("SELECT permission_mode FROM users WHERE id = ? AND tenant_id = ?")
                .bind(user_id)
                .bind(tenant_id)
                .fetch_optional(&self.db)
                .await?;

        Ok(match mode.as_deref() {
            Some("read_only") => runtime::PermissionMode::ReadOnly,
            Some("danger_full_access") => runtime::PermissionMode::DangerFullAccess,
            _ => runtime::PermissionMode::WorkspaceWrite,
        })
    }

    /// Invalidate all cached configs for a user. Call after config updates (API keys, MCP, etc.)
    /// since changes affect all scenarios.
    pub async fn invalidate_cache(&self, tenant_id: &str, user_id: &str) {
        let prefix = format!("{tenant_id}:{user_id}:");
        {
            let mut cache = self.config_cache.write().await;
            cache.remove_by_prefix(&prefix);
        }
        {
            let mut cache = self.quota_cache.write().await;
            cache.remove(&format!("{tenant_id}:{user_id}"));
        }
    }

    /// Invalidate all cached runtime/quota entries for a tenant across users.
    ///
    /// Useful after tenant-scoped API-key changes where key visibility may affect
    /// all user+scenario cache keys.
    pub async fn invalidate_tenant_cache(&self, tenant_id: &str) {
        let prefix = format!("{tenant_id}:");
        {
            let mut cache = self.config_cache.write().await;
            cache.remove_by_prefix(&prefix);
        }
        {
            let mut cache = self.quota_cache.write().await;
            cache.remove_by_prefix(&prefix);
        }
    }

    /// Get the current API keys version for a tenant.
    /// This is incremented whenever API keys are added/updated/deleted.
    /// Returns 0 if the column doesn't exist yet (backward compatibility for
    /// deployments that haven't run migration 030) or on DB errors.
    pub async fn get_api_keys_version(&self, tenant_id: &str) -> u64 {
        match sqlx::query("SELECT api_keys_version FROM tenants WHERE id = ?")
            .bind(tenant_id)
            .fetch_optional(&self.db)
            .await
        {
            Ok(Some(row)) => u64::try_from(row.get::<i64, _>(0)).unwrap_or(0),
            Ok(None) => 0,
            Err(e) => {
                tracing::warn!(
                    "get_api_keys_version: failed (column may not exist yet — run migration 030): {}",
                    e
                );
                0
            }
        }
    }

    /// Update token usage for a user (called after each agent turn).
    pub async fn record_token_usage(&self, params: TokenUsageParams) -> Result<(), sqlx::Error> {
        // Update user_quotas cumulative token count
        sqlx::query(
            "UPDATE user_quotas SET current_tokens = current_tokens + ? + ? WHERE tenant_id = ? AND user_id = ?",
        )
        .bind(params.input_tokens)
        .bind(params.output_tokens)
        .bind(&params.tenant_id)
        .bind(&params.user_id)
        .execute(&self.db)
        .await?;

        // Insert detailed record into token_usage for history
        let id = uuid::Uuid::new_v4().to_string();
        let request_id = normalize_token_usage_request_id(Some(&id));
        let total_tokens = params.input_tokens
            + params.output_tokens
            + params.cache_creation_tokens
            + params.cache_read_tokens;
        let (pricing, pricing_source) = runtime::pricing_with_provenance(
            &params.model,
            params.custom_input_price,
            params.custom_output_price,
        );
        let usage = runtime::TokenUsage {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            input_tokens: params.input_tokens as u32,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            output_tokens: params.output_tokens as u32,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            cache_creation_input_tokens: params.cache_creation_tokens as u32,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            cache_read_input_tokens: params.cache_read_tokens as u32,
        };
        let cost = pricing.map_or(0.0, |pricing| {
            usage
                .estimate_cost_usd_with_pricing(pricing)
                .total_cost_usd()
        });

        sqlx::query(
            r"
            INSERT INTO token_usage
                (id, tenant_id, user_id, session_id, request_id, model,
                 input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, total_tokens,
                 estimated_cost_usd, api_key_id, provider, usage_kind, pricing_source, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'request_delta', ?, CURRENT_TIMESTAMP)
            ",
        )
        .bind(&id)
        .bind(&params.tenant_id)
        .bind(&params.user_id)
        .bind(&params.session_id)
        .bind(&request_id)
        .bind(&params.model)
        .bind(params.input_tokens)
        .bind(params.output_tokens)
        .bind(params.cache_creation_tokens)
        .bind(params.cache_read_tokens)
        .bind(total_tokens)
        .bind(cost)
        .bind(&params.api_key_id)
        .bind(&params.provider)
        .bind(pricing_source.as_str())
        .execute(&self.db)
        .await?;

        Ok(())
    }

    pub async fn latest_compaction_window(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            r#"
            SELECT window_id
            FROM agent_session_compactions
            WHERE tenant_id = ? AND user_id = ? AND session_id = ?
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .fetch_optional(&self.db)
        .await
    }

    pub async fn record_session_compaction(
        &self,
        params: SessionCompactionParams,
    ) -> Result<String, sqlx::Error> {
        let window_id = format!("ctx-{}", uuid::Uuid::new_v4());
        let previous_window_id = self
            .latest_compaction_window(&params.tenant_id, &params.user_id, &params.session_id)
            .await
            .ok()
            .flatten();
        let used_memory_refs_json = serde_json::to_string(
            &params
                .used_memory_refs_json
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .ok();
        let metadata_json = serde_json::to_string(
            &params
                .metadata_json
                .unwrap_or_else(|| serde_json::json!({})),
        )
        .ok();
        sqlx::query(
            r#"
            INSERT INTO agent_session_compactions
              (id, tenant_id, user_id, session_id, window_id, previous_window_id, trigger,
               strategy, summary_tokens, removed_message_count, retained_tail_tokens,
               used_memory_refs_json, metadata_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?), json(?))
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&params.tenant_id)
        .bind(&params.user_id)
        .bind(&params.session_id)
        .bind(&window_id)
        .bind(previous_window_id)
        .bind(&params.trigger)
        .bind(&params.strategy)
        .bind(params.summary_tokens)
        .bind(params.removed_message_count)
        .bind(params.retained_tail_tokens)
        .bind(used_memory_refs_json)
        .bind(metadata_json)
        .execute(&self.db)
        .await?;
        Ok(window_id)
    }

    pub async fn record_context_archives(
        &self,
        entries: Vec<ContextArchiveParams>,
    ) -> Result<usize, sqlx::Error> {
        let mut inserted = 0usize;
        for entry in entries {
            let metadata_json = serde_json::to_string(
                &entry.metadata_json.unwrap_or_else(|| serde_json::json!({})),
            )
            .ok();
            let result = sqlx::query(
                r#"
                INSERT INTO agent_context_archives
                  (id, tenant_id, user_id, session_id, window_id, source, role, ordinal,
                   content, content_hash, content_kind, char_count, metadata_json)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, json(?))
                ON CONFLICT DO UPDATE SET
                  metadata_json = COALESCE(excluded.metadata_json, metadata_json)
                "#,
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&entry.tenant_id)
            .bind(&entry.user_id)
            .bind(&entry.session_id)
            .bind(&entry.window_id)
            .bind(&entry.source)
            .bind(&entry.role)
            .bind(entry.ordinal)
            .bind(&entry.content)
            .bind(&entry.content_hash)
            .bind(&entry.content_kind)
            .bind(entry.content.chars().count() as i64)
            .bind(metadata_json)
            .execute(&self.db)
            .await?;
            inserted = inserted.saturating_add(result.rows_affected() as usize);
        }
        Ok(inserted)
    }

    pub async fn record_hook_progress_events(
        &self,
        events: &[crate::events::AgentEvent],
        scenario: Option<&str>,
    ) {
        for event in events {
            let crate::events::AgentEvent::HookProgress { phase, detail } = event else {
                continue;
            };
            if phase != "completed" && phase != "cancelled" {
                continue;
            }
            let Some(detail) = detail else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(detail) else {
                continue;
            };
            let Some(hook_id) = value.get("hookId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(tenant_id) = value.get("tenantId").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let event_type = match value
                .get("eventType")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
            {
                raw if runtime::HookEvent::from_event_name(raw).is_some() => {
                    runtime::HookEvent::from_event_name(raw)
                        .map(runtime::HookEvent::db_key)
                        .unwrap_or(raw)
                }
                other => other,
            };
            if event_type.is_empty() {
                continue;
            }

            let tool_name = value
                .get("toolName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let exit_code = value
                .get("exitCode")
                .and_then(serde_json::Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let duration_ms = value
                .get("durationMs")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(phase.as_str());
            let stderr = value
                .get("stderr")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let input_json = value
                .get("toolInput")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
            let output_json = serde_json::json!({
                "status": status,
                "command": value.get("command"),
                "stdout": value.get("stdout"),
                "stderr": value.get("stderr"),
                "toolOutput": value.get("toolOutput"),
            })
            .to_string();
            let error_message = if status == "failed" || status == "cancelled" || !stderr.is_empty()
            {
                Some(if stderr.is_empty() { status } else { stderr }.to_string())
            } else {
                None
            };

            if let Err(error) = sqlx::query(
                r"
                INSERT INTO hook_execution_logs
                    (id, tenant_id, hook_id, event_type, scenario, tool_name, input_json, output_json,
                     exit_code, duration_ms, error_message)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(tenant_id)
            .bind(hook_id)
            .bind(event_type)
            .bind(scenario)
            .bind(tool_name)
            .bind(input_json)
            .bind(output_json)
            .bind(exit_code)
            .bind(duration_ms)
            .bind(error_message)
            .execute(&self.db)
            .await
            {
                tracing::warn!(
                    hook_id,
                    tenant_id,
                    error = %error,
                    "failed to persist hook execution log"
                );
            }
        }
    }

    /// Persist a new agent session to the DB.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_session_record(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
        workspace: &std::path::Path,
        source: &str,
        name: &str,
        model: &str,
        model_pinned: bool,
        provider: &str,
    ) -> Result<(), sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            r"
            INSERT INTO agent_sessions (id, tenant_id, user_id, session_id, name, model, model_pinned, workspace_path, state, source, provider, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'idle', ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT DO UPDATE SET workspace_path = excluded.workspace_path, state = 'idle', model = excluded.model, model_pinned = excluded.model_pinned, provider = excluded.provider
            ",
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(name)
        .bind(model)
        .bind(model_pinned)
        .bind(workspace.to_string_lossy().as_ref())
        .bind(source)
        .bind(provider)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    /// Mark a session as finished.
    pub async fn finish_session(&self, session_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE agent_sessions SET state = 'completed', updated_at = CURRENT_TIMESTAMP WHERE session_id = ?",
        )
        .bind(session_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Persist the effective runtime selection after a session hot-reload.
    pub async fn update_session_runtime_selection(
        &self,
        session_id: &str,
        model: &str,
        model_pinned: bool,
        provider: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE agent_sessions
             SET model = ?, model_pinned = ?, provider = ?, updated_at = CURRENT_TIMESTAMP
             WHERE session_id = ?",
        )
        .bind(model)
        .bind(model_pinned)
        .bind(provider)
        .bind(session_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Persist a session's pinned state to the DB.
    pub async fn update_session_pin(
        &self,
        session_id: &str,
        is_pinned: bool,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE agent_sessions SET is_pinned = ? WHERE session_id = ?")
            .bind(is_pinned)
            .bind(session_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Get a session's full record from the DB by `session_id`.
    /// Returns the session metadata needed to restore an inactive session.
    pub async fn get_agent_session_record(
        &self,
        session_id: &str,
    ) -> Result<Option<AgentSessionRecord>, sqlx::Error> {
        let row = sqlx::query_as::<_, AgentSessionRow>(
            "SELECT session_id, name, model, model_pinned, state, is_pinned, is_bookmarked, source, workspace_path, provider, tenant_id, user_id, created_at, updated_at FROM agent_sessions WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|r| AgentSessionRecord {
            session_id: r.session_id,
            name: r.name,
            model: r.model,
            model_pinned: r.model_pinned,
            state: r.state,
            is_pinned: r.is_pinned,
            is_bookmarked: r.is_bookmarked,
            source: r.source,
            workspace_path: r.workspace_path,
            provider: r.provider,
            tenant_id: r.tenant_id,
            user_id: r.user_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }))
    }

    /// Get a session's pinned state from the DB.
    pub async fn get_session_pin(&self, session_id: &str) -> Result<bool, sqlx::Error> {
        let rec = self.get_agent_session_record(session_id).await?;
        Ok(rec.is_some_and(|r| r.is_pinned))
    }

    /// Get a session's bookmark state from the DB.
    pub async fn get_session_bookmark(&self, session_id: &str) -> Result<bool, sqlx::Error> {
        let rec = self.get_agent_session_record(session_id).await?;
        Ok(rec.is_some_and(|r| r.is_bookmarked))
    }

    /// Toggle bookmark state of a session and return the new state.
    /// Gracefully handles the case where the `is_bookmarked` column does not exist
    /// (e.g., when migration 032 has not been applied yet) by returning `Ok(false)`.
    pub async fn toggle_session_bookmark(&self, session_id: &str) -> Result<bool, sqlx::Error> {
        let current = self.get_session_bookmark(session_id).await.unwrap_or(false);
        let new_state = !current;
        match sqlx::query("UPDATE agent_sessions SET is_bookmarked = ? WHERE session_id = ?")
            .bind(new_state)
            .bind(session_id)
            .execute(&self.db)
            .await
        {
            Ok(result) => {
                if result.rows_affected() == 0 {
                    tracing::warn!(
                        session_id,
                        "bookmark toggle: no rows updated — session may not exist"
                    );
                }
                Ok(new_state)
            }
            Err(sqlx::Error::Database(ref db_err))
                if db_err.code().is_some_and(|c| c == "42S22") =>
            {
                tracing::warn!(
                    session_id,
                    "bookmark toggle: `is_bookmarked` column missing — run migration 032. \
                     Bookmarking is a no-op until the column is added."
                );
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// List agent sessions for a user from the DB, sorted by pinned (desc) then last updated (desc).
    pub async fn list_agent_sessions(
        &self,
        tenant_id: &str,
        user_id: &str,
        source_filter: Option<&str>,
    ) -> Result<Vec<AgentSessionRecord>, sqlx::Error> {
        let rows = if let Some(source) = source_filter {
            sqlx::query_as::<_, AgentSessionRow>(
                r"
                SELECT session_id, name, model, model_pinned, state, is_pinned, is_bookmarked, source, workspace_path, provider, tenant_id, user_id, created_at, updated_at
                FROM agent_sessions
                WHERE tenant_id = ? AND user_id = ? AND source = ? AND state != 'completed'
                ORDER BY is_bookmarked DESC, is_pinned DESC, updated_at DESC
                ",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(source)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query_as::<_, AgentSessionRow>(
                r"
                SELECT session_id, name, model, model_pinned, state, is_pinned, is_bookmarked, source, workspace_path, provider, tenant_id, user_id, created_at, updated_at
                FROM agent_sessions
                WHERE tenant_id = ? AND user_id = ? AND state != 'completed'
                ORDER BY is_bookmarked DESC, is_pinned DESC, updated_at DESC
                ",
            )
            .bind(tenant_id)
            .bind(user_id)
            .fetch_all(&self.db)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|r| AgentSessionRecord {
                session_id: r.session_id,
                name: r.name,
                model: r.model,
                model_pinned: r.model_pinned,
                state: r.state,
                is_pinned: r.is_pinned,
                is_bookmarked: r.is_bookmarked,
                source: r.source,
                workspace_path: r.workspace_path,
                provider: r.provider,
                tenant_id: r.tenant_id,
                user_id: r.user_id,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect())
    }

    /// Update a session's name in the DB.
    pub async fn update_session_name(
        &self,
        session_id: &str,
        name: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE agent_sessions SET name = ? WHERE session_id = ?")
            .bind(name)
            .bind(session_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct AgentSessionRow {
    pub session_id: String,
    pub name: Option<String>,
    pub model: String,
    pub model_pinned: bool,
    pub state: String,
    pub is_pinned: bool,
    pub is_bookmarked: bool,
    pub source: String,
    pub workspace_path: String,
    pub provider: String,
    pub tenant_id: String,
    pub user_id: String,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug)]
pub struct AgentSessionRecord {
    pub session_id: String,
    pub name: Option<String>,
    pub model: String,
    pub model_pinned: bool,
    pub state: String,
    pub is_pinned: bool,
    pub is_bookmarked: bool,
    pub source: String,
    pub workspace_path: String,
    pub provider: String,
    pub tenant_id: String,
    pub user_id: String,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

fn normalize_token_usage_request_id(value: Option<&str>) -> Option<String> {
    let raw = value.map(str::trim).filter(|value| !value.is_empty())?;
    if raw.chars().count() <= MAX_TOKEN_USAGE_REQUEST_ID_CHARS {
        return Some(raw.to_string());
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw.hash(&mut hasher);
    let suffix = format!("{:016x}", hasher.finish());
    let head_chars = MAX_TOKEN_USAGE_REQUEST_ID_CHARS
        .saturating_sub(suffix.len())
        .saturating_sub(1);
    let mut normalized = raw.chars().take(head_chars).collect::<String>();
    normalized.push('#');
    normalized.push_str(&suffix);
    tracing::warn!(
        original_chars = raw.chars().count(),
        normalized_chars = normalized.chars().count(),
        "token usage request_id exceeded storage budget; stored compact form"
    );
    Some(normalized)
}

fn parse_pm_search_provider_type(raw: &str) -> Option<tools::WebSearchProviderType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "brave" => Some(tools::WebSearchProviderType::Brave),
        "tavily" => Some(tools::WebSearchProviderType::Tavily),
        "serper" => Some(tools::WebSearchProviderType::Serper),
        "exa" => Some(tools::WebSearchProviderType::Exa),
        "searxng" | "searx_ng" => Some(tools::WebSearchProviderType::Searxng),
        "generic_json" | "generic" => Some(tools::WebSearchProviderType::GenericJson),
        "internal_http" | "internal" => Some(tools::WebSearchProviderType::InternalHttp),
        "demo" | "demo_search" => Some(tools::WebSearchProviderType::DemoSearch),
        _ => None,
    }
}

fn parse_json_value(raw: Option<String>) -> Option<serde_json::Value> {
    raw.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str(value).ok())
}

fn parse_json_string_vec(raw: Option<String>) -> Option<Vec<String>> {
    let value = parse_json_value(raw)?;
    match value {
        serde_json::Value::Array(items) => {
            let out = items
                .into_iter()
                .filter_map(|item| item.as_str().map(str::trim).map(ToOwned::to_owned))
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::rd_diff_first_blocked_tools;

    #[test]
    fn rd_diff_first_blocks_mutating_and_background_tools() {
        let blocked = rd_diff_first_blocked_tools();
        for tool in [
            "write_file",
            "edit_file",
            "NotebookEdit",
            "Agent",
            "TaskCreate",
            "RunTaskPacket",
            "REPL",
            "PowerShell",
        ] {
            assert!(
                blocked.iter().any(|item| item == tool),
                "RD scenario must hide {tool} from the runtime tool list"
            );
        }
    }
}
