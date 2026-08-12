//! Data Sources API — CRUD for multi-tenant data source registry.
//!
//! Supports three driver types:
//!   - `sql`    — `MySQL` / `TiDB` / `PostgreSQL` / `ClickHouse` / `Presto` / `Hive` via sqlx
//!   - `http_api` — Custom REST APIs with schema definition
//!   - `mcp`    — Referenced MCP servers (no direct connection here)
//!
//! Sensitive fields in `config` (passwords, tokens) are encrypted before storage
//! using AES-256-GCM (same key as API key encryption).

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use axum::{
    body::Bytes,
    extract::{Extension, Path, Query, State},
    routing::{
        delete as routing_delete, get as routing_get, patch, post as routing_post,
        put as routing_put,
    },
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashSet;
use std::sync::Arc;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::PaginationParams;
use crate::state::AppState;
use nl2sql_domain::datasource_config::{
    build_mongodb_uri, build_mysql_url, build_postgres_url, format_datasource_description,
    normalize_host_input, redact_sensitive_config, ClickHouseConfig, MongoConfig, SqlConfig,
    TrinoConfig,
};
pub(crate) use nl2sql_domain::datasource_config::{
    build_mysql_url_parts, build_postgres_url_parts,
};
use std::path::PathBuf;

/// A single SQL query parameter, preserving column order for correct binding.
enum SqlBindValue {
    String(String),
    Json(serde_json::Value),
    Null,
}

async fn validate_knowledge_bindings_after_visibility_change(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    next_visibility: &str,
) -> Result<()> {
    let conflicting_pack: Option<String> = sqlx::query_scalar(
        "SELECT p.name \
         FROM nl2sql_reference_packs p \
         WHERE p.tenant_id = ? AND p.scope <> 'tenant' \
           AND EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) current_binding WHERE current_binding.value = ?) \
           AND EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) other_binding \
                       JOIN data_sources other_ds ON other_ds.tenant_id = p.tenant_id AND other_ds.id = other_binding.value \
                       WHERE other_binding.value <> ? AND other_ds.visibility <> ?) \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .bind(next_visibility)
    .fetch_optional(db)
    .await?;
    if let Some(pack_name) = conflicting_pack {
        return Err(AppError::ValidationError(format!(
            "changing visibility would mix tenant and private data sources in knowledge space '{pack_name}'; update or remove that knowledge-space binding first"
        )));
    }
    Ok(())
}

fn map_schema_discovery_error(db_type: &str, error: String) -> AppError {
    if matches!(db_type, "trino" | "presto") && is_actionable_trino_discovery_error(&error) {
        return AppError::ValidationError(error);
    }
    AppError::Internal(error)
}

fn is_actionable_trino_discovery_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("hive metastore")
        || lower.contains("hive_metastore_error")
        || lower.contains("sockettimeoutexception")
        || lower.contains("read timed out")
        || (lower.contains("information_schema.tables")
            && (lower.contains("timed out") || lower.contains("timeout")))
        || (lower.contains("information_schema.columns")
            && (lower.contains("timed out") || lower.contains("timeout")))
        || lower.contains("show tables")
        || lower.contains("show schemas")
        || lower.contains("show databases")
        || error.contains("获取表列表失败")
        || error.contains("获取表结构失败")
        || error.contains("获取 Schema 失败")
}

fn aggregate_embedding_usage(usages: &[api::Usage]) -> Option<api::Usage> {
    if usages.is_empty() {
        return None;
    }
    let mut agg = api::Usage::default();
    for usage in usages {
        agg.input_tokens = agg.input_tokens.saturating_add(usage.input_tokens);
        agg.output_tokens = agg.output_tokens.saturating_add(usage.output_tokens);
        agg.cache_creation_input_tokens = agg
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
        agg.cache_read_input_tokens = agg
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }
    Some(agg)
}

async fn persist_embedding_usage(
    usage_writer: Option<Arc<crate::routes::chat::TokenUsageWriter>>,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    request_id: Option<&str>,
    model: &str,
    api_key_id: Option<String>,
    usage: Option<api::Usage>,
) {
    let (Some(writer), Some(usage)) = (usage_writer, usage) else {
        return;
    };
    let total_tokens = usage.total_tokens();
    if total_tokens == 0 {
        return;
    }
    let record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        request_id: request_id.map(std::string::ToString::to_string),
        model: model.to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        total_tokens,
        estimated_cost_usd: usage.estimated_cost_usd(model).total_cost_usd(),
        api_key_id,
        provider: "nl2sql_embedding".to_string(),
        created_at: chrono::Utc::now(),
    };
    if let Err(e) = writer.write(&record).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            session_id = %session_id,
            error = %e,
            "failed to persist datasource embedding token usage"
        );
    }
}

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DataSourceInfo {
    pub id: String,
    pub tenant_id: String,
    pub user_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub db_type: String,
    pub visibility: String,
    /// Config without sensitive fields; sensitive keys are redacted.
    pub config_preview: serde_json::Value,
    /// Full plaintext config, returned only by the single-datasource detail
    /// endpoint after the normal owner/admin permission check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_plain: Option<serde_json::Value>,
    pub schema_info: Option<serde_json::Value>,
    pub enabled: bool,
    pub last_tested_at: Option<String>,
    pub last_error: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sensitive_columns: Option<serde_json::Value>,
    pub embedding_status: String,
}

#[derive(Debug, Serialize)]
pub struct DataSourceListResponse {
    pub data_sources: Vec<DataSourceInfo>,
    pub total: usize,
}

#[derive(Debug, Deserialize)]
pub struct CreateDataSourceRequest {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub db_type: String,
    pub visibility: Option<String>,
    pub config: serde_json::Value,
    pub schema_info: Option<serde_json::Value>,
    pub sensitive_columns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDataSourceRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<String>,
    pub config: Option<serde_json::Value>,
    pub schema_info: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub sensitive_columns: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct TestConnectionResponse {
    pub success: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub schema_preview: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct DiscoverTrinoSchemasRequest {
    pub host: String,
    pub port: Option<u16>,
    pub catalog: String,
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub ssl: Option<bool>,
    #[serde(default)]
    pub basic_auth: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct DiscoverTrinoSchemasResponse {
    pub catalog: String,
    pub schemas: Vec<String>,
    pub method: String,
    pub warnings: Vec<String>,
}

/// A single data source entry in a batch export file.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedDataSource {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub db_type: String,
    pub visibility: String,
    /// Full config (with sensitive fields) is included in exports so the
    /// importer can re-use it. Consumers should treat this as confidential.
    pub config: serde_json::Value,
    pub schema_info: Option<serde_json::Value>,
    pub sensitive_columns: Option<Vec<String>>,
    pub enabled: bool,
    /// Semantic descriptions for tables and columns.
    /// Maps `table_name` -> list of column semantics.
    pub table_semantics: Vec<ExportedTableSemantics>,
    /// Datasource-level semantic description.
    pub datasource_semantics: Option<ExportedDatasourceSemantics>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedTableSemantics {
    pub table_name: String,
    pub table_description: Option<String>,
    pub columns: Vec<ExportedColumnSemantics>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedColumnSemantics {
    pub column_name: String,
    pub description: String,
    /// "ai" or "user"
    pub description_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedDatasourceSemantics {
    pub description: String,
    pub description_type: String,
}

/// Response for batch export — a JSON file containing all data sources.
#[derive(Debug, Serialize)]
pub struct BatchExportResponse {
    pub version: String,
    pub exported_at: String,
    pub data_sources: Vec<ExportedDataSource>,
}

/// Request body for batch import.
#[derive(Debug, Deserialize)]
pub struct BatchImportRequest {
    /// What to do when a data source with the same name already exists.
    /// "skip" (default) — keep existing, don't update
    /// "overwrite" — replace existing
    #[serde(default)]
    pub on_existing: String,
    pub data_sources: Vec<ExportedDataSource>,
}

/// Result for a single import entry.
#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub name: String,
    pub status: String,
    pub id: Option<String>,
    pub error: Option<String>,
}

/// Outcome of importing a single data source.
enum ImportOutcome {
    Created(String),
    Skipped(String),
    Failed(String),
}

/// Response for batch import.
#[derive(Debug, Serialize)]
pub struct BatchImportResponse {
    pub total: usize,
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub results: Vec<ImportResult>,
}

// ── Encryption helpers ────────────────────────────────────────────────────────

/// Fatal crypto failures that must not be hidden from operators.
/// Returned by [`encrypt_config`] / [`decrypt_config`] instead of papering
/// over errors with `{}` — silently losing user config is worse than
/// surfacing a 500.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption key is missing or invalid at {}/.encryption_key; run `aos keygen` or provide a 64-hex key", .0.display())]
    MissingKey(std::path::PathBuf),
    #[error("AES-GCM encryption failed: {0}")]
    Encrypt(String),
    #[error("AES-GCM decryption failed (wrong key or tampered ciphertext): {0}")]
    Decrypt(String),
    #[error("ciphertext payload malformed: {0}")]
    Malformed(String),
    #[error("config JSON (de)serialisation failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<CryptoError> for AppError {
    fn from(e: CryptoError) -> Self {
        AppError::Internal(e.to_string())
    }
}

/// Load the AES-256 key.
///
/// The key file is `{data_dir}/.encryption_key` and must contain exactly
/// 64 hex characters (32 bytes). If the file is missing we generate one
/// automatically **only in development** — set `AOS_STRICT_ENCRYPTION=1`
/// (the default in production) to forbid auto-generation and fail loudly
/// instead of silently using an ephemeral key, which would render any
/// previously-stored credentials unreadable on restart.
pub fn get_encryption_key(
    data_dir: &std::path::Path,
) -> std::result::Result<[u8; 32], CryptoError> {
    let key_path = data_dir.join(".encryption_key");
    if let Ok(key_hex) = std::fs::read_to_string(&key_path) {
        let key_hex = key_hex.trim();
        if key_hex.len() == 64 {
            if let Ok(decoded) = hex::decode(key_hex) {
                if decoded.len() == 32 {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&decoded);
                    return Ok(key);
                }
            }
        }
        return Err(CryptoError::MissingKey(key_path));
    }

    // Key does not exist yet. In strict mode this is fatal.
    let strict = std::env::var("AOS_STRICT_ENCRYPTION")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    if strict {
        return Err(CryptoError::MissingKey(key_path));
    }

    // Dev-mode: generate a fresh random key and persist it so restarts
    // don't invalidate existing ciphertexts. Two UUIDs give us 32 bytes of
    // CSPRNG-quality randomness without pulling in another dependency.
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let hex_key = {
        use std::fmt::Write;
        let mut s = String::with_capacity(64);
        for b in key {
            let _ = write!(s, "{b:02x}");
        }
        s
    };
    if let Some(parent) = key_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&key_path, hex_key) {
        tracing::error!(error = %e, path = %key_path.display(), "failed to persist generated encryption key");
        return Err(CryptoError::MissingKey(key_path));
    }
    // Restrict permissions on POSIX.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::warn!(
        path = %key_path.display(),
        "generated new encryption key (dev mode). Set AOS_STRICT_ENCRYPTION=1 in production."
    );
    Ok(key)
}

fn encrypt_config(
    config: &serde_json::Value,
    data_dir: &std::path::Path,
) -> std::result::Result<serde_json::Value, CryptoError> {
    let key = get_encryption_key(data_dir)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| CryptoError::Encrypt(e.to_string()))?;
    let nonce_bytes = generate_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = config.to_string();
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| CryptoError::Encrypt(e.to_string()))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend(ciphertext);

    Ok(serde_json::json!({
        "_encrypted": true,
        "nonce": BASE64.encode(nonce_bytes),
        "data": BASE64.encode(combined),
    }))
}

pub fn decrypt_config(
    encrypted: &serde_json::Value,
    data_dir: &std::path::Path,
) -> std::result::Result<serde_json::Value, CryptoError> {
    // Non-encrypted config (freshly imported / legacy rows) passes through
    // unchanged so callers don't need to special-case it.
    if encrypted.get("_encrypted").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(encrypted.clone());
    }

    let combined_b64 = encrypted["data"]
        .as_str()
        .ok_or_else(|| CryptoError::Malformed("missing `data` field".into()))?;
    let combined = BASE64
        .decode(combined_b64)
        .map_err(|e| CryptoError::Malformed(format!("base64 decode: {e}")))?;
    if combined.len() < 12 {
        return Err(CryptoError::Malformed(
            "ciphertext shorter than nonce prefix".into(),
        ));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Try the current key first. If that fails, fall back to the legacy
    // all-zero key that earlier builds used before the key-file check was
    // made strict. This keeps previously-saved datasources readable after
    // upgrading; the warning nudges operators to re-save so the row gets
    // rewritten with the current key.
    let current_key = get_encryption_key(data_dir)?;
    let current_cipher =
        Aes256Gcm::new_from_slice(&current_key).map_err(|e| CryptoError::Decrypt(e.to_string()))?;
    match current_cipher.decrypt(nonce, ciphertext) {
        Ok(plaintext) => {
            let value: serde_json::Value = serde_json::from_slice(&plaintext)?;
            Ok(value)
        }
        Err(primary_err) => {
            let legacy_key = [0u8; 32];
            let legacy_cipher = Aes256Gcm::new_from_slice(&legacy_key)
                .map_err(|e| CryptoError::Decrypt(e.to_string()))?;
            match legacy_cipher.decrypt(nonce, ciphertext) {
                Ok(plaintext) => {
                    tracing::warn!(
                        "decrypted data source config with the legacy zero-key; \
                         please re-save this data source so it is re-encrypted with \
                         the current key at {}/.encryption_key",
                        data_dir.display()
                    );
                    let value: serde_json::Value = serde_json::from_slice(&plaintext)?;
                    Ok(value)
                }
                Err(_) => Err(CryptoError::Decrypt(primary_err.to_string())),
            }
        }
    }
}

// ── Row → Info helper ─────────────────────────────────────────────────────────

fn row_to_info(
    data_dir: &std::path::Path,
    row: &sqlx::sqlite::SqliteRow,
    include_plain_config: bool,
) -> DataSourceInfo {
    let config_json: serde_json::Value = row.get("config");
    let id: String = row.get("id");
    // Decrypt first so we have the plaintext config keys (host, port, etc.).
    // A corrupt row is logged but not allowed to abort the whole listing:
    // the preview is substituted with an explicit marker so operators can
    // spot the broken datasource in the UI and re-key / re-save it.
    let decrypted = match decrypt_config(&config_json, data_dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                data_source_id = %id,
                error = %e,
                "row_to_info: decrypt failed; returning masked preview"
            );
            serde_json::json!({ "_corrupt": "[unable to decrypt — re-save this data source]" })
        }
    };
    let config_preview = redact_sensitive_config(&decrypted);
    let config_plain = if include_plain_config {
        Some(decrypted)
    } else {
        None
    };
    DataSourceInfo {
        id,
        tenant_id: row.get("tenant_id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        description: row.get("description"),
        db_type: row.get("db_type"),
        visibility: row.get("visibility"),
        config_preview,
        config_plain,
        schema_info: row.get::<Option<serde_json::Value>, _>("schema_info"),
        enabled: row.get("enabled"),
        last_tested_at: row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_tested_at")
            .map(|dt| dt.to_rfc3339()),
        last_error: row.get("last_error"),
        created_by: row.get("created_by"),
        created_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .to_rfc3339(),
        updated_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            .to_rfc3339(),
        sensitive_columns: row.get::<Option<serde_json::Value>, _>("sensitive_columns"),
        embedding_status: row.get("embedding_status"),
    }
}

// ── Route handlers ────────────────────────────────────────────────────────────

/// GET /api/v1/data-sources — list visible data sources for current user.
async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<DataSourceListResponse>> {
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    let offset = params.offset();
    let limit = params.limit();

    let rows = sqlx::query(
        "SELECT id, tenant_id, user_id, name, description, db_type, visibility, config, \
         schema_info, enabled, last_tested_at, last_error, created_by, created_at, updated_at, \
         sensitive_columns, embedding_status \
         FROM data_sources \
         WHERE tenant_id = ? AND (user_id IS NULL OR user_id = ? OR ?) \
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(is_admin)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM data_sources \
         WHERE tenant_id = ? AND (user_id IS NULL OR user_id = ? OR ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(is_admin)
    .fetch_one(&state.db)
    .await?;

    let data_sources: Vec<DataSourceInfo> = rows
        .into_iter()
        .map(|row| row_to_info(&state.data_dir, &row, false))
        .collect();
    let total = usize::try_from(total.0).unwrap_or(0);

    Ok(Json(DataSourceListResponse {
        data_sources,
        total,
    }))
}

/// POST /api/v1/data-sources — create a new data source.
async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateDataSourceRequest>,
) -> Result<Json<DataSourceInfo>> {
    crate::routes::nl2sql::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;

    let visibility = req.visibility.as_deref().unwrap_or("private");
    if req.name.trim().is_empty() {
        return Err(AppError::ValidationError("name is required".into()));
    }

    // Types accepted by the public API. `trino` is an alias for `presto`.
    // HTTP-API and MCP-backed "data sources" were never wired end-to-end
    // (no execute, no schema discovery), so we no longer accept them in
    // this release. Legacy rows that pre-date this change remain
    // readable — the WebUI simply hides the action buttons for them.
    let valid_types = [
        "mysql",
        "tidb",
        "postgres",
        "clickhouse",
        "presto",
        "trino",
        "mongodb",
    ];
    if !valid_types.contains(&req.db_type.as_str()) {
        return Err(AppError::ValidationError(format!(
            "db_type must be one of: {}",
            valid_types.join(", ")
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let user_id: Option<String> = if visibility == "private" {
        Some(claims.sub.clone())
    } else {
        None
    };
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.is_none() && !is_admin {
        return Err(AppError::Forbidden);
    }

    // Keep this preflight for a clear conflict response; the SQLite baseline
    // also enforces the scoped name uniqueness.
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM data_sources \
         WHERE tenant_id = ? AND name = ? \
           AND user_id IS ? \
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&req.name)
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!(
            "a data source named '{}' already exists in this {}",
            req.name,
            if user_id.is_some() {
                "workspace"
            } else {
                "tenant"
            }
        )));
    }

    // Check for duplicate config (same host+port+database for SQL types).
    let sql_types = ["mysql", "tidb", "postgres", "clickhouse"];
    if sql_types.contains(&req.db_type.as_str()) {
        if let Ok(cfg) = serde_json::from_value::<SqlConfig>(req.config.clone()) {
            if let Some(conflict_name) = check_duplicate_config(
                &state.db,
                &state.data_dir,
                &claims.tenant_id,
                &req.db_type,
                &cfg.host,
                cfg.port,
                &cfg.database,
                "",
            )
            .await
            {
                return Err(AppError::Conflict(format!(
                    "a data source with the same host/port/database already exists: '{conflict_name}'"
                )));
            }
        }
    }

    let probe = probe_connection(
        &req.db_type,
        &req.config,
        Some((&claims.tenant_id, &claims.sub)),
    )
    .await?;
    if !probe.success {
        return Err(AppError::ValidationError(format!(
            "data source connection failed: {}",
            probe
                .error
                .unwrap_or_else(|| "unknown connection error".to_string())
        )));
    }

    let encrypted_config = serde_json::to_value(encrypt_config(&req.config, &state.data_dir)?)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let sensitive_columns_json = req
        .sensitive_columns
        .as_ref()
        .map(|cols| serde_json::to_string(cols).map_err(|e| AppError::Internal(e.to_string())))
        .transpose()?;

    // When no description is provided, generate one from the schema to give
    // the routing engine meaningful text for vector similarity matching.
    let description = req.description.clone().filter(|d| !d.trim().is_empty());
    let resolved_description = if description.is_some() {
        description.clone()
    } else {
        let schema_arr = req.schema_info.as_ref().and_then(|s| {
            s.as_array()
                .or_else(|| s.get("tables").and_then(|t| t.as_array()))
        });
        schema_arr.and_then(|arr| {
            let table_names: Vec<&str> = arr
                .iter()
                .filter_map(|t| t.get("table_name").and_then(|n| n.as_str()))
                .take(10)
                .collect();
            if table_names.is_empty() {
                None
            } else {
                Some(format_datasource_description(&req.db_type, &table_names))
            }
        })
    };

    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO data_sources \
         (id, tenant_id, user_id, name, description, db_type, visibility, config, schema_info, sensitive_columns, created_by, last_tested_at, last_error) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, NULL)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&user_id)
    .bind(&req.name)
    .bind(&resolved_description)
    .bind(&req.db_type)
    .bind(visibility)
    .bind(&encrypted_config)
    .bind(&req.schema_info)
    .bind(&sensitive_columns_json)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    let seed_embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    // Seed the AI-generated description into nl2sql_datasource_semantics so
    // it is available for routing immediately, without waiting for a refresh.
    if let Some(ref desc) = resolved_description {
        if description.is_none() {
            // Only seed the auto-generated description when the user didn't
            // provide one; user descriptions take priority in the semantics layer.
            if let Err(e) = sqlx::query(
                "INSERT OR IGNORE INTO nl2sql_datasource_semantics \
                 (tenant_id, datasource_id, ai_description, embedding_model) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&claims.tenant_id)
            .bind(&id)
            .bind(desc)
            .bind(&seed_embedding_model)
            .execute(&state.db)
            .await
            {
                tracing::warn!(
                    data_source_id = %id,
                    error = %e,
                    "failed to seed auto-generated datasource description"
                );
            }
        }
    }

    // Seed an empty `nl2sql_datasource_semantics` row so users can edit
    // the datasource-level description in the Semantics drawer even before
    // the first schema refresh has produced an AI description. Without
    // this, PATCH /semantics/:id/datasource would 404 on a brand-new ds.
    // We use INSERT IGNORE rather than a plain INSERT to make the seed
    // idempotent on re-create-after-delete — cheap belt-and-braces.
    if let Err(e) = sqlx::query(
        "INSERT OR IGNORE INTO nl2sql_datasource_semantics \
         (tenant_id, datasource_id, embedding_model) \
         VALUES (?, ?, ?)",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(&seed_embedding_model)
    .execute(&state.db)
    .await
    {
        tracing::warn!(
            data_source_id = %id,
            error = %e,
            "failed to seed nl2sql_datasource_semantics row"
        );
    }

    let row = sqlx::query(
        "SELECT id, tenant_id, user_id, name, description, db_type, visibility, config, \
         schema_info, enabled, last_tested_at, last_error, created_by, created_at, updated_at, \
         sensitive_columns, embedding_status \
         FROM data_sources WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(row_to_info(&state.data_dir, &row, false)))
}

/// GET /api/v1/data-sources/:id — get a single data source.
async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<DataSourceInfo>> {
    let row = sqlx::query(
        "SELECT id, tenant_id, user_id, name, description, db_type, visibility, config, \
         schema_info, enabled, last_tested_at, last_error, created_by, created_at, updated_at, \
         sensitive_columns, embedding_status \
         FROM data_sources WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => {
            let tenant_id: String = r.get("tenant_id");
            let user_id: Option<String> = r.get("user_id");
            let is_admin = claims.role == "admin" || claims.role == "superadmin";
            if tenant_id != claims.tenant_id {
                return Err(AppError::Forbidden);
            }
            if user_id.as_ref() != Some(&claims.sub) && !is_admin {
                return Err(AppError::Forbidden);
            }
            Ok(Json(row_to_info(&state.data_dir, &r, true)))
        }
        None => Err(AppError::NotFound("data source not found".into())),
    }
}

/// PATCH /api/v1/data-sources/:id — update a data source.
async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DataSourceInfo>> {
    // Never log the raw body — it may contain plaintext passwords / tokens.
    // We log the sanitised request struct below, after the `{"data": ...}`
    // wrapper has been unwrapped.
    let raw: serde_json::Value = serde_json::from_slice(&body)?;
    let req: UpdateDataSourceRequest = match raw.get("data") {
        Some(inner) => serde_json::from_value(inner.clone())?,
        None => serde_json::from_value(raw)?,
    };
    tracing::debug!(
        "data_sources::update: id={}, name={:?}, description={:?}, config_present={}, visibility={:?}, enabled={:?}",
        id,
        req.name,
        req.description,
        req.config.is_some(),
        req.visibility,
        req.enabled,
    );
    let row = sqlx::query("SELECT tenant_id, user_id, visibility FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;

    let (tenant_id, user_id, current_visibility): (String, Option<String>, String) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id"), r.get("visibility")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }
    if let Some(next_visibility) = req.visibility.as_deref() {
        if next_visibility != current_visibility {
            validate_knowledge_bindings_after_visibility_change(
                &state.db,
                &claims.tenant_id,
                &id,
                next_visibility,
            )
            .await?;
        }
    }

    let mut all_updates: Vec<&'static str> = Vec::new();
    let mut bindings: Vec<SqlBindValue> = Vec::new();

    if let Some(name) = req.name {
        all_updates.push("name = ?");
        bindings.push(SqlBindValue::String(name));
    }
    if let Some(desc) = req.description {
        all_updates.push("description = ?");
        bindings.push(SqlBindValue::String(desc));
    }
    if let Some(cfg) = req.config {
        let config_row: (serde_json::Value,) = sqlx::query_as::<_, (serde_json::Value,)>(
            "SELECT config FROM data_sources WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
        let decrypted = decrypt_config(&config_row.0, &state.data_dir)?;
        let mut merged = decrypted.as_object().cloned().unwrap_or_default();
        if let Some(new_cfg) = cfg.as_object() {
            // Merge policy (explicit-null-is-delete, mirroring JSON Merge
            // Patch RFC 7396 for top-level keys):
            //   - A `null` value removes the key from the stored config.
            //   - A non-null value replaces the key.
            //   - Keys not present in `new_cfg` are preserved unchanged.
            // Config is a flat key-value map in current data-source types,
            // so deep-merge semantics are intentionally NOT supported.
            for (k, v) in new_cfg {
                if v.is_null() {
                    merged.remove(k);
                } else {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
        let encrypted = encrypt_config(&serde_json::json!(merged), &state.data_dir)?;
        // Check for duplicate config (same host+port+database for SQL types).
        let sql_types = ["mysql", "tidb", "postgres", "clickhouse"];
        let db_type_row: Option<(String,)> =
            sqlx::query_as("SELECT db_type FROM data_sources WHERE id = ?")
                .bind(&id)
                .fetch_optional(&state.db)
                .await?;
        if let Some((db_type,)) = db_type_row {
            if sql_types.contains(&db_type.as_str()) {
                if let Ok(cfg) =
                    serde_json::from_value::<SqlConfig>(serde_json::json!(merged.clone()))
                {
                    if let Some(conflict_name) = check_duplicate_config(
                        &state.db,
                        &state.data_dir,
                        &claims.tenant_id,
                        &db_type,
                        &cfg.host,
                        cfg.port,
                        &cfg.database,
                        &id,
                    )
                    .await
                    {
                        return Err(AppError::Conflict(format!(
                            "a data source with the same host/port/database already exists: '{conflict_name}'"
                        )));
                    }
                }
            }
            let probe = probe_connection(
                &db_type,
                &serde_json::json!(merged.clone()),
                Some((&claims.tenant_id, &claims.sub)),
            )
            .await?;
            if !probe.success {
                return Err(AppError::ValidationError(format!(
                    "data source connection failed: {}",
                    probe
                        .error
                        .unwrap_or_else(|| "unknown connection error".to_string())
                )));
            }
        }
        all_updates.push("config = ?");
        bindings.push(SqlBindValue::Json(encrypted));
        all_updates.push("last_tested_at = CURRENT_TIMESTAMP");
        all_updates.push("last_error = NULL");
    }
    if let Some(schema) = req.schema_info {
        all_updates.push("schema_info = ?");
        bindings.push(SqlBindValue::Json(schema));
    }
    if let Some(enabled) = req.enabled {
        all_updates.push("enabled = ?");
        let val = if enabled { "1" } else { "0" };
        bindings.push(SqlBindValue::String(val.to_string()));
    }
    if let Some(sensitive_columns) = req.sensitive_columns {
        all_updates.push("sensitive_columns = ?");
        bindings.push(SqlBindValue::Json(
            serde_json::to_value(sensitive_columns).unwrap_or(serde_json::Value::Null),
        ));
    }
    if let Some(visibility) = req.visibility {
        let is_admin = claims.role == "admin" || claims.role == "superadmin";
        if visibility != "private" && visibility != "tenant" {
            return Err(AppError::ValidationError(
                "visibility must be 'private' or 'tenant'".into(),
            ));
        }
        if visibility == "tenant" && !is_admin {
            return Err(AppError::Forbidden);
        }
        all_updates.push("visibility = ?");
        bindings.push(SqlBindValue::String(visibility.clone()));
        all_updates.push("user_id = ?");
        if visibility == "tenant" {
            bindings.push(SqlBindValue::Null);
        } else {
            bindings.push(SqlBindValue::String(claims.sub.clone()));
        }
    }

    if !all_updates.is_empty() {
        let query = format!(
            "UPDATE data_sources SET {} WHERE id = ?",
            all_updates.join(", ")
        );
        tracing::debug!("data_sources::update: executing: {}", query,);
        let mut q = sqlx::query(&query);
        for b in &bindings {
            match b {
                SqlBindValue::String(s) => q = q.bind(s.as_str()),
                SqlBindValue::Json(j) => q = q.bind(j),
                SqlBindValue::Null => q = q.bind(Option::<String>::None),
            }
        }
        q.bind(&id).execute(&state.db).await?;
        tracing::debug!("data_sources::update: SQL executed, affected rows updated");

        // Any config-affecting change should drop the cached pool so the
        // next NL2SQL execute picks up the new credentials/host. The
        // pool-cache key includes `updated_at`, which ON UPDATE bumps
        // automatically — but eagerly invalidating frees memory sooner
        // and forces credential-rotation correctness even if the clock
        // skewed.
        state
            .nl2sql_pool_cache
            .invalidate_datasource(&claims.tenant_id, &id);
    }

    let row = sqlx::query(
        "SELECT id, tenant_id, user_id, name, description, db_type, visibility, config, \
         schema_info, enabled, last_tested_at, last_error, created_by, created_at, updated_at, \
         sensitive_columns, embedding_status \
         FROM data_sources WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(row_to_info(&state.data_dir, &row, false)))
}

/// DELETE /api/v1/data-sources/:id — delete a data source.
async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;

    let (tenant_id, user_id): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    // Run the platform-database cascade in a transaction so a failure halfway
    // through doesn't leave us with a deleted data_sources row and
    // orphaned semantic records.
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "DELETE FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM nl2sql_table_desc_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM nl2sql_datasource_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM nl2sql_refresh_tasks WHERE datasource_id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let deleted_reference_pack_ids =
        cleanup_sql_knowledge_for_deleted_datasource(&mut tx, &tenant_id, &id).await?;
    sqlx::query("DELETE FROM data_sources WHERE id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    // Drop any cached downstream pool so we don't keep credentials resident
    // for a deleted datasource. Safe to call unconditionally.
    state
        .nl2sql_pool_cache
        .invalidate_datasource(&claims.tenant_id, &id);

    // The separate embedding store lives outside the platform SQLite transaction:
    // if the purge fails we only leak vectors, which is recoverable because the
    // platform database is the source of truth for which datasources still exist.
    if let Some(store) = state.nl2sql_embedding_store.as_ref() {
        if let Err(e) = store.delete_datasource(&tenant_id, &id) {
            tracing::warn!(
                datasource_id = %id,
                error = %e,
                "failed to purge embedding store; platform database row already deleted"
            );
        }
    }
    for pack_id in deleted_reference_pack_ids {
        let _ =
            tokio::fs::remove_dir_all(sql_knowledge_pack_dir(&state, &tenant_id, &pack_id)).await;
    }

    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

async fn cleanup_sql_knowledge_for_deleted_datasource(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tenant_id: &str,
    datasource_id: &str,
) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT id, datasource_id, scope, CAST(datasource_bindings_json AS TEXT) AS datasource_bindings_json \
         FROM nl2sql_reference_packs \
         WHERE tenant_id = ? \
           AND (datasource_id = ? \
                OR EXISTS (SELECT 1 FROM json_each(COALESCE(datasource_bindings_json, JSON_ARRAY())) WHERE json_each.value = ?))",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .fetch_all(&mut **tx)
    .await?;

    let mut deleted_pack_ids = Vec::new();
    for row in rows {
        let pack_id: String = row.get("id");
        let primary_datasource_id: String = row.get("datasource_id");
        let scope: String = row.get("scope");
        let bindings_json: Option<String> = row.get("datasource_bindings_json");
        let mut bindings = parse_datasource_bindings(bindings_json.as_deref());
        if bindings.is_empty() && primary_datasource_id != "global" {
            bindings.push(primary_datasource_id.clone());
        }
        let remaining = normalize_datasource_bindings(
            bindings
                .into_iter()
                .filter(|binding| binding != datasource_id),
        );

        if primary_datasource_id == datasource_id && remaining.is_empty() {
            sqlx::query(
                "DELETE FROM sql_knowledge_usage_events WHERE tenant_id = ? AND pack_id = ?",
            )
            .bind(tenant_id)
            .bind(&pack_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "DELETE FROM nl2sql_query_reference_usages WHERE tenant_id = ? AND pack_id = ?",
            )
            .bind(tenant_id)
            .bind(&pack_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query("DELETE FROM nl2sql_reference_chunks WHERE tenant_id = ? AND pack_id = ?")
                .bind(tenant_id)
                .bind(&pack_id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("DELETE FROM nl2sql_reference_files WHERE tenant_id = ? AND pack_id = ?")
                .bind(tenant_id)
                .bind(&pack_id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("DELETE FROM nl2sql_reference_packs WHERE tenant_id = ? AND id = ?")
                .bind(tenant_id)
                .bind(&pack_id)
                .execute(&mut **tx)
                .await?;
            deleted_pack_ids.push(pack_id);
            continue;
        }

        let next_primary = if primary_datasource_id == datasource_id {
            remaining
                .first()
                .cloned()
                .unwrap_or_else(|| "global".to_string())
        } else {
            primary_datasource_id.clone()
        };
        let next_bindings =
            if remaining.is_empty() && next_primary != "global" && scope.as_str() != "tenant" {
                vec![next_primary.clone()]
            } else {
                remaining
            };
        let next_bindings_json = serde_json::to_value(&next_bindings)?;

        sqlx::query(
            "UPDATE nl2sql_reference_packs \
             SET datasource_id = ?, datasource_bindings_json = ?, stale = 1 \
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(&next_primary)
        .bind(next_bindings_json)
        .bind(tenant_id)
        .bind(&pack_id)
        .execute(&mut **tx)
        .await?;

        if primary_datasource_id == datasource_id {
            sqlx::query(
                "UPDATE nl2sql_reference_files SET datasource_id = ? \
                 WHERE tenant_id = ? AND pack_id = ? AND datasource_id = ?",
            )
            .bind(&next_primary)
            .bind(tenant_id)
            .bind(&pack_id)
            .bind(datasource_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE nl2sql_reference_chunks SET datasource_id = ? \
                 WHERE tenant_id = ? AND pack_id = ? AND datasource_id = ?",
            )
            .bind(&next_primary)
            .bind(tenant_id)
            .bind(&pack_id)
            .bind(datasource_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE sql_knowledge_usage_events SET datasource_id = ? \
                 WHERE tenant_id = ? AND pack_id = ? AND datasource_id = ?",
            )
            .bind(&next_primary)
            .bind(tenant_id)
            .bind(&pack_id)
            .bind(datasource_id)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "UPDATE nl2sql_query_reference_usages SET datasource_id = ? \
                 WHERE tenant_id = ? AND pack_id = ? AND datasource_id = ?",
            )
            .bind(&next_primary)
            .bind(tenant_id)
            .bind(&pack_id)
            .bind(datasource_id)
            .execute(&mut **tx)
            .await?;
        }
    }

    sqlx::query("DELETE FROM sql_knowledge_usage_events WHERE tenant_id = ? AND datasource_id = ?")
        .bind(tenant_id)
        .bind(datasource_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "DELETE FROM nl2sql_query_reference_usages WHERE tenant_id = ? AND datasource_id = ?",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .execute(&mut **tx)
    .await?;

    Ok(deleted_pack_ids)
}

fn parse_datasource_bindings(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .map(normalize_datasource_bindings)
        .unwrap_or_default()
}

fn normalize_datasource_bindings<I>(bindings: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for binding in bindings {
        let trimmed = binding.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

fn sql_knowledge_pack_dir(state: &AppState, tenant_id: &str, pack_id: &str) -> PathBuf {
    state
        .data_dir
        .join(".aos")
        .join("nl2sql-reference")
        .join(safe_path_segment(tenant_id))
        .join(safe_path_segment(pack_id))
}

fn safe_path_segment(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// POST /api/v1/data-sources/:id/test — test connection to a data source.
async fn test_connection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<TestConnectionResponse>> {
    let row =
        sqlx::query("SELECT tenant_id, user_id, db_type, config FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await?;

    let (tenant_id, user_id, db_type, config_json): (
        String,
        Option<String>,
        String,
        serde_json::Value,
    ) = match row {
        Some(r) => (
            r.get("tenant_id"),
            r.get("user_id"),
            r.get("db_type"),
            r.get("config"),
        ),
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let config = decrypt_config(&config_json, &state.data_dir)?;
    let response =
        probe_connection(&db_type, &config, Some((&claims.tenant_id, &claims.sub))).await?;
    if response.success {
        sqlx::query(
            "UPDATE data_sources SET last_tested_at = CURRENT_TIMESTAMP, last_error = NULL WHERE id = ?",
        )
        .bind(&id)
        .execute(&state.db)
        .await?;
    } else {
        let error = response
            .error
            .as_deref()
            .unwrap_or("unknown connection error");
        tracing::warn!(data_source_id = %id, db_type = %db_type, error, "data source connection test failed");
        sqlx::query(
            "UPDATE data_sources SET last_tested_at = CURRENT_TIMESTAMP, last_error = ? WHERE id = ?",
        )
        .bind(error)
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    Ok(Json(response))
}

fn probe_result(start: std::time::Instant, error: Option<String>) -> TestConnectionResponse {
    TestConnectionResponse {
        success: error.is_none(),
        latency_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        error,
        schema_preview: None,
    }
}

async fn probe_connection(
    db_type: &str,
    config_json: &serde_json::Value,
    trino_owner: Option<(&str, &str)>,
) -> Result<TestConnectionResponse> {
    if matches!(db_type, "presto" | "trino") && trino_owner.is_some() {
        // The Trino branch owns query-id-aware timeout and cancellation. An
        // outer timeout could discard that future while cancellation is still
        // in progress and release the user's concurrency slot too early.
        return probe_connection_inner(db_type, config_json, trino_owner).await;
    }
    let started_at = std::time::Instant::now();
    match tokio::time::timeout(
        std::time::Duration::from_secs(12),
        probe_connection_inner(db_type, config_json, trino_owner),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Ok(probe_result(
            started_at,
            Some("connection timed out after 12 seconds".into()),
        )),
    }
}

async fn probe_connection_inner(
    db_type: &str,
    config_json: &serde_json::Value,
    trino_owner: Option<(&str, &str)>,
) -> Result<TestConnectionResponse> {
    let start = std::time::Instant::now();
    match db_type {
        "mysql" | "tidb" => {
            let config: SqlConfig =
                serde_json::from_value(config_json.clone())
                    .map_err(|_| AppError::ValidationError("invalid config".into()))?;
            let url = build_mysql_url(&config);
            let result = sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(&url)
                .await;
            match result {
                Ok(pool) => {
                    pool.close().await;
                    Ok(probe_result(start, None))
                }
                Err(error) => Ok(probe_result(start, Some(error.to_string()))),
            }
        }
        "postgres" => {
            let config: SqlConfig =
                serde_json::from_value(config_json.clone())
                    .map_err(|_| AppError::ValidationError("invalid config".into()))?;
            let url = build_postgres_url(&config);
            let result = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(&url)
                .await;
            match result {
                Ok(pool) => {
                    pool.close().await;
                    Ok(probe_result(start, None))
                }
                Err(error) => Ok(probe_result(start, Some(error.to_string()))),
            }
        }
        "clickhouse" => {
            let cfg: ClickHouseConfig =
                serde_json::from_value(config_json.clone())
                    .map_err(|_| AppError::ValidationError("invalid clickhouse config".into()))?;
            let addr = format!("http://{}:{}", cfg.host, cfg.port);
            let client = clickhouse::Client::default()
                .with_url(&addr)
                .with_user(&cfg.username)
                .with_password(&cfg.password)
                .with_database(&cfg.database);

            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client.query("SELECT 1").execute(),
            )
            .await
            {
                Ok(Ok(_)) => Ok(probe_result(start, None)),
                Ok(Err(error)) => Ok(probe_result(start, Some(error.to_string()))),
                Err(_) => Ok(probe_result(start, Some("connection timed out after 10 seconds".into()))),
            }
        }
        "presto" | "trino" => {
            let cfg: TrinoConfig =
                serde_json::from_value(config_json.clone())
                    .map_err(|_| AppError::ValidationError("invalid trino/presto config".into()))?;
            let normalized_host = normalize_host_input(&cfg.host);
            let port = normalized_host.port.unwrap_or(cfg.port);
            let secure = cfg
                .ssl
                .or(normalized_host.secure)
                .unwrap_or(port == 443);
            let schemas = cfg.effective_schemas();
            let schema = schemas.first().map(String::as_str).unwrap_or("default");
            let mut builder = trino_rust_client::ClientBuilder::new(&cfg.username, &normalized_host.host)
                .port(port)
                .catalog(&cfg.catalog)
                .schema(schema)
                .secure(secure);
            if cfg.basic_auth.unwrap_or(!cfg.password.is_empty()) {
                builder = builder.auth(trino_rust_client::auth::Auth::Basic(
                    cfg.username.clone(),
                    Some(cfg.password.clone()),
                ));
            }
            let cli = builder
                .max_attempt(0)
                .build()
                .map_err(|e| AppError::ValidationError(format!("invalid trino connection configuration: {e}")))?;

            if let Some((tenant_id, user_id)) = trino_owner {
                match crate::routes::nl2sql::agent_executor::execute_trino_query_bounded(
                    cli,
                    "SELECT 1".to_string(),
                    10,
                    tenant_id,
                    user_id,
                    "Trino connection probe",
                    crate::routes::nl2sql::agent_executor::DatasourceRequestBudget::new(1),
                )
                .await
                {
                    Ok(_) => Ok(probe_result(start, None)),
                    Err(error) => Ok(probe_result(start, Some(error.to_string()))),
                }
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    cli.get_all::<trino_rust_client::Row>("SELECT 1".to_string()),
                )
                .await
                {
                    Ok(Ok(_)) => Ok(probe_result(start, None)),
                    Ok(Err(error)) => Ok(probe_result(start, Some(error.to_string()))),
                    Err(_) => Ok(probe_result(start, Some("connection timed out after 10 seconds".into()))),
                }
            }
        }
        "mongodb" => {
            let cfg: MongoConfig = serde_json::from_value(config_json.clone())
                .map_err(|error| AppError::ValidationError(format!("invalid MongoDB config: {error}")))?;
            if cfg.database.trim().is_empty() {
                return Err(AppError::ValidationError(
                    "MongoDB database is required".into(),
                ));
            }
            let uri = build_mongodb_uri(&cfg).map_err(AppError::ValidationError)?;
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let mut options = mongodb::options::ClientOptions::parse(&uri).await?;
                options.server_selection_timeout = Some(std::time::Duration::from_secs(10));
                options.connect_timeout = Some(std::time::Duration::from_secs(10));
                let client = mongodb::Client::with_options(options)?;
                client
                    .database(cfg.database.trim())
                    .run_command(mongodb::bson::doc! { "ping": 1 })
                    .await?;
                Ok::<(), mongodb::error::Error>(())
            })
            .await;
            match result {
                Ok(Ok(())) => Ok(probe_result(start, None)),
                Ok(Err(error)) => Ok(probe_result(start, Some(error.to_string()))),
                Err(_) => Ok(probe_result(
                    start,
                    Some("connection timed out after 10 seconds".into()),
                )),
            }
        }
        "http_api" | "mcp" | "hive" => Err(AppError::ValidationError(format!(
            "unsupported db_type for test: {db_type} (support was removed — legacy rows remain readable but can no longer be tested)",
        ))),
        _ => Err(AppError::ValidationError(format!("unsupported db_type for test: {db_type}"))),
    }
}

async fn discover_trino_schemas_for_config(
    Extension(claims): Extension<Claims>,
    Json(req): Json<DiscoverTrinoSchemasRequest>,
) -> Result<Json<DiscoverTrinoSchemasResponse>> {
    let normalized_host = normalize_host_input(&req.host);
    let host = normalized_host.host.trim().to_string();
    let catalog = req.catalog.trim().to_string();
    let username = req.username.trim().to_string();
    if host.is_empty() {
        return Err(AppError::ValidationError("host is required".into()));
    }
    if catalog.is_empty() {
        return Err(AppError::ValidationError("catalog is required".into()));
    }
    if username.is_empty() {
        return Err(AppError::ValidationError("username is required".into()));
    }

    let port = normalized_host.port.or(req.port).unwrap_or(443);
    let secure = req.ssl.or(normalized_host.secure).unwrap_or(port == 443);
    let basic_auth = req.basic_auth.unwrap_or(true);
    let _trino_permit = crate::routes::nl2sql::agent_executor::acquire_trino_user_permit(
        &claims.tenant_id,
        &claims.sub,
    )
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    let result = crate::nl2sql::schema_discovery::discover_trino_schemas(
        &host,
        port,
        &catalog,
        &username,
        Some(&req.password),
        secure,
        basic_auth,
    )
    .await
    .map_err(AppError::ValidationError)?;

    Ok(Json(DiscoverTrinoSchemasResponse {
        catalog,
        schemas: result.schemas,
        method: result.method,
        warnings: result.warnings,
    }))
}

/// POST /api/v1/data-sources/:id/discover — auto-discover schema for SQL sources.
///
/// Holds a per-datasource refresh lock so an inflight periodic schedule cycle
/// cannot race with this request and clobber `schema_info`. Manual tables
/// (`is_manual: true` in the stored JSON) are preserved untouched — the live
/// introspection pass never sees them, and we merge them back in before
/// writing.
///
/// When the tenant has an embedding model configured AND the resulting schema
/// actually changed, we kick off an async semantic re-index task in the
/// background (non-blocking) and surface its `task_id` to the caller. The
/// frontend uses this to poll progress via `GET /nl2sql/semantics-tasks/:id`
/// rather than leaving users wondering why NL2SQL results haven't improved
/// after clicking "Discover".
#[allow(clippy::too_many_lines)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscoverRefreshMode {
    Incremental,
    Force,
}

#[derive(Debug, Deserialize)]
struct DiscoverSchemaRequest {
    #[serde(default)]
    mode: Option<DiscoverRefreshMode>,
}

struct InflightRefreshTask {
    task_id: String,
    status: String,
}

async fn fail_stale_refresh_tasks(db: &sqlx::SqlitePool, datasource_id: &str) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE nl2sql_refresh_tasks \
         SET status = 'failed', \
             error_message = 'task timed out (no progress for 30 minutes)', \
             completed_at = CURRENT_TIMESTAMP \
         WHERE datasource_id = ? \
           AND status IN ('pending', 'running') \
           AND updated_at < datetime(CURRENT_TIMESTAMP, '-30 minutes')",
    )
    .bind(datasource_id)
    .execute(db)
    .await?;
    if result.rows_affected() > 0 {
        tracing::warn!(
            datasource_id,
            count = result.rows_affected(),
            "marked stale NL2SQL refresh tasks as failed before schema discovery"
        );
    }
    Ok(result.rows_affected())
}

async fn load_inflight_refresh_task(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Result<Option<InflightRefreshTask>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT task_id, status \
         FROM nl2sql_refresh_tasks \
         WHERE datasource_id = ? AND status IN ('pending', 'running') \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(datasource_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(task_id, status)| InflightRefreshTask { task_id, status }))
}

async fn discover_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<DiscoverSchemaRequest>,
) -> Result<Json<serde_json::Value>> {
    let discover_started_at = std::time::Instant::now();
    let row = sqlx::query(
        "SELECT tenant_id, user_id, db_type, config, name FROM data_sources WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;

    let (tenant_id, user_id, db_type, config_json, _ds_name): (
        String,
        Option<String>,
        String,
        serde_json::Value,
        String,
    ) = match row {
        Some(r) => (
            r.get("tenant_id"),
            r.get("user_id"),
            r.get("db_type"),
            r.get("config"),
            r.get("name"),
        ),
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    if ![
        "mysql",
        "tidb",
        "postgres",
        "clickhouse",
        "presto",
        "trino",
        "mongodb",
    ]
    .contains(&db_type.as_str())
    {
        return Err(AppError::ValidationError(
            "schema discovery is not supported for this data source type".into(),
        ));
    }

    let force_refresh = matches!(req.mode, Some(DiscoverRefreshMode::Force));

    // Hold the per-datasource advisory lock so the periodic scheduler
    // can't race with us. If another refresh is already running we
    // surface 409 — retrying in a couple of seconds is far better than
    // silently overwriting half-written state.
    fail_stale_refresh_tasks(&state.db, &id).await?;
    let _lock = match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&state.db, &id).await {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            let in_flight = load_inflight_refresh_task(&state.db, &id).await?;
            let detail = in_flight
                .map(|task| format!(" task_id={}, status={}", task.task_id, task.status))
                .unwrap_or_default();
            return Err(AppError::Conflict(format!(
                    "another refresh is already in progress for this data source; try again shortly{detail}"
                )));
        }
        Err(e) => {
            return Err(AppError::Internal(format!(
                "failed to acquire refresh lock: {e}"
            )))
        }
    };

    let decrypted = decrypt_config(&config_json, &state.data_dir)?;
    let discovery_started_at = std::time::Instant::now();
    let outcome = match db_type.as_str() {
        "mysql" | "tidb" => {
            let cfg: SqlConfig = serde_json::from_value(decrypted)
                .map_err(|_| AppError::ValidationError("invalid config".into()))?;
            crate::nl2sql::schema_discovery::discover_mysql(
                &cfg.host,
                cfg.port,
                &cfg.database,
                &cfg.username,
                &cfg.password,
            )
            .await
        }
        "postgres" => {
            let cfg: SqlConfig = serde_json::from_value(decrypted)
                .map_err(|_| AppError::ValidationError("invalid config".into()))?;
            crate::nl2sql::schema_discovery::discover_postgres(
                &cfg.host,
                cfg.port,
                &cfg.database,
                &cfg.username,
                &cfg.password,
            )
            .await
        }
        "clickhouse" => {
            let cfg: ClickHouseConfig = serde_json::from_value(decrypted)
                .map_err(|_| AppError::ValidationError("invalid clickhouse config".into()))?;
            crate::nl2sql::schema_discovery::discover_clickhouse(
                &cfg.host,
                cfg.port,
                &cfg.database,
                &cfg.username,
                &cfg.password,
            )
            .await
        }
        "presto" | "trino" => {
            let cfg: TrinoConfig = serde_json::from_value(decrypted)
                .map_err(|_| AppError::ValidationError("invalid trino/presto config".into()))?;
            let schemas = cfg.effective_schemas();
            let _trino_permit = crate::routes::nl2sql::agent_executor::acquire_trino_user_permit(
                &claims.tenant_id,
                &claims.sub,
            )
            .await
            .map_err(|error| AppError::Internal(error.to_string()))?;
            crate::nl2sql::schema_discovery::discover_trino_multi(
                &cfg.host,
                cfg.port,
                &cfg.catalog,
                &schemas,
                &cfg.username,
                Some(&cfg.password),
                cfg.ssl.unwrap_or(cfg.port == 443),
                cfg.basic_auth.unwrap_or(!cfg.password.is_empty()),
            )
            .await
        }
        "mongodb" => {
            let cfg: MongoConfig = serde_json::from_value(decrypted)
                .map_err(|_| AppError::ValidationError("invalid MongoDB config".into()))?;
            crate::nl2sql::schema_discovery::discover_mongodb(&cfg).await
        }
        _ => unreachable!("db_type already validated above"),
    }
    .map_err(|e| map_schema_discovery_error(&db_type, e))?;
    tracing::info!(
        datasource_id = %id,
        db_type = %db_type,
        elapsed_ms = discovery_started_at.elapsed().as_millis() as u64,
        tables = outcome.tables.len(),
        columns = outcome.total_columns(),
        skipped = outcome.skipped.len(),
        "discover_schema: source introspection finished"
    );

    if outcome.tables.is_empty() && outcome.skipped.is_empty() {
        return Err(AppError::ValidationError(format!(
            "未发现任何表。请检查数据源配置中的 catalog/schema 是否正确，以及当前账号是否有读取表结构权限。当前类型：{db_type}"
        )));
    }

    tracing::info!(
        datasource_id = %id,
        db_type = %db_type,
        tables = outcome.tables.len(),
        columns = outcome.total_columns(),
        skipped = outcome.skipped.len(),
        cap_hit = outcome.cap_hit,
        "discover_schema completed"
    );

    // Merge manual tables back in so user-managed entries survive.
    let existing_schema: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
    let manual_tables = crate::nl2sql::schema_diff::extract_manual_tables(&existing_schema);
    let mut schemas = outcome.tables.clone();
    schemas.extend(manual_tables);

    // Build enriched schema_info: tables + foreign_keys at root level.
    let enriched_schema = serde_json::json!({
        "tables": schemas,
        "foreign_keys": outcome.foreign_keys,
    });

    // Detect whether this discovery changed schema and capture changed table set.
    // Both sides must use the same JSON shape: {"tables": [...], "foreign_keys": [...]}.
    let schema_diff = crate::nl2sql::schema_diff::diff_schemas(
        &existing_schema,
        &serde_json::json!({ "tables": schemas, "foreign_keys": Vec::<()>::new() }),
    );
    let schema_changed = force_refresh || !schema_diff.is_empty();

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(enriched_schema.clone())
        .bind(&id)
        .execute(&state.db)
        .await?;
    tracing::info!(
        datasource_id = %id,
        elapsed_ms = discover_started_at.elapsed().as_millis() as u64,
        schema_changed,
        force_refresh,
        "discover_schema: schema persisted"
    );

    // Release the lock before triggering the async refresh so its own
    // worker can acquire it.
    drop(_lock);

    // Incremental mode: only patch changed/incomplete tables.
    // Force mode: always rewrite + re-embed the whole datasource.
    let refresh_tables = if force_refresh {
        None
    } else {
        let tables = detect_incremental_refresh_tables(
            &state.db,
            state.nl2sql_embedding_store.as_ref(),
            &claims.tenant_id,
            &id,
            &schemas,
            &schema_diff,
        )
        .await?;
        if tables.is_empty() {
            None
        } else {
            Some(tables)
        }
    };

    let needs_embedding_refresh = force_refresh || refresh_tables.is_some();

    let mut task_id: Option<String> = None;
    if needs_embedding_refresh {
        match maybe_trigger_semantics_refresh(
            &state,
            &claims.tenant_id,
            &id,
            Some(&schemas),
            refresh_tables.clone(),
            force_refresh,
        )
        .await
        {
            Ok(Some(tid)) => task_id = Some(tid),
            Ok(None) => {} // no embedding model / already in flight
            Err(e) => {
                tracing::warn!(
                    datasource_id = %id,
                    error = %e,
                    "failed to auto-trigger semantic refresh; user must refresh manually"
                );
            }
        }
    }
    tracing::info!(
        datasource_id = %id,
        schema_changed,
        force_refresh,
        needs_embedding_refresh,
        refresh_tables = ?refresh_tables,
        refresh_task_id = ?task_id,
        elapsed_ms = discover_started_at.elapsed().as_millis() as u64,
        "discover_schema: refresh decision made"
    );

    Ok(Json(serde_json::json!({
        "schemas": schemas,
        "skipped_tables": outcome.skipped.iter().map(|(name, err)| {
            serde_json::json!({ "table": name, "error": err })
        }).collect::<Vec<_>>(),
        "cap_hit": outcome.cap_hit,
        "schema_changed": schema_changed,
        "force_refresh": force_refresh,
        "needs_embedding_refresh": needs_embedding_refresh,
        "refresh_task_id": task_id,
        "foreign_keys": outcome.foreign_keys.iter().map(|fk| {
            serde_json::json!({
                "source_table": fk.source_table,
                "source_column": fk.source_column,
                "target_table": fk.target_table,
                "target_column": fk.target_column,
            })
        }).collect::<Vec<_>>(),
    })))
    .map(|resp| {
        tracing::info!(
            datasource_id = %id,
            total_elapsed_ms = discover_started_at.elapsed().as_millis() as u64,
            "discover_schema: response returned"
        );
        resp
    })
}

/// Discover and re-index a single table's schema and semantic embeddings.
/// Used by the "refresh this table" action in the schema drawer.
///
/// Flow:
/// 1. Pull the live schema for the named table from the source DB
/// 2. Compare with the current stored schema for that table
/// 3. If unchanged → return immediately with `refresh_task_id: null`
/// 4. If changed → update schema_info, trigger single-table semantic refresh,
///    return the new `refresh_task_id`
async fn discover_table_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    let row =
        sqlx::query("SELECT tenant_id, user_id, db_type, config FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await?;

    let (tenant_id, user_id, db_type, config_json): (
        String,
        Option<String>,
        String,
        serde_json::Value,
    ) = match row {
        Some(r) => (
            r.get("tenant_id"),
            r.get("user_id"),
            r.get("db_type"),
            r.get("config"),
        ),
        None => return Err(AppError::NotFound("data source not found".into())),
    };

    if tenant_id != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if user_id.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    fail_stale_refresh_tasks(&state.db, &id).await?;
    let _lock = match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&state.db, &id).await {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            let in_flight = load_inflight_refresh_task(&state.db, &id).await?;
            let detail = in_flight
                .map(|task| format!(" task_id={}, status={}", task.task_id, task.status))
                .unwrap_or_default();
            return Err(AppError::Conflict(format!(
                    "another refresh is already in progress for this data source; try again shortly{detail}"
                )));
        }
        Err(e) => {
            return Err(AppError::Internal(format!(
                "failed to acquire refresh lock: {e}"
            )))
        }
    };

    let decrypted = decrypt_config(&config_json, &state.data_dir)?;

    let _trino_permit = if matches!(db_type.as_str(), "presto" | "trino") {
        Some(
            crate::routes::nl2sql::agent_executor::acquire_trino_user_permit(
                &claims.tenant_id,
                &claims.sub,
            )
            .await
            .map_err(|error| AppError::Internal(error.to_string()))?,
        )
    } else {
        None
    };
    let live_table = crate::nl2sql::schema_discovery::SchemaDiscovery::new()
        .discover_table(&db_type, &decrypted, &table_name)
        .await
        .map_err(|e| map_schema_discovery_error(&db_type, e))?;

    let live_table = match live_table {
        Some(t) => t,
        None => {
            return Err(AppError::NotFound(format!(
                "table `{table_name}` not found in the source database"
            )));
        }
    };

    // Load current schema_info
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    // Extract the stored entry for this table (handles both {tables:[...]} and flat array)
    let stored_tables = crate::nl2sql::schema_diff::extract_schema_tables(&stored);
    let stored_entry: Option<&serde_json::Value> = stored_tables
        .iter()
        .find(|t| t.get("table_name").and_then(|v| v.as_str()) == Some(&table_name));

    // Normalise both sides for comparison (sort columns, strip non-structural fields)
    let live_norm = normalise_table_for_diff(&live_table);
    let stored_norm = stored_entry.map(normalise_table_for_diff);

    let schema_changed = match &stored_norm {
        Some(s) if s == &live_norm => false,
        _ => true,
    };

    // Determine whether a semantic refresh (embedding re-index) is needed:
    //   - schema changed → refresh always needed
    //   - schema unchanged but this table has no vectors in the embedding store → refresh needed
    //   - nothing changed → no refresh
    let local_indexed_keys = if let Some(registry) = state.nl2sql_embedding_store.as_ref() {
        match crate::nl2sql::embedding_profiles::resolve_profiles(
            &state.db,
            &claims.tenant_id,
            Some("nl2sql"),
        )
        .await
        {
            Ok(profiles) => registry
                .profile_store(
                    &claims.tenant_id,
                    &profiles.local.id,
                    &profiles.local.config.model,
                    profiles.local.config.base_url.clone(),
                )
                .and_then(|store| store.indexed_keys(&id))
                .unwrap_or_default(),
            Err(error) => {
                tracing::warn!(datasource_id = %id, error = %error, "failed to resolve local embedding profile");
                std::collections::HashSet::new()
            }
        }
    } else {
        std::collections::HashSet::new()
    };
    let needs_refresh = if schema_changed {
        true
    } else if state.nl2sql_embedding_store.is_some() {
        let live_columns: std::collections::HashSet<String> = live_table
            .get("columns")
            .and_then(|columns| columns.as_array())
            .map(|columns| {
                columns
                    .iter()
                    .filter_map(|column| column.get("name")?.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let indexed_cols: std::collections::HashSet<String> = local_indexed_keys
            .iter()
            .filter(|(table, _, embed_type)| table == &table_name && embed_type == "col")
            .map(|(_, column, _)| column.clone())
            .collect();
        indexed_cols != live_columns
    } else {
        false
    };

    let mut refresh_task_id: Option<String> = None;

    if schema_changed {
        // Merge the updated table back into schema_info, preserving all others
        let mut new_tables: Vec<serde_json::Value> = stored_tables
            .into_iter()
            .filter(|t| t.get("table_name").and_then(|v| v.as_str()) != Some(&table_name))
            .collect();
        new_tables.push(live_table);

        let foreign_keys = stored
            .get("foreign_keys")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let enriched = serde_json::json!({
            "tables": new_tables,
            "foreign_keys": foreign_keys,
        });

        sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
            .bind(&enriched)
            .bind(&id)
            .execute(&state.db)
            .await?;

        // Trigger single-table semantic refresh, passing the updated schema so the
        // worker doesn't need to re-read it from DB.
        match trigger_single_table_semantics(
            &state,
            &claims.tenant_id,
            &id,
            &table_name,
            &db_type,
            &config_json,
            Some(new_tables.clone()),
        )
        .await
        {
            Ok(Some(tid)) => refresh_task_id = Some(tid),
            Ok(None) => {} // no embedding model / already in flight
            Err(e) => {
                tracing::warn!(datasource_id = %id, table = %table_name, error = %e,
                    "failed to trigger semantic refresh for single table; user can retry manually");
            }
        }
    } else if needs_refresh {
        // Schema unchanged but vectors are missing — still trigger refresh.
        // Pass the current stored schema so the worker has it.
        // If the table is absent from schema_info (e.g. it was just discovered but
        // stored_schema was empty), merge it in first so the worker can find it.
        let schema_json: Vec<serde_json::Value> = stored_tables.iter().cloned().collect();
        let schema_json = if stored_tables.iter().any(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        }) {
            schema_json
        } else {
            let mut updated = schema_json;
            updated.push(live_table.clone());
            updated
        };
        match trigger_single_table_semantics(
            &state,
            &claims.tenant_id,
            &id,
            &table_name,
            &db_type,
            &config_json,
            Some(schema_json),
        )
        .await
        {
            Ok(Some(tid)) => refresh_task_id = Some(tid),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(datasource_id = %id, table = %table_name, error = %e,
                    "failed to trigger semantic refresh for single table; user can retry manually");
            }
        }
    }

    drop(_lock);

    Ok(Json(serde_json::json!({
        "table_name": table_name,
        "schema_changed": schema_changed,
        "refresh_task_id": refresh_task_id,
    })))
}

/// Normalise a table entry for diffing: sort columns by name and strip
/// non-structural metadata so minor presentation differences don't trigger
/// a re-index.
fn normalise_table_for_diff(table: &serde_json::Value) -> serde_json::Value {
    let columns = table
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|arr| {
            let mut cols: Vec<serde_json::Value> = arr
                .iter()
                .filter_map(|col| {
                    let name = col.get("name")?.as_str()?;
                    let col_type = col.get("type")?.as_str()?;
                    let nullable = col.get("nullable")?.as_bool()?;
                    Some(serde_json::json!({
                        "name": name,
                        "type": col_type,
                        "nullable": nullable,
                    }))
                })
                .collect();
            cols.sort_by(|a, b| {
                a.get("name")
                    .and_then(|v| v.as_str())
                    .cmp(&b.get("name").and_then(|v| v.as_str()))
            });
            cols
        })
        .unwrap_or_default();

    serde_json::json!({
        "table_name": table.get("table_name").and_then(|v| v.as_str()).unwrap_or(""),
        "columns": columns,
    })
}

/// Compute incremental refresh targets:
/// - schema-added/changed tables
/// - tables with missing AI rewrite (table/column descriptions)
/// - tables with missing embeddings (column/table/datasource coverage)
///
/// Returns table names only. Datasource-only gaps are mapped to a single table
/// refresh so we can repair datasource-level embedding/description without
/// reindexing the whole datasource.
async fn detect_incremental_refresh_tables(
    db: &sqlx::SqlitePool,
    embed_store: Option<&Arc<crate::nl2sql::embedding::EmbeddingStoreRegistry>>,
    tenant_id: &str,
    datasource_id: &str,
    schemas: &[serde_json::Value],
    schema_diff: &crate::nl2sql::schema_diff::SchemaDiff,
) -> Result<Vec<String>> {
    let mut live_cols: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for table in schemas {
        if table
            .get("is_manual")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(table_name) = table.get("table_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut cols = std::collections::HashSet::new();
        if let Some(arr) = table.get("columns").and_then(|v| v.as_array()) {
            for col in arr {
                if let Some(name) = col.get("name").and_then(|v| v.as_str()) {
                    cols.insert(name.to_string());
                }
            }
        }
        live_cols.insert(table_name.to_string(), cols);
    }

    let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    pending.extend(schema_diff.added.iter().cloned());
    pending.extend(schema_diff.changed.iter().cloned());

    let col_rows: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT table_name, column_name, COALESCE(semantic_description, '') AS semantic_description, is_indexed \
         FROM nl2sql_table_semantics \
         WHERE datasource_id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .bind(tenant_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut col_sem_map: std::collections::HashMap<(String, String), (bool, bool)> =
        std::collections::HashMap::new();
    for (table_name, column_name, semantic_description, is_indexed) in col_rows {
        col_sem_map.insert(
            (table_name, column_name),
            (!semantic_description.trim().is_empty(), is_indexed),
        );
    }

    let table_desc_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') AS description \
         FROM nl2sql_table_desc_semantics \
         WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut table_desc_ready: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    for (table_name, description) in table_desc_rows {
        table_desc_ready.insert(table_name, !description.trim().is_empty());
    }

    let ds_desc_ready: bool = sqlx::query_scalar::<_, Option<String>>(
        "SELECT COALESCE(NULLIF(user_description, ''), NULLIF(ai_description, ''), '') \
         FROM nl2sql_datasource_semantics \
         WHERE datasource_id = ? AND deleted_at IS NULL \
         LIMIT 1",
    )
    .bind(datasource_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
    .map(|s| !s.trim().is_empty())
    .unwrap_or(false);

    let indexed_keys = if let Some(registry) = embed_store {
        match crate::nl2sql::embedding_profiles::resolve_profiles(db, tenant_id, Some("nl2sql"))
            .await
        {
            Ok(profiles) => registry
                .profile_store(
                    tenant_id,
                    &profiles.local.id,
                    &profiles.local.config.model,
                    profiles.local.config.base_url.clone(),
                )
                .and_then(|store| store.indexed_keys(datasource_id))
                .unwrap_or_default(),
            Err(_) => std::collections::HashSet::new(),
        }
    } else {
        std::collections::HashSet::new()
    };
    let ds_embedding_ready = indexed_keys
        .iter()
        .any(|(t, c, et)| t == "__datasource__" && c == "__datasource__" && et == "datasource");

    for (table_name, cols) in &live_cols {
        let table_emb_ready = indexed_keys
            .iter()
            .any(|(t, c, et)| t == table_name && c == "__table__" && et == "table");
        if !table_desc_ready.get(table_name).copied().unwrap_or(false) || !table_emb_ready {
            pending.insert(table_name.clone());
        }
        for col_name in cols {
            let (desc_ready, row_index_ready) = col_sem_map
                .get(&(table_name.clone(), col_name.clone()))
                .copied()
                .unwrap_or((false, false));
            let emb_ready =
                indexed_keys.contains(&(table_name.clone(), col_name.clone(), "col".to_string()));
            if !desc_ready || !row_index_ready || !emb_ready {
                pending.insert(table_name.clone());
                break;
            }
        }
    }

    // Datasource-level repair: if only datasource semantics are missing,
    // route one table through incremental refresh so datasource embedding/desc
    // gets recomputed without triggering full refresh.
    if (!ds_desc_ready || !ds_embedding_ready) && pending.is_empty() {
        if let Some(first_table) = live_cols.keys().next() {
            pending.insert(first_table.clone());
        }
    }

    let mut out: Vec<String> = pending.into_iter().collect();
    out.sort();
    Ok(out)
}

/// Kick off a single-table semantic refresh, returning the task_id.
/// Returns Ok(None) if the tenant has no embedding model configured.
async fn trigger_single_table_semantics(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    table_name: &str,
    db_type: &str,
    config_json: &serde_json::Value,
    override_schema: Option<Vec<serde_json::Value>>,
) -> Result<Option<String>> {
    let embed_cfg = crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, None).await;
    if embed_cfg.is_none() {
        return Ok(None);
    }

    fail_stale_refresh_tasks(&state.db, datasource_id).await?;
    let in_flight: Option<String> = sqlx::query_scalar(
        "SELECT task_id FROM nl2sql_refresh_tasks \
         WHERE datasource_id = ? AND status IN ('pending', 'running') LIMIT 1",
    )
    .bind(datasource_id)
    .fetch_optional(&state.db)
    .await?;
    if in_flight.is_some() {
        return Ok(None);
    }

    let embed_store = match state.nl2sql_embedding_store.as_ref() {
        Some(s) => s.clone(),
        None => return Ok(None),
    };
    let ann_registry = Arc::clone(&embed_store);

    let chat_cfg = match state.config_registry.as_ref() {
        Some(registry) => crate::nl2sql::resolve_chat_config(
            registry,
            tenant_id,
            tenant_id,
            &state.default_model,
            None,
        )
        .await
        .map_err(AppError::Internal)?,
        None => return Ok(None),
    };

    let task_id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks \
         (task_id, tenant_id, datasource_id, trigger_source, status, total_tables) \
         VALUES (?, ?, ?, 'user', 'pending', 1)",
    )
    .bind(&task_id)
    .bind(tenant_id)
    .bind(datasource_id)
    .execute(&state.db)
    .await?;

    let tenant_id = tenant_id.to_owned();
    let datasource_id = datasource_id.to_owned();
    let task_id_inner = task_id.clone();
    let table_name_owned = table_name.to_owned();
    let db_type_owned = db_type.to_owned();
    let config_json_owned = config_json.clone();
    let db = state.db.clone();
    let usage_writer = state.usage_writer.clone();
    let override_schema_owned = override_schema;

    tokio::spawn(async move {
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let _lock = loop {
            match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&db, &datasource_id).await {
                Ok(Some(guard)) => break guard,
                Ok(None) => {
                    if std::time::Instant::now() >= lock_deadline {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = 'timed out waiting for another refresh (60s)' \
                             WHERE task_id = ?",
                        )
                        .bind(&task_id_inner)
                        .execute(&db)
                        .await;
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    let _ = sqlx::query(
                        "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                         error_message = ? WHERE task_id = ?",
                    )
                    .bind(format!("failed to acquire refresh lock: {e}"))
                    .bind(&task_id_inner)
                    .execute(&db)
                    .await;
                    return;
                }
            }
        };

        let _ = sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'running' WHERE task_id = ?")
            .bind(&task_id_inner)
            .execute(&db)
            .await;

        let embed_cfg = crate::nl2sql::resolve_embedding_config(&db, &tenant_id, None).await;
        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            embed_store,
            embed_cfg,
            Some(chat_cfg),
        );

        #[derive(Clone)]
        struct SingleReporter {
            task_id: String,
            db: sqlx::SqlitePool,
        }
        #[async_trait::async_trait]
        impl crate::nl2sql::schema_describer::ProgressReporter for SingleReporter {
            async fn report(&self, percent: u32, processed_tables: u32) {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET progress = ?, processed_tables = ?, updated_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(percent)
                .bind(processed_tables)
                .bind(&self.task_id)
                .execute(&self.db)
                .await;
            }
        }

        let reporter = SingleReporter {
            task_id: task_id_inner.clone(),
            db: db.clone(),
        };

        let result = if let Some(schema_json) = override_schema_owned {
            let tables: Vec<crate::nl2sql::schema_describer::TableSchema> = schema_json
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            tracing::info!(ds_id = %datasource_id, total_parsed = tables.len(), "override_schema parsed from json");
            // Filter to only the requested table
            let filtered: Vec<_> = tables
                .into_iter()
                .filter(|t| t.table_name == table_name_owned)
                .collect();
            tracing::info!(ds_id = %datasource_id, table_name = %table_name_owned, filtered_count = filtered.len(), "table filter result");
            if filtered.is_empty() {
                tracing::warn!(ds_id = %datasource_id, table_name = %table_name_owned,
                    "override_schema contained no entry for this table — trying direct discovery from source DB");
                match describer
                    .discover_single_table_schema(
                        &db_type_owned,
                        &config_json_owned,
                        &table_name_owned,
                    )
                    .await
                {
                    Ok(Some(discovered)) => {
                        describer
                            .refresh_schema_directly(
                                &tenant_id,
                                &datasource_id,
                                vec![discovered],
                                reporter.clone(),
                            )
                            .await
                    }
                    Ok(None) => {
                        tracing::error!(ds_id = %datasource_id, table_name = %table_name_owned,
                            "table not found in source DB");
                        use crate::nl2sql::schema_describer::ProgressReporter as _;
                        reporter.report(100, 0).await;
                        Ok(crate::nl2sql::schema_describer::RefreshResult {
                            tables_processed: 0,
                            columns_processed: 0,
                            failed_tables: vec![(
                                table_name_owned,
                                "table not found in source database".to_owned(),
                            )],
                            embedding_usage: vec![],
                        })
                    }
                    Err(e) => {
                        tracing::error!(ds_id = %datasource_id, table_name = %table_name_owned,
                            error = %e, "direct discovery failed");
                        Err(e)
                    }
                }
            } else {
                describer
                    .refresh_schema_directly(&tenant_id, &datasource_id, filtered, reporter)
                    .await
            }
        } else {
            tracing::info!(ds_id = %datasource_id, "no override_schema, falling back to refresh_tables");
            describer
                .refresh_tables(&tenant_id, &datasource_id, &[table_name_owned], reporter)
                .await
        };

        let (status, processed, _error_message) = match result {
            Ok(r) => {
                let ann_registry_for_snapshot = Arc::clone(&ann_registry);
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    ann_registry_for_snapshot.persist_ann_snapshots_if_dirty()
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
                {
                    tracing::warn!(datasource_id = %datasource_id, error, "ANN snapshot refresh failed after table indexing; brute-force retrieval remains available");
                }
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant_id,
                    &tenant_id,
                    &format!("datasource:discover:{datasource_id}"),
                    Some(&task_id_inner),
                    &embed_model_for_usage,
                    embed_api_key_for_usage.clone(),
                    aggregate_embedding_usage(&r.embedding_usage),
                )
                .await;
                let _failed = if r.failed_tables.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(r.failed_tables.iter().map(|(name, err)| {
                        serde_json::json!({ "table": name, "error": err })
                    }).collect::<Vec<_>>()).unwrap_or_default())
                };
                (
                    "completed".to_owned(),
                    r.tables_processed as u32,
                    None::<String>,
                )
            }
            Err(e) => ("failed".to_owned(), 0u32, Some(e.to_string())),
        };

        let _ = sqlx::query(
            "UPDATE nl2sql_refresh_tasks \
             SET status = ?, progress = 100, processed_tables = ?, completed_at = CURRENT_TIMESTAMP \
             WHERE task_id = ?",
        )
        .bind(&status)
        .bind(processed)
        .bind(&task_id_inner)
        .execute(&db)
        .await;
    });

    Ok(Some(task_id))
}

/// Kick off an async semantic refresh if the tenant has an embedding
/// model configured and no refresh is currently in flight for this
/// datasource. Returns the new task_id on success.
///
/// Delegates to the same platform SQLite task table that the user-facing
/// `POST /nl2sql/semantics/:id/refresh-async` endpoint uses, so progress
/// is visible through the existing poll endpoint and the Semantics
/// Drawer UI.
async fn maybe_trigger_semantics_refresh(
    state: &AppState,
    tenant_id: &str,
    datasource_id: &str,
    // When provided, the worker uses this instead of reading schema_info from DB,
    // avoiding a race where the discover handler's UPDATE hasn't committed yet.
    override_schema: Option<&[serde_json::Value]>,
    only_tables: Option<Vec<String>>,
    force_full_rewrite: bool,
) -> Result<Option<String>> {
    // Skip if the tenant doesn't have an embedding model — a refresh
    // without vectors can't improve routing, so there's no point.
    let embed_cfg = crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, None).await;
    if embed_cfg.is_none() {
        return Ok(None);
    }

    // Skip if a refresh is already queued or running. This keeps the
    // discover-then-refresh UX idempotent if the user double-clicks.
    fail_stale_refresh_tasks(&state.db, datasource_id).await?;
    let in_flight: Option<String> = sqlx::query_scalar(
        "SELECT task_id FROM nl2sql_refresh_tasks \
         WHERE datasource_id = ? AND status IN ('pending', 'running') \
         LIMIT 1",
    )
    .bind(datasource_id)
    .fetch_optional(&state.db)
    .await?;
    if in_flight.is_some() {
        return Ok(None);
    }

    let embed_store = match state.nl2sql_embedding_store.as_ref() {
        Some(s) => s.clone(),
        None => return Ok(None),
    };
    let ann_registry = Arc::clone(&embed_store);
    let chat_cfg = match state.config_registry.as_ref() {
        Some(registry) => crate::nl2sql::resolve_chat_config(
            registry,
            tenant_id,
            tenant_id,
            &state.default_model,
            None,
        )
        .await
        .map_err(AppError::Internal)?,
        None => return Ok(None),
    };

    let task_id = uuid::Uuid::new_v4().to_string();

    // Count non-manual tables for the progress bar. We re-read schema_info
    // here instead of trusting the caller's copy because the UPDATE may
    // have happened between decisions.
    let total_tables: i32 = if let Some(ref list) = only_tables {
        i32::try_from(list.len()).unwrap_or(i32::MAX)
    } else {
        let schema_info: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT schema_info FROM data_sources WHERE id = ?")
                .bind(datasource_id)
                .fetch_optional(&state.db)
                .await?
                .flatten();
        let tables = schema_info
            .as_ref()
            .map(crate::nl2sql::schema_diff::extract_schema_tables)
            .unwrap_or_default();
        i32::try_from(
            tables
                .iter()
                .filter(|t| {
                    !t.get("is_manual")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .count(),
        )
        .unwrap_or(i32::MAX)
    };

    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks \
         (task_id, tenant_id, trigger_source, datasource_id, status, total_tables) \
         VALUES (?, ?, 'discover', ?, 'pending', ?)",
    )
    .bind(&task_id)
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(total_tables)
    .execute(&state.db)
    .await?;

    let db = state.db.clone();
    let usage_writer = state.usage_writer.clone();
    let tenant = tenant_id.to_owned();
    let ds_id = datasource_id.to_owned();
    let task_id_clone = task_id.clone();
    let override_schema_owned = override_schema.map(|s| s.to_vec());
    let only_tables_owned = only_tables.clone();
    let force_full_rewrite_owned = force_full_rewrite;

    tokio::spawn(async move {
        // Acquire the refresh lock. It was released before this task
        // was spawned (the discover handler drops its own guard), but
        // the periodic scheduler could have snatched it. Wait up to
        // 60s before giving up.
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let _lock = loop {
            match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&db, &ds_id).await {
                Ok(Some(guard)) => break guard,
                Ok(None) => {
                    if std::time::Instant::now() >= lock_deadline {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = 'timed out waiting for another refresh to finish (60s); try again later' \
                             WHERE task_id = ?",
                        )
                        .bind(&task_id_clone)
                        .execute(&db)
                        .await;
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    let _ = sqlx::query(
                        "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                         error_message = ? WHERE task_id = ?",
                    )
                    .bind(format!("failed to acquire refresh lock: {e}"))
                    .bind(&task_id_clone)
                    .execute(&db)
                    .await;
                    return;
                }
            }
        };

        let _ = sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'running' WHERE task_id = ?")
            .bind(&task_id_clone)
            .execute(&db)
            .await;

        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            embed_store,
            embed_cfg,
            Some(chat_cfg),
        );

        struct DbProgress {
            db: sqlx::SqlitePool,
            task_id: String,
        }
        #[async_trait::async_trait]
        impl crate::nl2sql::schema_describer::ProgressReporter for DbProgress {
            async fn report(&self, percent: u32, processed_tables: u32) {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET progress = ?, processed_tables = ?, updated_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(percent)
                .bind(processed_tables)
                .bind(&self.task_id)
                .execute(&self.db)
                .await;
            }
        }

        let reporter = DbProgress {
            db: db.clone(),
            task_id: task_id_clone.clone(),
        };

        // Full refresh mode means "rewrite + re-embed everything" without
        // checking existing rewrite/index state. We achieve that by clearing
        // column-level AI semantics/index flags before running the refresh
        // pipeline, so all columns are regenerated.
        if force_full_rewrite_owned && only_tables_owned.is_none() {
            if let Err(e) = sqlx::query(
                "UPDATE nl2sql_table_semantics \
                 SET semantic_description = '', embedding_model = '', is_indexed = 0 \
                 WHERE datasource_id = ? AND deleted_at IS NULL",
            )
            .bind(&ds_id)
            .execute(&db)
            .await
            {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(format!("failed to prepare full rewrite state: {e}"))
                .bind(&task_id_clone)
                .execute(&db)
                .await;
                return;
            }
        }

        let result = if let Some(schema_json) = override_schema_owned {
            let tables: Vec<crate::nl2sql::schema_describer::TableSchema> = schema_json
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            let tables = if let Some(ref only) = only_tables_owned {
                let set: std::collections::HashSet<&str> =
                    only.iter().map(std::string::String::as_str).collect();
                tables
                    .into_iter()
                    .filter(|t| set.contains(t.table_name.as_str()))
                    .collect::<Vec<_>>()
            } else {
                tables
            };
            if tables.is_empty() {
                tracing::warn!(ds_id = %ds_id, "override_schema was provided but parsed to 0 tables — falling back to DB read");
                if let Some(ref only) = only_tables_owned {
                    describer
                        .refresh_tables(&tenant, &ds_id, only, reporter)
                        .await
                } else {
                    describer
                        .refresh_datasource_with_progress(&tenant, &ds_id, reporter)
                        .await
                }
            } else {
                describer
                    .refresh_schema_directly(&tenant, &ds_id, tables, reporter)
                    .await
            }
        } else {
            if let Some(ref only) = only_tables_owned {
                describer
                    .refresh_tables(&tenant, &ds_id, only, reporter)
                    .await
            } else {
                describer
                    .refresh_datasource_with_progress(&tenant, &ds_id, reporter)
                    .await
            }
        };

        match result {
            Ok(result) => {
                let ann_registry_for_snapshot = Arc::clone(&ann_registry);
                match tokio::task::spawn_blocking(move || {
                    ann_registry_for_snapshot.persist_ann_snapshots_if_dirty()
                })
                .await
                {
                    Ok(Ok(count)) if count > 0 => {
                        tracing::info!(count, datasource_id = %ds_id, "ANN snapshot rebuilt and loaded after datasource indexing");
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(datasource_id = %ds_id, error = %error, "ANN snapshot refresh failed; brute-force retrieval remains available");
                    }
                    Err(error) => {
                        tracing::warn!(datasource_id = %ds_id, error = %error, "ANN snapshot worker failed; brute-force retrieval remains available");
                    }
                }
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant,
                    &tenant,
                    &format!("datasource:discover:{ds_id}"),
                    Some(&task_id_clone),
                    &embed_model_for_usage,
                    embed_api_key_for_usage.clone(),
                    aggregate_embedding_usage(&result.embedding_usage),
                )
                .await;
                let failed_json = if result.failed_tables.is_empty() {
                    None
                } else {
                    serde_json::to_value(
                        result
                            .failed_tables
                            .iter()
                            .map(|(name, err)| serde_json::json!({ "table": name, "error": err }))
                            .collect::<Vec<_>>(),
                    )
                    .ok()
                };
                let all_failed = result.tables_processed == 0 && !result.failed_tables.is_empty();
                let status = if all_failed { "failed" } else { "completed" };
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = ?, progress = 100, \
                     processed_tables = ?, failed_tables = ?, completed_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(status)
                .bind(i32::try_from(result.tables_processed).unwrap_or(i32::MAX))
                .bind(failed_json)
                .bind(&task_id_clone)
                .execute(&db)
                .await;
            }
            Err(e) => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(e.to_string())
                .bind(&task_id_clone)
                .execute(&db)
                .await;
            }
        }
    });

    Ok(Some(task_id))
}
// ── Router ──────────────────────────────────────────────────────────────────

// ── Manual table/column management ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AddManualTableRequest {
    pub table_name: String,
    pub description: Option<String>,
    pub columns: Vec<ManualColumn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManualColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
    pub description: Option<String>,
    pub nullable: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PutManualTableRequest {
    pub table_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSqlSchemaRequest {
    pub sql: String,
    #[serde(default = "default_import_overwrite")]
    pub overwrite_existing: bool,
}

fn default_import_overwrite() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSqlSchemaTableResult {
    pub table_name: String,
    pub column_count: usize,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSqlSchemaResponse {
    pub success: bool,
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub tables: Vec<ImportSqlSchemaTableResult>,
    pub refresh_task_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedSqlSchemaTable {
    table_name: String,
    description: Option<String>,
    columns: Vec<ManualColumn>,
}

async fn import_sql_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<ImportSqlSchemaRequest>,
) -> Result<Json<ImportSqlSchemaResponse>> {
    if req.sql.trim().is_empty() {
        return Err(AppError::ValidationError(
            "SQL/DDL content is required".into(),
        ));
    }

    let row =
        sqlx::query("SELECT tenant_id, user_id, db_type, config FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await?;
    let (ds_tenant, ds_user, db_type, config_json): (
        String,
        Option<String>,
        String,
        serde_json::Value,
    ) = match row {
        Some(r) => (
            r.get("tenant_id"),
            r.get("user_id"),
            r.get("db_type"),
            r.get("config"),
        ),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let decrypted_config = decrypt_config(&config_json, &state.data_dir).ok();
    let imported_tables = parse_sql_schema_tables(&req.sql);
    if imported_tables.is_empty() {
        return Err(AppError::ValidationError(
            "未从内容中解析到 CREATE TABLE 结构。请粘贴 CREATE TABLE / SHOW CREATE TABLE 输出，或包含列定义的 DDL。".into(),
        ));
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);
    let foreign_keys = schema_json
        .get("foreign_keys")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    let embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    let mut results = Vec::new();
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut changed_tables = Vec::new();

    for parsed in imported_tables {
        let table_entry = build_imported_table_entry(&db_type, decrypted_config.as_ref(), parsed);
        let table_name = table_entry
            .get("table_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if table_name.is_empty() {
            skipped += 1;
            continue;
        }
        let column_count = table_entry
            .get("columns")
            .and_then(|v| v.as_array())
            .map(|cols| cols.len())
            .unwrap_or(0);
        let existing_idx = tables.iter().position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|name| name.eq_ignore_ascii_case(&table_name))
                .unwrap_or(false)
        });

        let status = if let Some(idx) = existing_idx {
            if !req.overwrite_existing {
                skipped += 1;
                "skipped".to_string()
            } else {
                tables[idx] = table_entry.clone();
                updated += 1;
                changed_tables.push(table_name.clone());
                "updated".to_string()
            }
        } else {
            tables.push(table_entry.clone());
            imported += 1;
            changed_tables.push(table_name.clone());
            "imported".to_string()
        };

        if status != "skipped" {
            upsert_manual_schema_semantics(
                &state.db,
                &claims.tenant_id,
                &id,
                &table_name,
                table_entry.get("description").and_then(|v| v.as_str()),
                table_entry
                    .get("columns")
                    .and_then(|v| v.as_array())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                &embedding_model,
            )
            .await?;
        }

        results.push(ImportSqlSchemaTableResult {
            table_name,
            column_count,
            status,
        });
    }

    if imported + updated == 0 {
        return Ok(Json(ImportSqlSchemaResponse {
            success: true,
            imported,
            updated,
            skipped,
            tables: results,
            refresh_task_id: None,
        }));
    }

    let enriched_schema = serde_json::json!({
        "tables": tables,
        "foreign_keys": foreign_keys,
    });
    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(enriched_schema)
        .bind(&id)
        .execute(&state.db)
        .await?;

    let refresh_task_id = match maybe_trigger_semantics_refresh(
        &state,
        &claims.tenant_id,
        &id,
        None,
        Some(changed_tables),
        false,
    )
    .await
    {
        Ok(task_id) => task_id,
        Err(e) => {
            tracing::warn!(
                datasource_id = %id,
                error = %e,
                "failed to trigger semantic refresh after SQL schema import"
            );
            None
        }
    };

    Ok(Json(ImportSqlSchemaResponse {
        success: true,
        imported,
        updated,
        skipped,
        tables: results,
        refresh_task_id,
    }))
}

async fn upsert_manual_schema_semantics(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    table_name: &str,
    description: Option<&str>,
    columns: &[serde_json::Value],
    embedding_model: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM nl2sql_table_semantics \
         WHERE datasource_id = ? AND table_name = ? AND is_manual = 1",
    )
    .bind(datasource_id)
    .bind(table_name)
    .execute(db)
    .await?;

    for col in columns {
        let Some(column_name) = col.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO nl2sql_table_semantics \
             (tenant_id, datasource_id, table_name, column_name, is_manual) \
             VALUES (?, ?, ?, ?, 1) \
             ON CONFLICT DO UPDATE SET is_manual = 1",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(table_name)
        .bind(column_name)
        .execute(db)
        .await?;
    }

    sqlx::query(
        "INSERT INTO nl2sql_table_desc_semantics \
         (tenant_id, datasource_id, table_name, ai_description, user_description, embedding_model, is_manual) \
         VALUES (?, ?, ?, ?, ?, ?, 1) \
         ON CONFLICT DO UPDATE SET user_description = excluded.user_description, is_manual = 1, embedding_model = excluded.embedding_model",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(table_name)
    .bind(serde_json::Value::Null)
    .bind(description)
    .bind(embedding_model)
    .execute(db)
    .await?;

    Ok(())
}

fn build_imported_table_entry(
    db_type: &str,
    decrypted_config: Option<&serde_json::Value>,
    parsed: ParsedSqlSchemaTable,
) -> serde_json::Value {
    let columns = parsed
        .columns
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "type": c.col_type,
                "description": c.description,
                "nullable": c.nullable.unwrap_or(true),
                "is_manual": true,
            })
        })
        .collect::<Vec<_>>();

    if matches!(db_type, "trino" | "presto") {
        if let Some(config) = decrypted_config {
            if let Ok(cfg) = serde_json::from_value::<TrinoConfig>(config.clone()) {
                let (catalog, schema, physical) =
                    qualify_trino_imported_table(&cfg, &parsed.table_name);
                let full_name = format!("{catalog}.{schema}.{physical}");
                return serde_json::json!({
                    "table_name": full_name,
                    "name": physical,
                    "physical_table_name": physical,
                    "catalog": catalog,
                    "schema": schema,
                    "qualified_name": format!("{schema}.{physical}"),
                    "fully_qualified_name": full_name,
                    "description": parsed.description,
                    "is_manual": true,
                    "source": "sql_import",
                    "columns": columns,
                });
            }
        }
    }

    serde_json::json!({
        "table_name": parsed.table_name,
        "description": parsed.description,
        "is_manual": true,
        "source": "sql_import",
        "columns": columns,
    })
}

fn qualify_trino_imported_table(cfg: &TrinoConfig, table_name: &str) -> (String, String, String) {
    let parts = split_qualified_identifier(table_name);
    match parts.as_slice() {
        [catalog, schema, table, ..] => (catalog.clone(), schema.clone(), table.clone()),
        [schema, table] => (cfg.catalog.clone(), schema.clone(), table.clone()),
        [table] => {
            let schema = cfg
                .effective_schemas()
                .into_iter()
                .next()
                .unwrap_or_else(|| "default".to_string());
            (cfg.catalog.clone(), schema, table.clone())
        }
        _ => {
            let schema = cfg
                .effective_schemas()
                .into_iter()
                .next()
                .unwrap_or_else(|| "default".to_string());
            (cfg.catalog.clone(), schema, table_name.trim().to_string())
        }
    }
}

fn parse_sql_schema_tables(sql: &str) -> Vec<ParsedSqlSchemaTable> {
    let cleaned = strip_sql_comments(sql);
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some((table_name, body_start, body_end)) = find_next_create_table(&cleaned, cursor) {
        let body = &cleaned[body_start + 1..body_end];
        let columns = split_top_level_commas(body)
            .into_iter()
            .filter_map(|part| parse_column_definition(&part))
            .collect::<Vec<_>>();
        if !columns.is_empty() {
            out.push(ParsedSqlSchemaTable {
                table_name,
                description: None,
                columns,
            });
        }
        cursor = body_end.saturating_add(1);
    }
    out
}

fn find_next_create_table(sql: &str, cursor: usize) -> Option<(String, usize, usize)> {
    static CREATE_TABLE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = CREATE_TABLE_RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)\bcreate\s+(?:or\s+replace\s+)?(?:external\s+)?table\s+(?:if\s+not\s+exists\s+)?((?:"[^"]+"|`[^`]+`|\[[^\]]+\]|[A-Za-z_][A-Za-z0-9_$]*)(?:\s*\.\s*(?:"[^"]+"|`[^`]+`|\[[^\]]+\]|[A-Za-z_][A-Za-z0-9_$]*))*)\s*\("#,
        )
        .expect("valid CREATE TABLE regex")
    });
    let mat = re.captures(&sql[cursor..])?;
    let table_name = mat
        .get(1)
        .map(|m| normalize_imported_identifier(m.as_str()))?;
    let full_match = mat.get(0)?;
    let open_paren = cursor + full_match.end().saturating_sub(1);
    let close_paren = find_matching_paren(sql, open_paren)?;
    Some((table_name, open_paren, close_paren))
}

fn parse_column_definition(definition: &str) -> Option<ManualColumn> {
    let trimmed = definition.trim();
    if trimmed.is_empty() || is_table_constraint(trimmed) {
        return None;
    }
    let (name, rest) = parse_leading_identifier(trimmed)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let type_end = [
        " comment ",
        " not null",
        " default ",
        " primary key",
        " unique",
        " constraint ",
        " encode ",
    ]
    .iter()
    .filter_map(|kw| find_keyword_top_level(rest, kw))
    .min()
    .unwrap_or(rest.len());
    let col_type = rest[..type_end].trim().trim_end_matches(',').trim();
    if col_type.is_empty() {
        return None;
    }
    let nullable = find_keyword_top_level(rest, " not null").is_none();
    let description = extract_comment_literal(rest);
    Some(ManualColumn {
        name,
        col_type: col_type.to_string(),
        description,
        nullable: Some(nullable),
    })
}

fn is_table_constraint(definition: &str) -> bool {
    let lower = definition.trim_start().to_ascii_lowercase();
    [
        "primary ",
        "unique ",
        "constraint ",
        "foreign ",
        "partitioned ",
        "clustered ",
        "bucketed ",
        "sort ",
        "index ",
        "key ",
        "check ",
        "like ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn parse_leading_identifier(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim_start();
    let mut chars = trimmed.char_indices();
    let (_, first) = chars.next()?;
    if matches!(first, '"' | '`' | '[') {
        let close = if first == '[' { ']' } else { first };
        let mut escaped = false;
        for (idx, ch) in chars {
            if ch == close && !escaped {
                let raw = &trimmed[..=idx];
                let rest = &trimmed[idx + ch.len_utf8()..];
                return Some((normalize_imported_identifier(raw), rest));
            }
            escaped = ch == '\\' && !escaped;
            if ch != '\\' {
                escaped = false;
            }
        }
        return None;
    }

    let mut end = first.len_utf8();
    for (idx, ch) in chars {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    Some((trimmed[..end].to_string(), &trimmed[end..]))
}

fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            out.push(ch);
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            out.push('\n');
            continue;
        }
        if ch == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            out.push(' ');
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn find_matching_paren(sql: &str, open_idx: usize) -> Option<usize> {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let start = chars.iter().position(|(idx, _)| *idx == open_idx)?;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (idx, ch) in chars.into_iter().skip(start) {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(idx);
            }
        }
    }
    None
}

fn split_top_level_commas(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut quote: Option<char> = None;
    for (idx, ch) in input.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '<' => angle_depth += 1,
            '>' => angle_depth -= 1,
            ',' if paren_depth == 0 && angle_depth == 0 => {
                out.push(input[start..idx].to_string());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    if start < input.len() {
        out.push(input[start..].to_string());
    }
    out
}

fn find_keyword_top_level(input: &str, keyword: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let needle = keyword.to_ascii_lowercase();
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut quote: Option<char> = None;
    for (idx, ch) in input.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '<' => angle_depth += 1,
            '>' => angle_depth -= 1,
            _ => {}
        }
        if paren_depth == 0 && angle_depth == 0 && lower[idx..].starts_with(&needle) {
            return Some(idx);
        }
    }
    None
}

fn extract_comment_literal(input: &str) -> Option<String> {
    let idx = find_keyword_top_level(input, " comment ")?;
    let after = input[idx + " comment ".len()..].trim_start();
    let quote = after.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let mut escaped = false;
    let mut value = String::new();
    for ch in after[quote.len_utf8()..].chars() {
        if ch == quote && !escaped {
            return Some(value);
        }
        if escaped {
            value.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            value.push(ch);
        }
    }
    None
}

fn split_qualified_identifier(input: &str) -> Vec<String> {
    normalize_imported_identifier(input)
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn normalize_imported_identifier(input: &str) -> String {
    input
        .split('.')
        .filter_map(|part| {
            let cleaned = part
                .trim()
                .trim_matches('`')
                .trim_matches('"')
                .trim_matches('[')
                .trim_matches(']')
                .trim();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

async fn add_manual_table(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
    Json(req): Json<AddManualTableRequest>,
) -> Result<Json<serde_json::Value>> {
    let embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    // Validate access
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    // Get existing schema_info
    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    // Check for duplicate
    if tables.iter().any(|t| {
        t.get("table_name")
            .and_then(|v| v.as_str())
            .map(|n| n == req.table_name)
            .unwrap_or(false)
    }) {
        return Err(AppError::ValidationError("table already exists".into()));
    }

    // Build new table entry
    let new_table = serde_json::json!({
        "table_name": req.table_name,
        "description": req.description,
        "is_manual": true,
        "columns": req.columns.iter().map(|c| serde_json::json!({
            "name": c.name,
            "type": c.col_type,
            "description": c.description,
            "nullable": c.nullable.unwrap_or(true),
            "is_manual": true,
        })).collect::<Vec<_>>(),
    });

    tables.push(new_table);

    // Update data_sources
    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    // Mark in nl2sql_table_semantics as manual (columns)
    for col in &req.columns {
        sqlx::query(
            "INSERT INTO nl2sql_table_semantics \
             (tenant_id, datasource_id, table_name, column_name, is_manual) \
             VALUES (?, ?, ?, ?, 1) \
             ON CONFLICT DO UPDATE SET is_manual = 1",
        )
        .bind(&claims.tenant_id)
        .bind(&id)
        .bind(&req.table_name)
        .bind(&col.name)
        .execute(&state.db)
        .await?;
    }

    // Mark in nl2sql_table_desc_semantics as manual (table-level description)
    sqlx::query(
        "INSERT INTO nl2sql_table_desc_semantics \
         (tenant_id, datasource_id, table_name, ai_description, user_description, embedding_model, is_manual) \
         VALUES (?, ?, ?, ?, ?, ?, 1) \
         ON CONFLICT DO UPDATE SET is_manual = 1, embedding_model = excluded.embedding_model",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(&req.table_name)
    .bind(serde_json::Value::Null)
    .bind(&req.description)
    .bind(&embedding_model)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "success": true, "table_name": req.table_name }),
    ))
}

async fn put_manual_table(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name)): Path<(String, String)>,
    Json(req): Json<PutManualTableRequest>,
) -> Result<Json<serde_json::Value>> {
    let embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    // Validate access
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    let idx = tables
        .iter()
        .position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("table not found".into()))?;

    if let Some(new_name) = req.table_name {
        tables[idx]["table_name"] = serde_json::json!(new_name);
        // Rename and re-stamp is_manual in a single UPDATE. Previously
        // we relied on the existing is_manual = 1 predicate, which
        // meant if the flag had ever been cleared by a buggy refresh,
        // this rename would silently no-op. Explicitly re-asserting
        // the flag makes the manual-table invariant self-healing.
        sqlx::query(
            "UPDATE nl2sql_table_semantics SET table_name = ?, is_manual = 1 \
             WHERE datasource_id = ? AND table_name = ?",
        )
        .bind(&new_name)
        .bind(&id)
        .bind(&table_name)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "UPDATE nl2sql_table_desc_semantics SET table_name = ?, is_manual = 1 \
             WHERE datasource_id = ? AND table_name = ?",
        )
        .bind(&new_name)
        .bind(&id)
        .bind(&table_name)
        .execute(&state.db)
        .await?;
    }

    if let Some(desc) = req.description {
        tables[idx]["description"] = serde_json::json!(desc);
        // Update nl2sql_table_desc_semantics (table-level user description)
        // and make sure is_manual stays true.
        sqlx::query(
            "UPDATE nl2sql_table_desc_semantics SET user_description = ?, embedding_model = ?, is_manual = 1 \
             WHERE datasource_id = ? AND table_name = ?",
        )
        .bind(&desc)
        .bind(&embedding_model)
        .bind(&id)
        .bind(&table_name)
        .execute(&state.db)
        .await?;
    }

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn delete_manual_table(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    // Authorization check
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    let idx = tables
        .iter()
        .position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("table not found".into()))?;

    tables.remove(idx);

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    // Clean up nl2sql_table_semantics for manual table (column-level)
    sqlx::query(
        "DELETE FROM nl2sql_table_semantics \
         WHERE datasource_id = ? AND table_name = ? AND is_manual = 1 AND deleted_at IS NULL",
    )
    .bind(&id)
    .bind(&table_name)
    .execute(&state.db)
    .await?;

    // Clean up nl2sql_table_desc_semantics for manual table (table-level)
    sqlx::query(
        "DELETE FROM nl2sql_table_desc_semantics \
         WHERE datasource_id = ? AND table_name = ? AND is_manual = 1 AND deleted_at IS NULL",
    )
    .bind(&id)
    .bind(&table_name)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Debug, Deserialize)]
pub struct AddManualColumnRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub col_type: String,
    pub description: Option<String>,
    pub nullable: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PutManualColumnRequest {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub col_type: Option<String>,
    pub description: Option<String>,
    pub nullable: Option<bool>,
}

async fn add_manual_column(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name)): Path<(String, String)>,
    Json(req): Json<AddManualColumnRequest>,
) -> Result<Json<serde_json::Value>> {
    // Authorization check
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    let idx = tables
        .iter()
        .position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("table not found".into()))?;

    let columns = tables[idx]
        .get_mut("columns")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| AppError::ValidationError("invalid schema".into()))?;

    // Check duplicate
    if columns.iter().any(|c| {
        c.get("name")
            .and_then(|v| v.as_str())
            .map(|n| n == req.name)
            .unwrap_or(false)
    }) {
        return Err(AppError::ValidationError("column already exists".into()));
    }

    let new_col = serde_json::json!({
        "name": req.name,
        "type": req.col_type,
        "description": req.description,
        "nullable": req.nullable.unwrap_or(true),
        "is_manual": true,
    });

    columns.push(new_col);

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    // Mark in nl2sql_table_semantics
    sqlx::query(
        "INSERT INTO nl2sql_table_semantics \
         (tenant_id, datasource_id, table_name, column_name, is_manual) \
         VALUES (?, ?, ?, ?, 1) \
         ON CONFLICT DO UPDATE SET is_manual = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&id)
    .bind(&table_name)
    .bind(&req.name)
    .execute(&state.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "success": true, "column_name": req.name }),
    ))
}

async fn put_manual_column(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name, column_name)): Path<(String, String, String)>,
    Json(req): Json<PutManualColumnRequest>,
) -> Result<Json<serde_json::Value>> {
    // Authorization check
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    let idx = tables
        .iter()
        .position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("table not found".into()))?;

    let columns = tables[idx]
        .get_mut("columns")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| AppError::ValidationError("invalid schema".into()))?;

    let col_idx = columns
        .iter()
        .position(|c| {
            c.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n == column_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("column not found".into()))?;

    if let Some(n) = req.name {
        columns[col_idx]["name"] = serde_json::json!(n);
        // Rename + re-stamp the manual flag in one go. Users occasionally
        // edit a column description via PATCH and then PATCH the name
        // separately; previously the rename path did not set is_manual=1,
        // which meant a subsequent scheduler refresh could match the
        // renamed row against auto-discovered data and overwrite it.
        sqlx::query(
            "UPDATE nl2sql_table_semantics \
             SET column_name = ?, is_manual = 1 \
             WHERE datasource_id = ? AND table_name = ? AND column_name = ?",
        )
        .bind(&n)
        .bind(&id)
        .bind(&table_name)
        .bind(&column_name)
        .execute(&state.db)
        .await?;
    } else {
        // No rename: still ensure is_manual stays set. This is cheap and
        // closes the gap where a row could have been created via
        // `add_manual_column` but later had its flag cleared by an
        // overlapping refresh before we fixed the scheduler.
        sqlx::query(
            "UPDATE nl2sql_table_semantics SET is_manual = 1 \
             WHERE datasource_id = ? AND table_name = ? AND column_name = ?",
        )
        .bind(&id)
        .bind(&table_name)
        .bind(&column_name)
        .execute(&state.db)
        .await?;
    }
    if let Some(t) = req.col_type {
        columns[col_idx]["type"] = serde_json::json!(t);
    }
    if let Some(d) = req.description {
        columns[col_idx]["description"] = serde_json::json!(d);
    }
    if let Some(n) = req.nullable {
        columns[col_idx]["nullable"] = serde_json::json!(n);
    }

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

async fn delete_manual_column(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, table_name, column_name)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>> {
    // Authorization check
    let row = sqlx::query("SELECT tenant_id, user_id FROM data_sources WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    let (ds_tenant, ds_user): (String, Option<String>) = match row {
        Some(r) => (r.get("tenant_id"), r.get("user_id")),
        None => return Err(AppError::NotFound("data source not found".into())),
    };
    if ds_tenant != claims.tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = claims.role == "admin" || claims.role == "superadmin";
    if ds_user.as_ref() != Some(&claims.sub) && !is_admin {
        return Err(AppError::Forbidden);
    }

    let schema_json: serde_json::Value =
        sqlx::query_scalar("SELECT COALESCE(schema_info, '[]') FROM data_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;

    let mut tables = crate::nl2sql::schema_diff::extract_schema_tables(&schema_json);

    let idx = tables
        .iter()
        .position(|t| {
            t.get("table_name")
                .and_then(|v| v.as_str())
                .map(|n| n == table_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("table not found".into()))?;

    let columns = tables[idx]
        .get_mut("columns")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| AppError::ValidationError("invalid schema".into()))?;

    let col_idx = columns
        .iter()
        .position(|c| {
            c.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n == column_name)
                .unwrap_or(false)
        })
        .ok_or_else(|| AppError::NotFound("column not found".into()))?;

    columns.remove(col_idx);

    sqlx::query("UPDATE data_sources SET schema_info = ? WHERE id = ?")
        .bind(serde_json::json!(&tables))
        .bind(&id)
        .execute(&state.db)
        .await?;

    sqlx::query(
        "DELETE FROM nl2sql_table_semantics \
         WHERE datasource_id = ? AND table_name = ? AND column_name = ? AND is_manual = 1 AND deleted_at IS NULL",
    )
    .bind(&id)
    .bind(&table_name)
    .bind(&column_name)
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

// ── Batch Import / Export ─────────────────────────────────────────────────────

async fn export_all(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<BatchExportResponse>> {
    let tenant_id = &claims.tenant_id;

    let rows = sqlx::query(
        "SELECT id, name, description, db_type, visibility, config, \
         schema_info, sensitive_columns, enabled FROM data_sources WHERE tenant_id = ? ORDER BY name",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let mut data_sources = Vec::with_capacity(rows.len());
    for row in rows {
        let ds_id: String = row.get("id");
        let name: String = row.get("name");
        let description: Option<String> = row.get("description");
        let db_type: String = row.get("db_type");
        let visibility: String = row.get("visibility");
        let config_json: serde_json::Value = row.get("config");
        let schema_info: Option<serde_json::Value> = row.get("schema_info");
        let sensitive_columns: Option<Vec<String>> = row
            .get::<Option<serde_json::Value>, _>("sensitive_columns")
            .and_then(|v| serde_json::from_value(v).ok());
        let enabled: bool = row.get("enabled");

        let config = decrypt_config(&config_json, &state.data_dir).map_err(CryptoError::from)?;

        let table_semantics = load_table_semantics(&state.db, &ds_id).await?;
        let datasource_semantics = load_datasource_semantics(&state.db, &ds_id).await?;

        data_sources.push(ExportedDataSource {
            name,
            description,
            db_type,
            visibility,
            config,
            schema_info,
            sensitive_columns,
            enabled,
            table_semantics,
            datasource_semantics,
        });
    }

    Ok(Json(BatchExportResponse {
        version: "1.0".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        data_sources,
    }))
}

async fn load_table_semantics(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Result<Vec<ExportedTableSemantics>> {
    let table_rows = sqlx::query(
        "SELECT table_name, ai_description, user_description \
         FROM nl2sql_table_desc_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await?;

    let column_rows = sqlx::query(
        "SELECT table_name, column_name, semantic_description, user_description \
         FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await?;

    let mut by_table: std::collections::BTreeMap<String, ExportedTableSemantics> =
        std::collections::BTreeMap::new();

    for row in table_rows {
        let table_name: String = row.get("table_name");
        let ai_description: Option<String> = row.get("ai_description");
        let user_description: Option<String> = row.get("user_description");
        let table_description = ai_description
            .filter(|s| !s.trim().is_empty())
            .or_else(|| user_description.filter(|s| !s.trim().is_empty()));
        if table_description.is_none() {
            by_table
                .entry(table_name.clone())
                .or_insert_with(|| ExportedTableSemantics {
                    table_name: table_name.clone(),
                    table_description: None,
                    columns: Vec::new(),
                });
            continue;
        }
        by_table
            .entry(table_name.clone())
            .or_insert_with(|| ExportedTableSemantics {
                table_name: table_name.clone(),
                table_description: None,
                columns: Vec::new(),
            })
            .table_description = table_description;
    }

    for row in column_rows {
        let table_name: String = row.get("table_name");
        let column_name: String = row.get("column_name");
        let semantic_description: Option<String> = row.get("semantic_description");
        let user_description: Option<String> = row.get("user_description");
        let semantic_present = semantic_description
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let description = semantic_description
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| user_description.as_deref().filter(|s| !s.trim().is_empty()))
            .unwrap_or_default()
            .to_string();
        if description.is_empty() {
            continue;
        }
        let description_type = if semantic_present { "ai" } else { "user" };

        by_table
            .entry(table_name.clone())
            .or_insert_with(|| ExportedTableSemantics {
                table_name,
                table_description: None,
                columns: Vec::new(),
            })
            .columns
            .push(ExportedColumnSemantics {
                column_name,
                description,
                description_type: description_type.to_string(),
            });
    }

    Ok(by_table.into_values().collect())
}

async fn load_datasource_semantics(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Result<Option<ExportedDatasourceSemantics>> {
    let row = sqlx::query(
        "SELECT ai_description, user_description FROM nl2sql_datasource_semantics \
         WHERE datasource_id = ? LIMIT 1",
    )
    .bind(datasource_id)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|r| ExportedDatasourceSemantics {
        description: {
            let ai: Option<String> = r.get("ai_description");
            let user: Option<String> = r.get("user_description");
            ai.filter(|s| !s.trim().is_empty())
                .or_else(|| user.filter(|s| !s.trim().is_empty()))
                .unwrap_or_default()
        },
        description_type: if r
            .get::<Option<String>, _>("ai_description")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            "ai".to_string()
        } else {
            "user".to_string()
        },
    }))
}

async fn import_batch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<BatchImportRequest>,
) -> Result<Json<BatchImportResponse>> {
    crate::routes::nl2sql::require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;

    let tenant_id = &claims.tenant_id;
    let user_id = &claims.sub;
    let overwrite = req.on_existing == "overwrite";
    let total = req.data_sources.len();

    let mut results = Vec::with_capacity(total);
    let mut succeeded = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for ds in req.data_sources {
        let outcome = import_single_datasource(&state, tenant_id, user_id, &ds, overwrite).await;

        match outcome {
            ImportOutcome::Created(id) => {
                succeeded += 1;
                results.push(ImportResult {
                    name: ds.name,
                    status: "created".to_string(),
                    id: Some(id),
                    error: None,
                });
            }
            ImportOutcome::Skipped(msg) => {
                skipped += 1;
                results.push(ImportResult {
                    name: ds.name,
                    status: "skipped".to_string(),
                    id: None,
                    error: Some(msg),
                });
            }
            ImportOutcome::Failed(msg) => {
                failed += 1;
                results.push(ImportResult {
                    name: ds.name,
                    status: "failed".to_string(),
                    id: None,
                    error: Some(msg),
                });
            }
        }
    }

    Ok(Json(BatchImportResponse {
        total,
        succeeded,
        skipped,
        failed,
        results,
    }))
}

/// Returns `Ok(id)` on success, `Err((msg, was_skipped))` on skip or failure.
async fn import_single_datasource(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    ds: &ExportedDataSource,
    overwrite: bool,
) -> ImportOutcome {
    // Check for existing data source by name.
    let existing = sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT id, config FROM data_sources WHERE tenant_id = ? AND name = ?",
    )
    .bind(tenant_id)
    .bind(&ds.name)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(v)) => v,
        Ok(None) => {
            return match create_datasource(state, tenant_id, user_id, ds).await {
                Ok(id) => ImportOutcome::Created(id),
                Err(msg) => ImportOutcome::Failed(msg.to_string()),
            };
        }
        Err(e) => return ImportOutcome::Failed(e.to_string()),
    };

    let (existing_id, _existing_config) = existing;

    if !overwrite {
        return ImportOutcome::Skipped(format!("data source '{}' already exists", ds.name));
    }

    let encrypted = match encrypt_config(&ds.config, &state.data_dir) {
        Ok(v) => v,
        Err(e) => return ImportOutcome::Failed(e.to_string()),
    };

    let ds_sensitive_json: Option<String> = ds
        .sensitive_columns
        .as_ref()
        .map(|cols| serde_json::to_string(cols).unwrap_or_else(|_| "[]".to_string()));

    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE data_sources SET \
         description = ?, db_type = ?, visibility = ?, config = ?, \
         schema_info = ?, sensitive_columns = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ?",
    )
    .bind(&ds.description)
    .bind(&ds.db_type)
    .bind(&ds.visibility)
    .bind(&encrypted)
    .bind(&ds.schema_info)
    .bind(&ds_sensitive_json)
    .bind(ds.enabled)
    .bind(&existing_id)
    .execute(&state.db)
    .await
    {
        return ImportOutcome::Failed(e.to_string());
    }

    let embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    if let Err(msg) =
        import_semantics(&state.db, tenant_id, &existing_id, ds, &embedding_model).await
    {
        return ImportOutcome::Failed(msg);
    }

    ImportOutcome::Created(existing_id)
}

async fn create_datasource(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    ds: &ExportedDataSource,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let encrypted = encrypt_config(&ds.config, &state.data_dir)?;
    let sc_json: Option<String> = ds
        .sensitive_columns
        .as_ref()
        .map(|cols| serde_json::to_string(cols).unwrap_or_else(|_| "[]".to_string()));

    sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO data_sources \
         (id, tenant_id, user_id, name, description, db_type, visibility, \
          config, schema_info, sensitive_columns, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&ds.name)
    .bind(&ds.description)
    .bind(&ds.db_type)
    .bind(&ds.visibility)
    .bind(&encrypted)
    .bind(&ds.schema_info)
    .bind(&sc_json)
    .bind(ds.enabled)
    .execute(&state.db)
    .await?;

    let embedding_model =
        crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, Some("nl2sql"))
            .await
            .map(|cfg| cfg.model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string());

    if let Err(msg) = import_semantics(&state.db, tenant_id, &id, ds, &embedding_model).await {
        return Err(AppError::Internal(msg));
    }

    Ok(id)
}

async fn import_semantics(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    ds: &ExportedDataSource,
    embedding_model: &str,
) -> std::result::Result<(), String> {
    // Upsert table-level and column-level semantics.
    for table_sem in &ds.table_semantics {
        // Upsert table description.
        if let Some(ref table_desc) = table_sem.table_description {
            sqlx::query(
                "INSERT INTO nl2sql_table_desc_semantics \
                 (tenant_id, datasource_id, table_name, ai_description, embedding_model) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT DO UPDATE SET \
                 ai_description = excluded.ai_description, embedding_model = excluded.embedding_model",
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&table_sem.table_name)
            .bind(table_desc)
            .bind(embedding_model)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
        }

        // Upsert each column.
        for col in &table_sem.columns {
            sqlx::query(
            "INSERT INTO nl2sql_table_semantics \
             (tenant_id, datasource_id, table_name, column_name, semantic_description, user_description, embedding_model) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT DO UPDATE SET \
             semantic_description = excluded.semantic_description, \
             user_description = excluded.user_description, \
             embedding_model = excluded.embedding_model",
            )
            .bind(tenant_id)
            .bind(datasource_id)
            .bind(&table_sem.table_name)
            .bind(&col.column_name)
            .bind(if col.description_type == "ai" { &col.description } else { "" })
            .bind(if col.description_type == "user" { &col.description } else { "" })
            .bind(embedding_model)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
        }
    }

    // Upsert datasource-level semantics.
    if let Some(ref ds_sem) = ds.datasource_semantics {
        sqlx::query(
            "INSERT INTO nl2sql_datasource_semantics \
             (tenant_id, datasource_id, ai_description, user_description, embedding_model) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT DO UPDATE SET \
             ai_description = excluded.ai_description, \
             user_description = excluded.user_description, \
             embedding_model = excluded.embedding_model",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(if ds_sem.description_type == "ai" {
            &ds_sem.description
        } else {
            ""
        })
        .bind(if ds_sem.description_type == "user" {
            &ds_sem.description
        } else {
            ""
        })
        .bind(embedding_model)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/", routing_post(create))
        .route(
            "/trino/schemas",
            routing_post(discover_trino_schemas_for_config),
        )
        .route("/{id}", routing_get(get))
        .route("/{id}", patch(update))
        .route("/{id}", routing_delete(delete))
        .route("/{id}/test", routing_post(test_connection))
        .route("/{id}/discover", routing_post(discover_schema))
        .route(
            "/{id}/discover/{table}",
            routing_post(discover_table_schema),
        )
        .route("/{id}/import-sql-schema", routing_post(import_sql_schema))
        // Manual table/column management
        .route("/{id}/tables", routing_post(add_manual_table))
        .route("/{id}/tables/{table}", routing_put(put_manual_table))
        .route("/{id}/tables/{table}", routing_delete(delete_manual_table))
        .route(
            "/{id}/tables/{table}/columns",
            routing_post(add_manual_column),
        )
        .route(
            "/{id}/tables/{table}/columns/{column}",
            routing_put(put_manual_column),
        )
        .route(
            "/{id}/tables/{table}/columns/{column}",
            routing_delete(delete_manual_column),
        )
        // Batch import / export (must be before /{id} wildcard)
        .route("/export", routing_get(export_all))
        .route("/import", routing_post(import_batch))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

/// Check if any existing datasource in the same tenant has the same db_type + host + port + database.
/// Returns the conflicting datasource name if found.
async fn check_duplicate_config(
    db: &sqlx::SqlitePool,
    data_dir: &std::path::Path,
    tenant_id: &str,
    db_type: &str,
    host: &str,
    port: u16,
    database: &str,
    exclude_id: &str,
) -> Option<String> {
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT id, name, config FROM data_sources \
         WHERE tenant_id = ? AND db_type = ? AND deleted_at IS NULL AND id != ?",
    )
    .bind(tenant_id)
    .bind(db_type)
    .bind(exclude_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for (_, name, config_json) in rows {
        if let Ok(decrypted) = decrypt_config(&config_json, data_dir) {
            if let Ok(cfg) = serde_json::from_value::<SqlConfig>(decrypted) {
                if cfg.host == host && cfg.port == port && cfg.database == database {
                    return Some(name);
                }
            }
        }
    }
    None
}

fn generate_nonce() -> [u8; 12] {
    let uuid = uuid::Uuid::new_v4();
    let mut bytes = uuid.as_bytes().to_vec();
    bytes.truncate(12);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mongodb_probe_rejects_unreachable_server() {
        let result = probe_connection(
            "mongodb",
            &serde_json::json!({
                "host": "127.0.0.1",
                "port": 1,
                "database": "aos_probe",
                "username": "",
                "password": ""
            }),
            None,
        )
        .await
        .expect("probe should return a failed result");

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn parse_sql_schema_tables_handles_trino_ddl_and_nested_types() {
        let ddl = r#"
CREATE TABLE iceberg.mps_prod.business_order (
  order_id varchar COMMENT '订单ID',
  user_id bigint NOT NULL,
  pay_amount decimal(18, 2),
  tags array(varchar),
  attrs map(varchar, varchar),
  PRIMARY KEY (order_id)
)
WITH (format = 'PARQUET');
"#;

        let tables = parse_sql_schema_tables(ddl);

        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].table_name, "iceberg.mps_prod.business_order");
        assert_eq!(tables[0].columns.len(), 5);
        assert_eq!(tables[0].columns[0].name, "order_id");
        assert_eq!(tables[0].columns[0].col_type, "varchar");
        assert_eq!(tables[0].columns[0].description.as_deref(), Some("订单ID"));
        assert_eq!(tables[0].columns[1].nullable, Some(false));
        assert_eq!(tables[0].columns[2].col_type, "decimal(18, 2)");
        assert_eq!(tables[0].columns[4].col_type, "map(varchar, varchar)");
    }

    #[test]
    fn qualify_trino_imported_table_uses_datasource_catalog_and_schema() {
        let cfg = TrinoConfig {
            host: "trino.example.com".to_string(),
            port: 443,
            catalog: "iceberg".to_string(),
            schema: "mps_prod".to_string(),
            schemas: vec!["analyst".to_string()],
            username: "user".to_string(),
            password: String::new(),
            ssl: Some(true),
            basic_auth: Some(true),
        };

        assert_eq!(
            qualify_trino_imported_table(&cfg, "business_order"),
            (
                "iceberg".to_string(),
                "mps_prod".to_string(),
                "business_order".to_string()
            )
        );
        assert_eq!(
            qualify_trino_imported_table(&cfg, "analyst.user_daily"),
            (
                "iceberg".to_string(),
                "analyst".to_string(),
                "user_daily".to_string()
            )
        );
    }
}
