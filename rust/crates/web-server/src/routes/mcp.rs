//! MCP (Model Context Protocol) server management API.
//!
//! Primary data source: the local SQLite `mcp_server_registry` table.
//!
//! The runtime reads enabled servers from SQLite via `TenantConfigRegistry`.
//! Changes are hot-reloaded into the per-tenant `McpServerSessionManager` and
//! broadcast to WebSocket clients for frontend UI refresh.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::system_events;
use crate::routes::PaginationParams;
use crate::state::AppState;

// ── SQL constants ─────────────────────────────────────────────────────────────

const MCP_SELECT_COLUMNS: &str = r"SELECT id, tenant_id, name, transport, COALESCE(command, ''),
               COALESCE(args, ''), COALESCE(url, ''), enabled,
               COALESCE(connection_status, 'disconnected'),
               status, COALESCE(last_error, ''), created_at, updated_at,
               COALESCE(auth_type, 'none'), COALESCE(auth_token, ''),
               COALESCE(extra_headers, ''), CAST(COALESCE(timeout_ms, 60000) AS INTEGER),
               COALESCE(env, ''), COALESCE(tools_json, '')
        FROM mcp_server_registry
";

const REDACTED_MCP_ENV_VALUE: &str = "********";
const MCP_STDIO_COLD_START_TIMEOUT_MS: u64 = 600_000;
const MCP_STDIO_INITIALIZE_TIMEOUT_MS: u64 = 60_000;
const MCP_STDIO_DISCOVERY_TIMEOUT_MS: u64 = 60_000;

// ── DTOs ────────────────────────────────────────────────────────────────────

/// Authentication type for remote MCP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum McpAuthType {
    #[default]
    None,
    BearerToken,
    OAuth,
}

impl std::fmt::Display for McpAuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::BearerToken => write!(f, "bearer_token"),
            Self::OAuth => write!(f, "oauth"),
        }
    }
}

impl From<&str> for McpAuthType {
    fn from(s: &str) -> Self {
        match s {
            "bearer_token" => Self::BearerToken,
            "oauth" => Self::OAuth,
            _ => Self::None,
        }
    }
}

/// Whether the server has an auth token configured (token value is never returned to the client).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpServerAuthInfo {
    pub auth_type: String,
    pub has_token: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
}

/// Connection status of an MCP server, tested from the `WebUI`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpConnectionStatus {
    /// Server has not been tested or was never reachable.
    #[default]
    Disconnected,
    /// Server responded successfully to initialize + tools/list.
    Connected,
    /// Server returned a non-2xx HTTP response or JSON-RPC error.
    Error,
}

impl std::fmt::Display for McpConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connected => write!(f, "connected"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl From<&str> for McpConnectionStatus {
    fn from(s: &str) -> Self {
        match s {
            "connected" => Self::Connected,
            "error" => Self::Error,
            _ => Self::Disconnected,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub id: Option<String>,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Environment variable names are returned with redacted values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub tools_count: u32,
    /// WebUI-tested connectivity: disconnected | connected | error.
    #[serde(default)]
    pub connection_status: McpConnectionStatus,
    /// Runtime health during session discovery: unknown | healthy | unhealthy | configured.
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// Auth configuration (token value is redacted).
    #[serde(default)]
    pub auth: McpServerAuthInfo,
    /// Cached tools exposed by this MCP server (populated by runtime discovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    /// Cached resources exposed by this MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<serde_json::Value>>,
    /// Cached prompts exposed by this MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<serde_json::Value>>,
}

fn default_status() -> String {
    "unknown".to_string()
}

#[derive(Debug, Serialize)]
pub struct McpListResponse {
    pub servers: Vec<McpServerInfo>,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct McpToolsResponse {
    pub tools: Vec<serde_json::Value>,
    pub count: u32,
}

#[derive(Debug, Serialize)]
pub struct McpResourcesResponse {
    pub resources: Vec<serde_json::Value>,
    pub count: u32,
}

#[derive(Debug, Serialize)]
pub struct McpPromptsResponse {
    pub prompts: Vec<serde_json::Value>,
    pub count: u32,
}

#[derive(Debug, Deserialize)]
pub struct AddMcpServerRequest {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub url: Option<String>,
    #[serde(default = "default_enabled_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auth_type: Option<String>,
    pub auth_token: Option<String>,
    pub extra_headers: Option<serde_json::Value>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: Option<u32>,
}

fn default_enabled_true() -> bool {
    true
}

#[allow(clippy::unnecessary_wraps)]
fn default_timeout_ms() -> Option<u32> {
    Some(60000)
}

fn validate_stdio_command(command: Option<&String>) -> Result<()> {
    let Some(command) = command
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Err(AppError::ValidationError(
            "stdio MCP command is required".to_string(),
        ));
    };
    if command.split_whitespace().count() > 1 {
        return Err(AppError::ValidationError(
            "stdio MCP command must be only the executable name/path. Put arguments in the Arguments field, for example command `uvx` and arguments `mcp-server-freesearch`.".to_string(),
        ));
    }
    let command_path = std::path::Path::new(command);
    let has_path_component = command_path.is_absolute()
        || command_path.components().count() > 1
        || command.contains(std::path::MAIN_SEPARATOR);
    let found = if has_path_component {
        command_path.is_file()
    } else {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|directory| {
                let candidate = directory.join(command);
                if candidate.is_file() {
                    return true;
                }
                #[cfg(windows)]
                {
                    return ["exe", "cmd", "bat", "com"].iter().any(|extension| {
                        directory.join(format!("{command}.{extension}")).is_file()
                    });
                }
                #[cfg(not(windows))]
                false
            })
        })
    };
    if !found {
        return Err(AppError::ValidationError(format!(
            "stdio MCP command `{command}` was not found. Install it and ensure it is available on the AOS process PATH."
        )));
    }
    Ok(())
}

fn validate_stdio_working_directories(args: &[String]) -> Result<()> {
    for (index, arg) in args.iter().enumerate() {
        let directory = if matches!(arg.as_str(), "--directory" | "--cwd" | "-C") {
            args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix("--directory=")
                .or_else(|| arg.strip_prefix("--cwd="))
        };
        let Some(directory) = directory.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let path = std::path::Path::new(directory);
        if !path.is_dir() {
            let resolved = std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf());
            return Err(AppError::ValidationError(format!(
                "stdio MCP working directory `{directory}` does not exist (resolved as `{}`). Use an existing absolute path or a path relative to the AOS startup directory.",
                resolved.display()
            )));
        }
    }
    Ok(())
}

async fn probe_stdio_server(
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    timeout_ms: u32,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let mut session = runtime::mcp::StdioTransportBuilder::new(name, command)
        .args(args.to_vec())
        .env(env.clone())
        .timeout_ms(u64::from(timeout_ms))
        .build()
        .map_err(|error| format!("failed to start stdio MCP process: {error}"))?;
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(u64::from(timeout_ms).clamp(1_000, 600_000)),
        session.list_tools(),
    )
    .await
    .map_err(|_| format!("stdio MCP initialize/tools-list timed out after {timeout_ms} ms"))
    .and_then(|result| {
        result
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| serde_json::to_value(tool).ok())
                    .collect::<Vec<_>>()
            })
            .map_err(|error| format!("stdio MCP initialize/tools-list failed: {error}"))
    });
    let _ = session.shutdown().await;
    result
}

fn stdio_command_requires_install(command: &str) -> bool {
    std::path::Path::new(command)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| matches!(name.to_ascii_lowercase().as_str(), "uvx" | "npx"))
}

async fn stdio_lifecycle_is_current(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    name: &str,
    revision: i64,
) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM mcp_server_registry
         WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1 AND transport = 'stdio'",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(revision)
    .fetch_one(db)
    .await
    .unwrap_or(0)
        == 1
}

async fn wait_for_stdio_lifecycle_invalidation(
    db: sqlx::SqlitePool,
    tenant_id: String,
    name: String,
    revision: i64,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if !stdio_lifecycle_is_current(&db, &tenant_id, &name, revision).await {
            return;
        }
    }
}

async fn persist_stdio_lifecycle_failure(
    state: &AppState,
    tenant_id: &str,
    name: &str,
    revision: i64,
    error: &str,
) {
    let error = error.chars().take(3_500).collect::<String>();
    let updated = sqlx::query(
        "UPDATE mcp_server_registry
         SET status = 'failed', connection_status = 'error', last_error = ?,
             lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1",
    )
    .bind(&error)
    .bind(tenant_id)
    .bind(name)
    .bind(revision)
    .execute(&state.db)
    .await
    .is_ok_and(|result| result.rows_affected() == 1);
    if updated {
        system_events::broadcast_mcp_status_changed(tenant_id, name, "failed", Some(&error));
    }
}

fn spawn_stdio_lifecycle(
    state: AppState,
    tenant_id: String,
    name: String,
    revision: i64,
    actor_user_id: String,
) {
    tokio::spawn(async move {
        let row = sqlx::query_as::<_, (String, String, String, i64)>(
            "SELECT COALESCE(command, ''), COALESCE(args, '[]'), COALESCE(env, '{}'),
                    CAST(COALESCE(timeout_ms, 60000) AS INTEGER)
             FROM mcp_server_registry
             WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1
               AND transport = 'stdio'",
        )
        .bind(&tenant_id)
        .bind(&name)
        .bind(revision)
        .fetch_optional(&state.db)
        .await;
        let Ok(Some((command, args_json, env_json, tool_timeout_ms))) = row else {
            return;
        };
        let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
        let env = parse_mcp_env(&env_json);
        let initial_status = if stdio_command_requires_install(&command) {
            "installing"
        } else {
            "starting"
        };
        let claimed = sqlx::query(
            "UPDATE mcp_server_registry
             SET status = ?, connection_status = 'disconnected', last_error = NULL,
                 attempt_count = attempt_count + 1, last_attempt_at = CURRENT_TIMESTAMP,
                 lease_expires_at = datetime(CURRENT_TIMESTAMP, '+11 minutes'),
                 updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1
               AND transport = 'stdio' AND status = 'queued'",
        )
        .bind(initial_status)
        .bind(&tenant_id)
        .bind(&name)
        .bind(revision)
        .execute(&state.db)
        .await
        .is_ok_and(|result| result.rows_affected() == 1);
        if !claimed {
            return;
        }
        system_events::broadcast_mcp_status_changed(&tenant_id, &name, initial_status, None);

        let cold_start = stdio_command_requires_install(&command);
        let initialize_timeout_ms = if cold_start {
            MCP_STDIO_COLD_START_TIMEOUT_MS
        } else {
            MCP_STDIO_INITIALIZE_TIMEOUT_MS
        };
        let mut session = match runtime::mcp::StdioTransportBuilder::new(&name, &command)
            .args(args)
            .env(env)
            .timeout_ms(u64::try_from(tool_timeout_ms).unwrap_or(60_000))
            .initialize_timeout_ms(initialize_timeout_ms)
            .list_timeout_ms(MCP_STDIO_DISCOVERY_TIMEOUT_MS)
            .build()
        {
            Ok(session) => session,
            Err(error) => {
                persist_stdio_lifecycle_failure(
                    &state,
                    &tenant_id,
                    &name,
                    revision,
                    &format!("failed to start stdio MCP process: {error}"),
                )
                .await;
                return;
            }
        };

        let invalidated = wait_for_stdio_lifecycle_invalidation(
            state.db.clone(),
            tenant_id.clone(),
            name.clone(),
            revision,
        );
        let initialize = tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_millis(initialize_timeout_ms),
                session.initialize(),
            ) => Some(result),
            () = invalidated => None,
        };
        let Some(initialize) = initialize else {
            let _ = session.shutdown().await;
            return;
        };
        match initialize {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                let _ = session.shutdown().await;
                persist_stdio_lifecycle_failure(
                    &state,
                    &tenant_id,
                    &name,
                    revision,
                    &format!("stdio MCP initialize failed: {error}"),
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = session.shutdown().await;
                persist_stdio_lifecycle_failure(
                    &state,
                    &tenant_id,
                    &name,
                    revision,
                    "stdio MCP installation/startup timed out",
                )
                .await;
                return;
            }
        }

        let discovering = sqlx::query(
            "UPDATE mcp_server_registry SET status = 'discovering', updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1",
        )
        .bind(&tenant_id)
        .bind(&name)
        .bind(revision)
        .execute(&state.db)
        .await
        .is_ok_and(|result| result.rows_affected() == 1);
        if !discovering {
            let _ = session.shutdown().await;
            return;
        }
        system_events::broadcast_mcp_status_changed(&tenant_id, &name, "discovering", None);
        let invalidated = wait_for_stdio_lifecycle_invalidation(
            state.db.clone(),
            tenant_id.clone(),
            name.clone(),
            revision,
        );
        let tools = tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_millis(MCP_STDIO_DISCOVERY_TIMEOUT_MS),
                session.list_tools(),
            ) => Some(result),
            () = invalidated => None,
        };
        let Some(tools) = tools else {
            let _ = session.shutdown().await;
            return;
        };
        let tools = match tools {
            Ok(Ok(tools)) => tools
                .iter()
                .filter_map(|tool| serde_json::to_value(tool).ok())
                .collect::<Vec<_>>(),
            Ok(Err(error)) => {
                let _ = session.shutdown().await;
                persist_stdio_lifecycle_failure(
                    &state,
                    &tenant_id,
                    &name,
                    revision,
                    &format!("stdio MCP tools/list failed: {error}"),
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = session.shutdown().await;
                persist_stdio_lifecycle_failure(
                    &state,
                    &tenant_id,
                    &name,
                    revision,
                    "stdio MCP tool discovery timed out",
                )
                .await;
                return;
            }
        };
        let _ = session.shutdown().await;
        let tools_json = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string());
        let completed = sqlx::query(
            "UPDATE mcp_server_registry
             SET status = 'healthy', connection_status = 'connected', last_error = NULL,
                 tools_json = ?, lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE tenant_id = ? AND name = ? AND revision = ? AND enabled = 1",
        )
        .bind(tools_json)
        .bind(&tenant_id)
        .bind(&name)
        .bind(revision)
        .execute(&state.db)
        .await
        .is_ok_and(|result| result.rows_affected() == 1);
        if !completed {
            return;
        }
        system_events::broadcast_mcp_status_changed(&tenant_id, &name, "healthy", None);
        if let Some(manager) = &state.agent_manager {
            if let Err(error) = manager.reload_mcp_servers(&tenant_id, &actor_user_id).await {
                tracing::warn!(tenant_id, name, %error, "failed to hot-reload installed MCP server");
            }
        }
    });
}

fn parse_mcp_env(raw: &str) -> BTreeMap<String, String> {
    if raw.is_empty() {
        return BTreeMap::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

fn redact_mcp_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.keys()
        .map(|key| (key.clone(), REDACTED_MCP_ENV_VALUE.to_string()))
        .collect()
}

fn merge_redacted_mcp_env(
    requested: BTreeMap<String, String>,
    existing: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    requested
        .into_iter()
        .map(|(key, value)| {
            if value == REDACTED_MCP_ENV_VALUE {
                let preserved = existing.get(&key).cloned().unwrap_or(value);
                (key, preserved)
            } else {
                (key, value)
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct McpStats {
    pub total_servers: i64,
    pub healthy_servers: i64,
    pub total_tools: i64,
    pub transport_distribution: std::collections::HashMap<String, i64>,
}

#[derive(Debug, Deserialize)]
pub struct ToggleMcpServerRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMcpServerRequest {
    pub name: Option<String>,
    pub transport: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub url: Option<String>,
    pub enabled: Option<bool>,
    pub auth_type: Option<String>,
    pub auth_token: Option<String>,
    pub extra_headers: Option<serde_json::Value>,
    pub timeout_ms: Option<u32>,
}

// ── MCP Tool/Resource Discovery Helpers ──────────────────────────────────────────

/// Performs the full MCP HTTP handshake (initialize + notifications/initialized) and
/// discovers all tools from an HTTP/SSE MCP server. Returns the tools array on success.
async fn discover_mcp_tools(
    state: &AppState,
    tenant_id: &str,
    server_name: &str,
    server: &McpServerInfo,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let url = server.url.as_deref().ok_or("missing URL")?.trim();
    let timeout_ms = u64::from(server.auth.timeout_ms.unwrap_or(60_000));

    let raw_token: Option<String> = sqlx::query_scalar(
        "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
    )
    .bind(tenant_id)
    .bind(server_name)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db query failed: {e}"))?;

    let bearer_token = (server.auth.auth_type == "bearer_token")
        .then_some(raw_token.as_deref())
        .flatten();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Step 1: initialize
    let (init_body, session_id) = http_jsonrpc_call(
        &client,
        url,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aos-web", "version": "1.0.0" }
        }),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        None,
        false, // not a notification
    )
    .await?;

    // Check for initialize errors
    if let Some(err) = parse_rpc_error(&init_body) {
        return Err(format!("initialize failed: {err}"));
    }

    // Step 2: notifications/initialized (required by MCP spec), passing session_id if available.
    // Marked as a notification (no id field) so the server does not expect a response.
    let _ = http_jsonrpc_call(
        &client,
        url,
        "notifications/initialized",
        serde_json::json!({}),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        true, // IS a notification — must NOT include id field
    )
    .await;

    // Step 3: tools/list (with session_id for stateful servers like ModelScope)
    let (tools_body, _session_id) = http_jsonrpc_call(
        &client,
        url,
        "tools/list",
        serde_json::json!(null),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        false, // not a notification
    )
    .await
    .map_err(|e| e.to_string())?;

    let tools = parse_mcp_tools(&tools_body)?;
    debug!(server = %server_name, tool_count = tools.len(), "discovered MCP tools");
    Ok(tools)
}

/// Performs MCP discovery for resources (initialize + notifications/initialized + resources/list).
async fn discover_mcp_resources(
    state: &AppState,
    tenant_id: &str,
    server_name: &str,
    server: &McpServerInfo,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let url = server.url.as_deref().ok_or("missing URL")?.trim();
    let timeout_ms = u64::from(server.auth.timeout_ms.unwrap_or(60_000));

    let raw_token: Option<String> = sqlx::query_scalar(
        "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
    )
    .bind(tenant_id)
    .bind(server_name)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db query failed: {e}"))?;

    let bearer_token = (server.auth.auth_type == "bearer_token")
        .then_some(raw_token.as_deref())
        .flatten();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    // Step 1: initialize
    let (init_body, session_id) = http_jsonrpc_call(
        &client,
        url,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aos-web", "version": "1.0.0" }
        }),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        None,
        false, // not a notification
    )
    .await?;

    debug!(
        server = %server_name,
        session_id = %session_id.as_ref().unwrap_or(&"<none>".into()),
        "discover_mcp_resources: initialize response"
    );

    if let Some(err) = parse_rpc_error(&init_body) {
        return Err(format!("initialize failed: {err}"));
    }

    // Step 2: notifications/initialized (required by MCP spec)
    let _ = http_jsonrpc_call(
        &client,
        url,
        "notifications/initialized",
        serde_json::json!({}),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        true, // IS a notification — must NOT include id field
    )
    .await;

    // Step 3: resources/list
    let (resources_body, _session_id) = http_jsonrpc_call(
        &client,
        url,
        "resources/list",
        serde_json::json!(null),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        false, // not a notification
    )
    .await
    .map_err(|e| e.to_string())?;

    let resources = parse_mcp_resources(&resources_body)?;
    debug!(server = %server_name, resource_count = resources.len(), "discovered MCP resources");
    Ok(resources)
}

/// Performs MCP discovery for prompts (initialize + notifications/initialized + prompts/list).
async fn discover_mcp_prompts(
    state: &AppState,
    tenant_id: &str,
    server_name: &str,
    server: &McpServerInfo,
) -> std::result::Result<Vec<serde_json::Value>, String> {
    let url = server.url.as_deref().ok_or("missing URL")?.trim();
    let timeout_ms = u64::from(server.auth.timeout_ms.unwrap_or(60_000));

    let raw_token: Option<String> = sqlx::query_scalar(
        "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
    )
    .bind(tenant_id)
    .bind(server_name)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db query failed: {e}"))?;

    let bearer_token = (server.auth.auth_type == "bearer_token")
        .then_some(raw_token.as_deref())
        .flatten();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let (init_body, session_id) = http_jsonrpc_call(
        &client,
        url,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aos-web", "version": "1.0.0" }
        }),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        None,
        false,
    )
    .await?;

    if let Some(err) = parse_rpc_error(&init_body) {
        return Err(format!("initialize failed: {err}"));
    }

    let _ = http_jsonrpc_call(
        &client,
        url,
        "notifications/initialized",
        serde_json::json!({}),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        true,
    )
    .await;

    let (prompts_body, _session_id) = http_jsonrpc_call(
        &client,
        url,
        "prompts/list",
        serde_json::json!(null),
        bearer_token,
        server.auth.extra_headers.as_ref(),
        session_id.as_deref(),
        false,
    )
    .await
    .map_err(|e| e.to_string())?;

    let prompts = parse_mcp_prompts(&prompts_body)?;
    debug!(server = %server_name, prompt_count = prompts.len(), "discovered MCP prompts");
    Ok(prompts)
}

/// Low-level HTTP JSON-RPC call helper used by discovery functions.
/// Set `is_notification` to `true` for methods that must NOT have an `id` field
/// (e.g. `notifications/initialized` per MCP spec), so the server treats the request
/// as a fire-and-forget notification rather than expecting a matching response.
async fn http_jsonrpc_call(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
    bearer_token: Option<&str>,
    extra_headers: Option<&serde_json::Value>,
    session_id: Option<&str>,
    is_notification: bool,
) -> std::result::Result<(String, Option<String>), String> {
    let body = if is_notification {
        // Notifications must NOT include an `id` field — the server must NOT send a response.
        if params.is_null() {
            serde_json::json!({ "jsonrpc": "2.0", "method": method })
        } else {
            serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
        }
    } else if params.is_null() {
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method })
    } else {
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    };

    let mut req_builder = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body);

    if let Some(token) = bearer_token {
        req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
    }

    if let Some(headers) = extra_headers {
        if let Some(obj) = headers.as_object() {
            for (k, v) in obj {
                if let Some(val) = v.as_str() {
                    req_builder = req_builder.header(k.as_str(), val);
                }
            }
        }
    }

    // Required by stateful servers (e.g. ModelScope): include the server-assigned
    // session ID on all requests after initialize to maintain session state.
    if let Some(sid) = session_id {
        req_builder = req_builder.header("Mcp-Session-Id", sid);
    }

    let resp = req_builder.send().await.map_err(|e| e.to_string())?;

    let session_id: Option<String> = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let content_type: String = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();

    let status_code = resp.status().as_u16();

    if !(200..300).contains(&status_code) {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {status_code}: {}",
            &body_text[..body_text.len().min(500)]
        ));
    }

    let body_text = resp.text().await.map_err(|e| e.to_string())?;

    // If server responds with SSE, extract JSON from data: lines
    if content_type.contains("text/event-stream") {
        for line in body_text.lines() {
            let trimmed = line.trim();
            if let Some(stripped) = trimmed.strip_prefix("data:") {
                let json_str = stripped.trim();
                if !json_str.is_empty() && json_str != "[DONE]" {
                    return Ok((json_str.to_string(), session_id));
                }
            }
        }
        if body_text.trim().is_empty() {
            return Err("SSE response empty".to_string());
        }
        return Err(format!(
            "SSE response without JSON data: {}",
            &body_text[..body_text.len().min(200)]
        ));
    }

    Ok((body_text, session_id))
}

/// Parses a JSON-RPC error message from response body.
fn parse_rpc_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
}

/// Extracts the tools array from a JSON-RPC tools/list response.
fn parse_mcp_tools(body: &str) -> std::result::Result<Vec<serde_json::Value>, String> {
    let v =
        serde_json::from_str::<serde_json::Value>(body).map_err(|e| format!("parse error: {e}"))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        return Err(format!("server error {code}: {msg}"));
    }

    let tools = v
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .map(|arr| arr.clone())
        .unwrap_or_default();

    Ok(tools)
}

/// Extracts the resources array from a JSON-RPC resources/list response.
fn parse_mcp_resources(body: &str) -> std::result::Result<Vec<serde_json::Value>, String> {
    let v =
        serde_json::from_str::<serde_json::Value>(body).map_err(|e| format!("parse error: {e}"))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        return Err(format!("server error {code}: {msg}"));
    }

    let resources = v
        .get("result")
        .and_then(|r| r.get("resources"))
        .and_then(|arr| arr.as_array())
        .map(|arr| arr.clone())
        .unwrap_or_default();

    Ok(resources)
}

/// Extracts the prompts array from a JSON-RPC prompts/list response.
fn parse_mcp_prompts(body: &str) -> std::result::Result<Vec<serde_json::Value>, String> {
    let v =
        serde_json::from_str::<serde_json::Value>(body).map_err(|e| format!("parse error: {e}"))?;

    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        return Err(format!("server error {code}: {msg}"));
    }

    let prompts = v
        .get("result")
        .and_then(|r| r.get("prompts"))
        .and_then(|arr| arr.as_array())
        .map(|arr| arr.clone())
        .unwrap_or_default();

    Ok(prompts)
}

// ── SQLite CRUD ───────────────────────────────────────────────────────────────

async fn db_list_servers(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<McpServerInfo>> {
    let rows = sqlx::query(&format!(
        "{MCP_SELECT_COLUMNS} WHERE tenant_id = ? ORDER BY name ASC LIMIT ? OFFSET ?",
    ))
    .bind(tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await?;

    let servers = rows
        .into_iter()
        .map(|row| {
            let id: String = sqlx::Row::get(&row, 0);
            let _tenant_id: String = sqlx::Row::get(&row, 1);
            let name: String = sqlx::Row::get(&row, 2);
            let transport: String = sqlx::Row::get(&row, 3);
            let command: String = sqlx::Row::get(&row, 4);
            let args: String = sqlx::Row::get(&row, 5);
            let url: String = sqlx::Row::get(&row, 6);
            let enabled: bool = sqlx::Row::get(&row, 7);
            let connection_status: String = sqlx::Row::get(&row, 8);
            let status: String = sqlx::Row::get(&row, 9);
            let last_error: String = sqlx::Row::get(&row, 10);
            let created_at: chrono::NaiveDateTime = sqlx::Row::get(&row, 11);
            let updated_at: chrono::NaiveDateTime = sqlx::Row::get(&row, 12);
            let auth_type: String = sqlx::Row::get(&row, 13);
            let auth_token: String = sqlx::Row::get(&row, 14);
            let extra_headers: String = sqlx::Row::get(&row, 15);
            let timeout_ms: u32 = sqlx::Row::get(&row, 16);
            let env: String = sqlx::Row::get(&row, 17);
            let tools_json: String = sqlx::Row::get(&row, 18);

            let args_vec: Vec<String> = if args.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&args).unwrap_or_default()
            };

            let extra_headers_val: Option<serde_json::Value> = if extra_headers.is_empty() {
                None
            } else {
                serde_json::from_str(&extra_headers).ok()
            };
            let tools = serde_json::from_str::<Vec<serde_json::Value>>(&tools_json)
                .ok()
                .filter(|tools| !tools.is_empty());

            McpServerInfo {
                id: Some(id),
                name,
                transport,
                command: if command.is_empty() {
                    None
                } else {
                    Some(command)
                },
                args: args_vec,
                env: redact_mcp_env(&parse_mcp_env(&env)),
                url: if url.is_empty() { None } else { Some(url) },
                enabled,
                tools_count: u32::try_from(tools.as_ref().map_or(0, Vec::len)).unwrap_or(u32::MAX),
                connection_status: connection_status.as_str().into(),
                status,
                last_error: if last_error.is_empty() {
                    None
                } else {
                    Some(last_error)
                },
                created_at: Some(created_at.format("%Y-%m-%dT%H:%M:%S").to_string()),
                updated_at: Some(updated_at.format("%Y-%m-%dT%H:%M:%S").to_string()),
                auth: McpServerAuthInfo {
                    auth_type,
                    has_token: !auth_token.is_empty(),
                    extra_headers: extra_headers_val,
                    timeout_ms: if timeout_ms == 60000 {
                        None
                    } else {
                        Some(timeout_ms)
                    },
                },
                tools,
                resources: None,
                prompts: None,
            }
        })
        .collect();

    Ok(servers)
}

async fn db_count_servers(db: &sqlx::SqlitePool, tenant_id: &str) -> Result<i64> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mcp_server_registry WHERE tenant_id = ?")
            .bind(tenant_id)
            .fetch_one(db)
            .await?;
    Ok(count)
}

#[derive(Debug, Serialize)]
pub struct McpTestResult {
    pub success: bool,
    pub tools_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct McpLatencyResult {
    pub success: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

async fn db_get_server(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    name: &str,
) -> Result<Option<McpServerInfo>> {
    let rows = sqlx::query(&format!(
        "{MCP_SELECT_COLUMNS} WHERE tenant_id = ? AND name = ?",
    ))
    .bind(tenant_id)
    .bind(name)
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        return Ok(None);
    }

    let row = &rows[0];
    let id: String = sqlx::Row::get(row, 0);
    let _tenant_id: String = sqlx::Row::get(row, 1);
    let name: String = sqlx::Row::get(row, 2);
    let transport: String = sqlx::Row::get(row, 3);
    let command: String = sqlx::Row::get(row, 4);
    let args: String = sqlx::Row::get(row, 5);
    let url: String = sqlx::Row::get(row, 6);
    let enabled: bool = sqlx::Row::get(row, 7);
    let connection_status: String = sqlx::Row::get(row, 8);
    let status: String = sqlx::Row::get(row, 9);
    let last_error: String = sqlx::Row::get(row, 10);
    let created_at: chrono::NaiveDateTime = sqlx::Row::get(row, 11);
    let updated_at: chrono::NaiveDateTime = sqlx::Row::get(row, 12);
    let auth_type: String = sqlx::Row::get(row, 13);
    let auth_token: String = sqlx::Row::get(row, 14);
    let extra_headers: String = sqlx::Row::get(row, 15);
    let timeout_ms: u32 = sqlx::Row::get(row, 16);
    let env: String = sqlx::Row::get(row, 17);
    let tools_json: String = sqlx::Row::get(row, 18);

    let args_vec: Vec<String> = if args.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&args).unwrap_or_default()
    };

    let extra_headers_val: Option<serde_json::Value> = if extra_headers.is_empty() {
        None
    } else {
        serde_json::from_str(&extra_headers).ok()
    };
    let tools = serde_json::from_str::<Vec<serde_json::Value>>(&tools_json)
        .ok()
        .filter(|tools| !tools.is_empty());

    Ok(Some(McpServerInfo {
        id: Some(id),
        name,
        transport,
        command: if command.is_empty() {
            None
        } else {
            Some(command)
        },
        args: args_vec,
        env: redact_mcp_env(&parse_mcp_env(&env)),
        url: if url.is_empty() { None } else { Some(url) },
        enabled,
        tools_count: u32::try_from(tools.as_ref().map_or(0, Vec::len)).unwrap_or(u32::MAX),
        connection_status: connection_status.as_str().into(),
        status,
        last_error: if last_error.is_empty() {
            None
        } else {
            Some(last_error)
        },
        created_at: Some(created_at.format("%Y-%m-%dT%H:%M:%S").to_string()),
        updated_at: Some(updated_at.format("%Y-%m-%dT%H:%M:%S").to_string()),
        auth: McpServerAuthInfo {
            auth_type,
            has_token: !auth_token.is_empty(),
            extra_headers: extra_headers_val,
            timeout_ms: if timeout_ms == 60000 {
                None
            } else {
                Some(timeout_ms)
            },
        },
        tools,
        resources: None,
        prompts: None,
    }))
}

async fn db_insert_server(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    req: &AddMcpServerRequest,
) -> Result<McpServerInfo> {
    let id = Uuid::new_v4().to_string();
    let args_json = serde_json::to_string(&req.args.as_ref().unwrap_or(&vec![]))?;
    let env = req.env.clone().unwrap_or_default();
    let env_json = serde_json::to_string(&env)?;
    let status = if !req.enabled {
        "disabled"
    } else if req.transport == "stdio" {
        "queued"
    } else {
        "configured"
    };
    let auth_type = req.auth_type.as_deref().unwrap_or("none");
    let auth_token = req.auth_token.as_deref().unwrap_or("");
    let extra_headers = req
        .extra_headers
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let timeout_ms: u32 = req.timeout_ms.unwrap_or(60000);

    sqlx::query(
        "
        INSERT INTO mcp_server_registry
            (id, tenant_id, name, transport, command, args, env, url, enabled, connection_status, status,
             auth_type, auth_token, extra_headers, timeout_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'disconnected', ?, ?, ?, ?, ?)
        ",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.transport)
    .bind(&req.command)
    .bind(&args_json)
    .bind(&env_json)
    .bind(&req.url)
    .bind(req.enabled)
    .bind(status)
    .bind(auth_type)
    .bind(auth_token)
    .bind(&extra_headers)
    .bind(timeout_ms)
    .execute(db)
    .await?;

    Ok(McpServerInfo {
        id: Some(id),
        name: req.name.clone(),
        transport: req.transport.clone(),
        command: req.command.clone(),
        args: req.args.clone().unwrap_or_default(),
        env: redact_mcp_env(&env),
        url: req.url.clone(),
        enabled: req.enabled,
        tools_count: 0,
        connection_status: McpConnectionStatus::Disconnected,
        status: status.to_string(),
        last_error: None,
        created_at: Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
        updated_at: Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
        auth: McpServerAuthInfo {
            auth_type: auth_type.to_string(),
            has_token: !auth_token.is_empty(),
            extra_headers: req.extra_headers.clone(),
            timeout_ms: if timeout_ms == 60000 {
                None
            } else {
                Some(timeout_ms)
            },
        },
        tools: None,
        resources: None,
        prompts: None,
    })
}

async fn db_delete_server(db: &sqlx::SqlitePool, tenant_id: &str, name: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM mcp_server_registry WHERE tenant_id = ? AND name = ?")
        .bind(tenant_id)
        .bind(name)
        .execute(db)
        .await?;

    Ok(result.rows_affected() > 0)
}

async fn db_toggle_server(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    name: &str,
    enabled: bool,
) -> Result<Option<McpServerInfo>> {
    sqlx::query(
        "UPDATE mcp_server_registry
         SET enabled = ?,
             status = CASE
               WHEN ? = 0 THEN 'disabled'
               WHEN transport = 'stdio' THEN 'queued'
               ELSE 'configured'
             END,
             connection_status = 'disconnected', last_error = NULL,
             revision = revision + 1, lease_expires_at = NULL,
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND name = ?",
    )
    .bind(enabled)
    .bind(enabled)
    .bind(tenant_id)
    .bind(name)
    .execute(db)
    .await?;

    db_get_server(db, tenant_id, name).await
}

async fn db_update_server(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    name: &str,
    req: &UpdateMcpServerRequest,
) -> Result<Option<McpServerInfo>> {
    // Fetch current server first
    let current = db_get_server(db, tenant_id, name).await?;
    let Some(current) = current else {
        return Ok(None);
    };

    // Fetch existing auth_token so we can preserve it when the field is not provided
    let existing_secrets: Option<(String, String)> = sqlx::query_as(
        "SELECT COALESCE(auth_token, ''), COALESCE(env, '') \
         FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
    )
    .bind(tenant_id)
    .bind(name)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let existing_token = existing_secrets
        .as_ref()
        .map(|(token, _)| token.clone())
        .unwrap_or_default();
    let existing_env = existing_secrets
        .as_ref()
        .map(|(_, env)| parse_mcp_env(env))
        .unwrap_or_default();

    let new_name = req.name.clone().unwrap_or(current.name.clone());
    let new_transport = req.transport.clone().unwrap_or(current.transport.clone());
    let new_command = req.command.clone().or_else(|| current.command.clone());
    let new_args = req.args.clone().unwrap_or(current.args.clone());
    let new_env = req
        .env
        .clone()
        .map(|env| merge_redacted_mcp_env(env, &existing_env))
        .unwrap_or(existing_env);
    let new_url = req.url.clone().or_else(|| current.url.clone());
    let new_enabled = req.enabled.unwrap_or(current.enabled);
    let new_auth_type = req
        .auth_type
        .clone()
        .unwrap_or_else(|| current.auth.auth_type.clone());
    // Preserve existing token if not provided or if explicitly cleared (empty string).
    // The frontend sends null/undefined when the field is untouched (preserve),
    // and sends "" when the field is explicitly cleared (also preserve).
    // Only update when the user has typed a non-empty value.
    let new_auth_token = match req.auth_token {
        Some(ref t) if !t.is_empty() => t.clone(),
        _ => existing_token,
    };
    let new_extra_headers = req
        .extra_headers
        .clone()
        .or_else(|| current.auth.extra_headers.clone());
    let new_timeout_ms: u32 = req.timeout_ms.or(current.auth.timeout_ms).unwrap_or(60000);
    let new_status = if !new_enabled {
        "disabled"
    } else if new_transport == "stdio" {
        "queued"
    } else {
        "configured"
    };

    if new_transport == "stdio" {
        validate_stdio_command(new_command.as_ref())?;
    }

    let args_json = serde_json::to_string(&new_args)?;
    let env_json = serde_json::to_string(&new_env)?;
    let extra_headers_json = new_extra_headers
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();

    sqlx::query(
        "
        UPDATE mcp_server_registry
        SET name = ?, transport = ?, command = ?, args = ?, env = ?, url = ?, enabled = ?,
            auth_type = ?, auth_token = ?, extra_headers = ?, timeout_ms = ?,
            status = ?, connection_status = 'disconnected', last_error = NULL,
            revision = revision + 1, lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
        WHERE tenant_id = ? AND name = ?
        ",
    )
    .bind(&new_name)
    .bind(&new_transport)
    .bind(&new_command)
    .bind(&args_json)
    .bind(&env_json)
    .bind(&new_url)
    .bind(new_enabled)
    .bind(&new_auth_type)
    .bind(&new_auth_token)
    .bind(&extra_headers_json)
    .bind(new_timeout_ms)
    .bind(new_status)
    .bind(tenant_id)
    .bind(name)
    .execute(db)
    .await?;

    db_get_server(db, tenant_id, &new_name).await
}

async fn db_server_revision(db: &sqlx::SqlitePool, tenant_id: &str, name: &str) -> Result<i64> {
    sqlx::query_scalar("SELECT revision FROM mcp_server_registry WHERE tenant_id = ? AND name = ?")
        .bind(tenant_id)
        .bind(name)
        .fetch_one(db)
        .await
        .map_err(Into::into)
}

// ── Route handlers ────────────────────────────────────────────────────────────

async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<McpListResponse>> {
    let offset = pagination.offset();
    let limit = pagination.limit();

    let servers = db_list_servers(&state.db, &claims.tenant_id, offset, limit).await?;
    let total = db_count_servers(&state.db, &claims.tenant_id).await?;

    Ok(Json(McpListResponse { servers, total }))
}

async fn stats(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<McpStats>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "
        SELECT transport, COUNT(*) as cnt
        FROM mcp_server_registry
        WHERE tenant_id = ?
        GROUP BY transport
        ",
    )
    .bind(&claims.tenant_id)
    .fetch_all(&state.db)
    .await?;

    let mut transport_dist = std::collections::HashMap::new();
    for (transport, count) in rows {
        transport_dist.insert(transport, count);
    }

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mcp_server_registry WHERE tenant_id = ?")
            .bind(&claims.tenant_id)
            .fetch_one(&state.db)
            .await?;

    let healthy: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mcp_server_registry WHERE tenant_id = ? AND status = 'healthy'",
    )
    .bind(&claims.tenant_id)
    .fetch_one(&state.db)
    .await?;

    let total_tools: i64 = 0; // tools_count is not persisted; runtime discovers tools dynamically

    Ok(Json(McpStats {
        total_servers: total,
        healthy_servers: healthy,
        total_tools,
        transport_distribution: transport_dist,
    }))
}

async fn add(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddMcpServerRequest>,
) -> Result<Json<McpServerInfo>> {
    if req.name.is_empty() {
        return Err(AppError::ValidationError("name is required".into()));
    }
    if req.transport != "stdio" && req.transport != "http" && req.transport != "sse" {
        return Err(AppError::ValidationError(
            "transport must be one of: stdio, http, sse".into(),
        ));
    }
    if req.transport == "stdio" {
        validate_stdio_command(req.command.as_ref())?;
        validate_stdio_working_directories(req.args.as_deref().unwrap_or_default())?;
    }

    // Check for duplicate name
    if db_get_server(&state.db, &claims.tenant_id, &req.name)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict(format!(
            "MCP server '{}' already exists",
            req.name
        )));
    }

    let server = db_insert_server(&state.db, &claims.tenant_id, &req).await?;

    // Hot-reload: update the tenant's McpServerManager so new sessions pick up the change immediately
    if server.transport != "stdio" {
        if let Some(manager) = &state.agent_manager {
            if let Err(e) = manager
                .reload_mcp_servers(&claims.tenant_id, &claims.sub)
                .await
            {
                tracing::warn!("failed to hot-reload MCP servers after add: {e}");
            }
        }
    }

    // Broadcast to all WebSocket clients
    system_events::broadcast_mcp_added(&claims.tenant_id, &server.name);

    // Background connection test for HTTP/SSE servers
    if server.transport == "http" || server.transport == "sse" {
        spawn_connection_test(state.clone(), claims.tenant_id, server.name.clone());
    } else if server.enabled {
        let revision = db_server_revision(&state.db, &claims.tenant_id, &server.name).await?;
        spawn_stdio_lifecycle(
            state.clone(),
            claims.tenant_id,
            server.name.clone(),
            revision,
            claims.sub,
        );
    }

    Ok(Json(server))
}

async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpServerInfo>> {
    let server = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;

    Ok(Json(server))
}

async fn remove(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let deleted = db_delete_server(&state.db, &claims.tenant_id, &name).await?;

    if !deleted {
        return Err(AppError::NotFound(format!("MCP server '{name}' not found")));
    }

    // Hot-reload: update the tenant's McpServerManager so new sessions pick up the change immediately
    if let Some(manager) = &state.agent_manager {
        if let Err(e) = manager
            .reload_mcp_servers(&claims.tenant_id, &claims.sub)
            .await
        {
            tracing::warn!("failed to hot-reload MCP servers after remove: {e}");
        }
    }

    // Broadcast to all WebSocket clients
    system_events::broadcast_mcp_removed(&claims.tenant_id, &name);

    Ok(Json(serde_json::json!({ "removed": true })))
}

async fn toggle(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
    Json(req): Json<ToggleMcpServerRequest>,
) -> Result<Json<McpServerInfo>> {
    if req.enabled {
        let current = db_get_server(&state.db, &claims.tenant_id, &name)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;
        if current.transport == "stdio" {
            validate_stdio_command(current.command.as_ref())?;
            validate_stdio_working_directories(&current.args)?;
        }
    }
    let server = db_toggle_server(&state.db, &claims.tenant_id, &name, req.enabled)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;

    // Hot-reload: update the tenant's McpServerManager so new sessions pick up the change immediately
    if server.transport != "stdio" || !server.enabled {
        if let Some(manager) = &state.agent_manager {
            if let Err(e) = manager
                .reload_mcp_servers(&claims.tenant_id, &claims.sub)
                .await
            {
                tracing::warn!("failed to hot-reload MCP servers after toggle: {e}");
            }
        }
    }

    // Broadcast to all WebSocket clients
    system_events::broadcast_mcp_toggled(&claims.tenant_id, &server.name, server.enabled);

    // Background connection test when enabling
    if server.enabled && (server.transport == "http" || server.transport == "sse") {
        spawn_connection_test(state.clone(), claims.tenant_id, server.name.clone());
    } else if server.enabled && server.transport == "stdio" {
        let revision = db_server_revision(&state.db, &claims.tenant_id, &server.name).await?;
        spawn_stdio_lifecycle(
            state.clone(),
            claims.tenant_id,
            server.name.clone(),
            revision,
            claims.sub,
        );
    }

    Ok(Json(server))
}

async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
    Json(req): Json<UpdateMcpServerRequest>,
) -> Result<Json<McpServerInfo>> {
    // Validate transport if provided
    if let Some(ref transport) = req.transport {
        if transport != "stdio" && transport != "http" && transport != "sse" {
            return Err(AppError::ValidationError(
                "transport must be one of: stdio, http, sse".into(),
            ));
        }
    }

    let current = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;
    let effective_transport = req
        .transport
        .as_deref()
        .unwrap_or(current.transport.as_str());
    if effective_transport == "stdio" {
        let effective_command = req.command.as_ref().or(current.command.as_ref());
        let effective_args = req.args.as_deref().unwrap_or(&current.args);
        validate_stdio_command(effective_command)?;
        validate_stdio_working_directories(effective_args)?;
    }

    // Check name uniqueness if changing
    #[allow(clippy::collapsible_if)]
    if let Some(ref new_name) = req.name {
        if new_name != &name
            && db_get_server(&state.db, &claims.tenant_id, new_name)
                .await?
                .is_some()
        {
            return Err(AppError::Conflict(format!(
                "MCP server '{new_name}' already exists"
            )));
        }
    }

    let server = db_update_server(&state.db, &claims.tenant_id, &name, &req)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;

    // Hot-reload: update the tenant's McpServerManager so new sessions pick up the change immediately
    if server.transport != "stdio" || !server.enabled {
        if let Some(manager) = &state.agent_manager {
            if let Err(e) = manager
                .reload_mcp_servers(&claims.tenant_id, &claims.sub)
                .await
            {
                tracing::warn!("failed to hot-reload MCP servers after update: {e}");
            }
        }
    }

    // Broadcast to all WebSocket clients
    system_events::broadcast_mcp_updated(&claims.tenant_id, &server.name);

    // Background connection test for HTTP/SSE servers
    if server.transport == "http" || server.transport == "sse" {
        spawn_connection_test(state.clone(), claims.tenant_id, server.name.clone());
    } else if server.enabled {
        let revision = db_server_revision(&state.db, &claims.tenant_id, &server.name).await?;
        spawn_stdio_lifecycle(
            state.clone(),
            claims.tenant_id,
            server.name.clone(),
            revision,
            claims.sub,
        );
    }

    Ok(Json(server))
}

async fn retry_stdio_lifecycle(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpServerInfo>> {
    let server = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;
    if server.transport != "stdio" {
        return Err(AppError::ValidationError(
            "retry is only available for stdio MCP servers".to_string(),
        ));
    }
    if !server.enabled {
        return Err(AppError::ValidationError(
            "enable the stdio MCP server before retrying".to_string(),
        ));
    }
    validate_stdio_command(server.command.as_ref())?;
    validate_stdio_working_directories(&server.args)?;
    sqlx::query(
        "UPDATE mcp_server_registry
         SET status = 'queued', connection_status = 'disconnected', last_error = NULL,
             revision = revision + 1, lease_expires_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND name = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&name)
    .execute(&state.db)
    .await?;
    let revision = db_server_revision(&state.db, &claims.tenant_id, &name).await?;
    let refreshed = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;
    system_events::broadcast_mcp_status_changed(&claims.tenant_id, &name, "queued", None);
    spawn_stdio_lifecycle(state, claims.tenant_id, name, revision, claims.sub);
    Ok(Json(refreshed))
}

/// Test an MCP server by performing the full MCP handshake (initialize + tools/list).
/// Returns the number of tools available.
#[allow(clippy::too_many_lines)]
async fn test(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpTestResult>> {
    let server = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;

    // Only test enabled servers.
    if !server.enabled {
        return Ok(Json(McpTestResult {
            success: false,
            tools_count: 0,
            error: Some("server is disabled".to_string()),
        }));
    }

    match server.transport.as_str() {
        "stdio" => {
            validate_stdio_command(server.command.as_ref())?;
            validate_stdio_working_directories(&server.args)?;
            let stored_env: String = sqlx::query_scalar(
                "SELECT COALESCE(env, '') FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&name)
            .fetch_one(&state.db)
            .await?;
            let stored_env = parse_mcp_env(&stored_env);
            let result = probe_stdio_server(
                &name,
                server.command.as_deref().unwrap_or_default(),
                &server.args,
                &stored_env,
                server.auth.timeout_ms.unwrap_or(60_000),
            )
            .await;
            let (success, tools_count, error) = match result {
                Ok(tools) => (true, u32::try_from(tools.len()).unwrap_or(u32::MAX), None),
                Err(error) => (false, 0, Some(error)),
            };
            sqlx::query(
                "UPDATE mcp_server_registry SET connection_status = ?, status = ?, last_error = ? WHERE tenant_id = ? AND name = ?",
            )
            .bind(if success { "connected" } else { "error" })
            .bind(if success { "healthy" } else { "unhealthy" })
            .bind(&error)
            .bind(&claims.tenant_id)
            .bind(&name)
            .execute(&state.db)
            .await?;
            Ok(Json(McpTestResult {
                success,
                tools_count,
                error,
            }))
        }
        "http" | "sse" => {
            let url = server.url.clone().ok_or_else(|| {
                AppError::ValidationError("URL is required for HTTP/SSE transports".into())
            })?;

            let raw_token: Option<String> = sqlx::query_scalar(
                "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&name)
            .fetch_optional(&state.db)
            .await?;

            #[allow(clippy::cast_lossless)]
            let timeout_ms = u64::from(server.auth.timeout_ms.unwrap_or(60000));

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .map_err(|e| AppError::Internal(format!("HTTP client error: {e}")))?;

            // ── Step 1: Build request helpers ───────────────────────────────────

            #[derive(Default)]
            #[allow(clippy::items_after_statements)]
            struct ResponseParts {
                body: String,
                session_id: Option<String>,
            }

            fn safe_external_detail(value: &str, max_chars: usize) -> String {
                let protected = runtime::protect_sensitive_text(
                    value,
                    runtime::configured_data_protection_mode(),
                );
                let mut output = protected.value.chars().take(max_chars).collect::<String>();
                if protected.value.chars().count() > max_chars {
                    output.push_str("...");
                }
                output
            }

            /// Sends a JSON-RPC request and returns (body text, mcp-session-id if present).
            async fn send_jsonrpc(
                client: &reqwest::Client,
                url: &str,
                method: &str,
                params: serde_json::Value,
                bearer_token: Option<&str>,
                extra_headers: Option<&serde_json::Value>,
                session_id: Option<&str>,
            ) -> std::result::Result<ResponseParts, String> {
                let body = if params.is_null() {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": method
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": method,
                        "params": params
                    })
                };

                let mut req_builder = client
                    .post(url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream")
                    .json(&body);

                if let Some(token) = bearer_token {
                    req_builder = req_builder.header("Authorization", format!("Bearer {token}"));
                }

                if let Some(headers) = extra_headers {
                    if let Some(obj) = headers.as_object() {
                        for (k, v) in obj {
                            if let Some(val) = v.as_str() {
                                req_builder = req_builder.header(k.as_str(), val);
                            }
                        }
                    }
                }

                if let Some(sid) = session_id {
                    req_builder = req_builder.header("Mcp-Session-Id", sid);
                }

                let safe_url = safe_external_detail(url, 500);
                debug!(method = %method, url = %safe_url, body_chars = body.to_string().chars().count(),
                    has_bearer = bearer_token.is_some(), extra_headers_present = extra_headers.is_some(),
                    session_id_present = session_id.is_some(),
                    "MCP sending JSON-RPC request");

                let resp = req_builder
                    .send()
                    .await
                    .map_err(|e| safe_external_detail(&e.to_string(), 500))?;

                let content_type: String = resp
                    .headers()
                    .get("Content-Type")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .unwrap_or_default();
                let status_code = resp.status().as_u16();

                debug!(method = %method, status = status_code,
                    "MCP HTTP response");

                // Extract session ID before consuming resp with .text()
                let session_id: Option<String> = resp
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);

                let body_text = resp.text().await.map_err(|e| e.to_string())?;

                // Non-2xx: surface the raw body as error so callers can see the error details
                if !(200..300).contains(&status_code) {
                    return Err(format!(
                        "HTTP {status_code}: {}",
                        safe_external_detail(&body_text, 500)
                    ));
                }

                // If server responds with SSE, extract JSON from data: lines
                if content_type.contains("text/event-stream") {
                    for line in body_text.lines() {
                        let trimmed = line.trim();
                        if let Some(stripped) = trimmed.strip_prefix("data:") {
                            let json_str = stripped.trim();
                            if !json_str.is_empty() && json_str != "[DONE]" {
                                return Ok(ResponseParts {
                                    body: json_str.to_string(),
                                    session_id,
                                });
                            }
                        }
                    }
                    if body_text.trim().is_empty() {
                        return Err("SSE response empty".to_string());
                    }
                    return Err(format!(
                        "SSE response without JSON data: {}",
                        safe_external_detail(&body_text, 200)
                    ));
                }

                Ok(ResponseParts {
                    body: body_text,
                    session_id,
                })
            }

            /// Parses a JSON-RPC response and extracts the tools list.
            /// Returns `Ok(Some(tools))` on success, `Ok(None)` if no tools array found,
            /// `Err(message)` if the server returned a JSON-RPC error.
            fn parse_tools(
                body: &str,
            ) -> std::result::Result<Option<Vec<serde_json::Value>>, String> {
                let v = match serde_json::from_str::<serde_json::Value>(body) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("failed to parse JSON-RPC response: {e}")),
                };

                debug!(body_chars = body.chars().count(), "MCP tools/list response");

                if let Some(err_obj) = v.get("error") {
                    let msg = err_obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error");
                    let code = err_obj
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    let data = err_obj
                        .get("data")
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    return Err(safe_external_detail(
                        &format!(
                            "server error {code}: {msg}{}",
                            if data.is_empty() {
                                String::new()
                            } else {
                                format!(" ({data})")
                            }
                        ),
                        500,
                    ));
                }

                let tools = v
                    .get("result")
                    .and_then(|r| r.get("tools"))
                    .and_then(serde_json::Value::as_array)
                    .map(|arr| arr.clone());

                Ok(tools)
            }

            /// Parses a JSON-RPC response and extracts the tools count.
            fn parse_tools_count(body: &str) -> std::result::Result<u32, String> {
                parse_tools(body)
                    .map(|opt| opt.map_or(0, |arr| u32::try_from(arr.len()).unwrap_or(0)))
            }

            /// Parses a JSON-RPC error message from response body.
            fn parse_rpc_error(body: &str) -> Option<String> {
                serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        let msg = v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())?;
                        Some(msg.to_string())
                    })
            }

            let bearer_token = (server.auth.auth_type == "bearer_token")
                .then_some(raw_token.as_deref())
                .flatten();
            let extra_headers = server.auth.extra_headers.as_ref();

            // ── Step 2: Initialize handshake ──────────────────────────────────────
            let init_body = serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "aos-web", "version": "1.0.0" }
            });

            type InnerResult<T> = std::result::Result<T, String>;

            let init_parts: InnerResult<ResponseParts> = send_jsonrpc(
                &client,
                &url,
                "initialize",
                init_body,
                bearer_token,
                extra_headers,
                None,
            )
            .await;

            // Capture session ID from initialize response (used by stateful servers like ModelScope).
            let session_id: Option<String> = init_parts
                .as_ref()
                .ok()
                .and_then(|parts| parts.session_id.clone());

            let init_error = match &init_parts {
                Ok(parts) => {
                    debug!(server = %name, response_chars = parts.body.chars().count(),
                        "MCP initialize response");
                    parse_rpc_error(&parts.body)
                }
                Err(e) => Some(e.clone()),
            };

            if let Some(err) = init_error {
                sqlx::query("UPDATE mcp_server_registry SET connection_status = 'error', status = 'unhealthy', last_error = ? WHERE tenant_id = ? AND name = ?")
                    .bind(&err)
                    .bind(&claims.tenant_id)
                    .bind(&name)
                    .execute(&state.db)
                    .await
                    .ok();

                return Ok(Json(McpTestResult {
                    success: false,
                    tools_count: 0,
                    error: Some(format!("initialize failed: {err}")),
                }));
            }

            // ── Step 3: Send notifications/initialized (required by MCP spec to complete handshake) ──
            let _: InnerResult<ResponseParts> = send_jsonrpc(
                &client,
                &url,
                "notifications/initialized",
                serde_json::json!({}),
                bearer_token,
                extra_headers,
                session_id.as_deref(),
            )
            .await;

            // ── Step 4: tools/list ──────────────────────────────────────────────────
            let tools_parts: InnerResult<ResponseParts> = send_jsonrpc(
                &client,
                &url,
                "tools/list",
                serde_json::json!(null),
                bearer_token,
                extra_headers,
                session_id.as_deref(),
            )
            .await;

            match tools_parts {
                Ok(parts) => {
                    match parse_tools_count(&parts.body) {
                        Ok(tools_count) => {
                            debug!(server = %name, tools_count, response_chars = parts.body.chars().count(),
                                "MCP test_connection: tools/list succeeded");
                            if tools_count == 0 {
                                warn!(server = %name,
                                    "MCP server responded successfully but reported 0 tools");
                            }

                            sqlx::query("UPDATE mcp_server_registry SET connection_status = 'connected', status = 'healthy', last_error = NULL WHERE tenant_id = ? AND name = ?")
                                .bind(&claims.tenant_id)
                                .bind(&name)
                                .execute(&state.db)
                                .await
                                .ok();

                            Ok(Json(McpTestResult {
                                success: true,
                                tools_count,
                                error: None,
                            }))
                        }
                        Err(msg) => {
                            // -32602 from tools/list means the server is reachable (initialize
                            // succeeded) but doesn't support the tools/list method (common for
                            // inference-only endpoints). Treat this as a successful connection.
                            let msg_str: String = msg;
                            let is_meta_method_unsupported = msg_str.contains("-32602");
                            if is_meta_method_unsupported {
                                debug!(server = %name, "MCP server doesn't support tools/list method");
                            } else {
                                let safe_error = runtime::protect_sensitive_text(
                                    &msg_str,
                                    runtime::configured_data_protection_mode(),
                                );
                                warn!(
                                    server = %name,
                                    error_chars = safe_error.value.chars().count(),
                                    redacted = safe_error.report.redacted,
                                    "MCP server returned a JSON-RPC error"
                                );
                            }

                            sqlx::query("UPDATE mcp_server_registry SET connection_status = 'connected', status = 'healthy', last_error = NULL WHERE tenant_id = ? AND name = ?")
                                .bind(&claims.tenant_id)
                                .bind(&name)
                                .execute(&state.db)
                                .await
                                .ok();

                            Ok(Json(McpTestResult {
                                success: true,
                                tools_count: 0,
                                error: if is_meta_method_unsupported {
                                    None
                                } else {
                                    Some(msg_str)
                                },
                            }))
                        }
                    }
                }
                Err(e) => {
                    // HTTP 400 with session-id error means the server rejected tools/list because
                    // it doesn't support that method. This is not a connection failure.
                    let is_session_error = e.contains("HTTP 400") && e.contains("mcp-session-id");
                    if is_session_error {
                        debug!(server = %name, "MCP server rejected tools/list (session requirement)");
                        sqlx::query("UPDATE mcp_server_registry SET connection_status = 'connected', status = 'healthy', last_error = NULL WHERE tenant_id = ? AND name = ?")
                            .bind(&claims.tenant_id)
                            .bind(&name)
                            .execute(&state.db)
                            .await
                            .ok();

                        Ok(Json(McpTestResult {
                            success: true,
                            tools_count: 0,
                            error: None,
                        }))
                    } else {
                        warn!(server = %name, "MCP tools/list request failed: {e}");
                        sqlx::query("UPDATE mcp_server_registry SET connection_status = 'error', status = 'unhealthy', last_error = ? WHERE tenant_id = ? AND name = ?")
                            .bind(&e)
                            .bind(&claims.tenant_id)
                            .bind(&name)
                            .execute(&state.db)
                            .await
                            .ok();

                        Ok(Json(McpTestResult {
                            success: false,
                            tools_count: 0,
                            error: Some(e),
                        }))
                    }
                }
            }
        }
        _ => Ok(Json(McpTestResult {
            success: false,
            tools_count: 0,
            error: Some(format!("Unsupported transport: {}", server.transport)),
        })),
    }
}

/// Spawns a background task that tests an MCP server's connection and updates DB + broadcasts result.
#[allow(clippy::single_match_else)]
fn spawn_connection_test(state: AppState, tenant_id: String, name: String) {
    tokio::spawn(async move {
        let (url, auth_type, auth_token, extra_headers) = {
            let server = match db_get_server(&state.db, &tenant_id, &name).await {
                Ok(Some(s)) => s,
                Ok(None) => {
                    tracing::debug!(tenant = %tenant_id, server = %name, "server gone before test");
                    return;
                }
                Err(e) => {
                    tracing::warn!(tenant = %tenant_id, server = %name, "failed to fetch server for test: {e}");
                    return;
                }
            };

            if !server.enabled {
                tracing::debug!(tenant = %tenant_id, server = %name, "skipping test for disabled server");
                return;
            }

            let url = match server.url {
                Some(u) => u,
                None => {
                    tracing::debug!(tenant = %tenant_id, server = %name, "no URL, skipping test");
                    return;
                }
            };

            let raw_token: Option<String> = sqlx::query_scalar(
                "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
            )
            .bind(&tenant_id)
            .bind(&name)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

            (
                url,
                server.auth.auth_type,
                raw_token,
                server.auth.extra_headers,
            )
        };

        let timeout_ms = 5000u64;
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(tenant = %tenant_id, server = %name, "reqwest client error: {e}");
                return;
            }
        };

        let bearer_token = (auth_type == "bearer_token")
            .then_some(auth_token.as_deref())
            .flatten();

        let start = std::time::Instant::now();
        let result = http_jsonrpc_call(
            &client,
            &url,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "aos-web", "version": "1.0.0" }
            }),
            bearer_token,
            extra_headers.as_ref(),
            None,
            false,
        )
        .await;
        let latency_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

        let (new_status, error_msg) = match result {
            Ok((body_text, _session_id)) => {
                if let Some(err) = parse_rpc_error(&body_text) {
                    ("error".to_string(), Some(err))
                } else {
                    ("connected".to_string(), None)
                }
            }
            Err(e) => ("error".to_string(), Some(e)),
        };
        let runtime_status = if new_status == "connected" {
            "healthy"
        } else {
            "unhealthy"
        };

        if let Err(e) = sqlx::query(
            "UPDATE mcp_server_registry SET connection_status = ?, status = ?, last_error = ? WHERE tenant_id = ? AND name = ?",
        )
        .bind(&new_status)
        .bind(runtime_status)
        .bind(&error_msg)
        .bind(&tenant_id)
        .bind(&name)
        .execute(&state.db)
        .await
        {
            tracing::warn!(tenant = %tenant_id, server = %name, "failed to update connection_status: {e}");
        }

        system_events::broadcast_mcp_status_changed(
            &tenant_id,
            &name,
            &new_status,
            error_msg.as_deref(),
        );
        tracing::debug!(tenant = %tenant_id, server = %name, latency_ms, status = %new_status, "background connection test done");
    });
}

/// Ping an MCP server by performing a full JSON-RPC `initialize` handshake
/// and return latency + connection status.
async fn test_connection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpLatencyResult>> {
    let server = db_get_server(&state.db, &claims.tenant_id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;

    if server.transport == "stdio" {
        let started = std::time::Instant::now();
        let validation = validate_stdio_command(server.command.as_ref())
            .and_then(|()| validate_stdio_working_directories(&server.args));
        let result = if let Err(error) = validation {
            Err(error.to_string())
        } else {
            let stored_env: String = sqlx::query_scalar(
                "SELECT COALESCE(env, '') FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
            )
            .bind(&claims.tenant_id)
            .bind(&name)
            .fetch_one(&state.db)
            .await?;
            let stored_env = parse_mcp_env(&stored_env);
            probe_stdio_server(
                &name,
                server.command.as_deref().unwrap_or_default(),
                &server.args,
                &stored_env,
                server.auth.timeout_ms.unwrap_or(60_000),
            )
            .await
            .map(|_| ())
        };
        let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let (success, error) = match result {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error)),
        };
        sqlx::query(
            "UPDATE mcp_server_registry SET connection_status = ?, status = ?, last_error = ? WHERE tenant_id = ? AND name = ?",
        )
        .bind(if success { "connected" } else { "error" })
        .bind(if success { "healthy" } else { "unhealthy" })
        .bind(&error)
        .bind(&claims.tenant_id)
        .bind(&name)
        .execute(&state.db)
        .await?;
        system_events::broadcast_mcp_status_changed(
            &claims.tenant_id,
            &name,
            if success { "connected" } else { "error" },
            error.as_deref(),
        );
        return Ok(Json(McpLatencyResult {
            success,
            latency_ms,
            error,
        }));
    }

    let url = server.url.clone().ok_or_else(|| {
        AppError::ValidationError("URL is required for HTTP/SSE transports".into())
    })?;

    let raw_token: Option<String> = sqlx::query_scalar(
        "SELECT auth_token FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
    )
    .bind(&claims.tenant_id)
    .bind(&name)
    .fetch_optional(&state.db)
    .await?;

    let timeout_ms = u64::from(server.auth.timeout_ms.unwrap_or(60000));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| AppError::Internal(format!("HTTP client error: {e}")))?;

    let bearer_token = (server.auth.auth_type == "bearer_token")
        .then_some(raw_token.as_deref())
        .flatten();
    let extra_headers = server.auth.extra_headers.as_ref();

    let start = std::time::Instant::now();
    let resp = http_jsonrpc_call(
        &client,
        &url,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aos-web", "version": "1.0.0" }
        }),
        bearer_token,
        extra_headers,
        None,
        false,
    )
    .await;
    let latency_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

    match resp {
        Ok((body_text, _session_id)) => {
            let (new_status, runtime_status, error_msg) =
                if let Some(err) = parse_rpc_error(&body_text) {
                    ("error".to_string(), "unhealthy", Some(err))
                } else {
                    ("connected".to_string(), "healthy", None)
                };

            // Update connection_status in DB and broadcast
            sqlx::query(
                "UPDATE mcp_server_registry SET connection_status = ?, status = ?, last_error = ? WHERE tenant_id = ? AND name = ?",
            )
            .bind(&new_status)
            .bind(runtime_status)
            .bind(&error_msg)
            .bind(&claims.tenant_id)
            .bind(&name)
            .execute(&state.db)
            .await
            .ok();

            system_events::broadcast_mcp_status_changed(
                &claims.tenant_id,
                &name,
                &new_status,
                error_msg.as_deref(),
            );

            Ok(Json(McpLatencyResult {
                success: new_status == "connected",
                latency_ms,
                error: error_msg,
            }))
        }
        Err(e) => {
            let err_str = e;
            sqlx::query(
                "UPDATE mcp_server_registry SET connection_status = 'error', status = 'unhealthy', last_error = ? WHERE tenant_id = ? AND name = ?",
            )
            .bind(&err_str)
            .bind(&claims.tenant_id)
            .bind(&name)
            .execute(&state.db)
            .await
            .ok();

            system_events::broadcast_mcp_status_changed(
                &claims.tenant_id,
                &name,
                "error",
                Some(&err_str),
            );

            Ok(Json(McpLatencyResult {
                success: false,
                latency_ms,
                error: Some(err_str),
            }))
        }
    }
}

/// GET /api/v1/mcp/{name}/tools — list tools exposed by an MCP server.
async fn list_tools(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpToolsResponse>> {
    let server = fetch_mcp_server(&state, &name, &claims.tenant_id).await?;

    // stdio discovery is owned by the asynchronous lifecycle worker. Starting
    // another uvx/npx process here races cold installation and used to turn the
    // real startup error into a misleading 60-second GET timeout. Return the
    // lifecycle worker's persisted snapshot instead; retry/test endpoints can
    // explicitly launch a fresh probe when requested by the user.
    if server.transport == "stdio" {
        let tools = server.tools.unwrap_or_default();
        return Ok(Json(McpToolsResponse {
            count: u32::try_from(tools.len()).unwrap_or(u32::MAX),
            tools,
        }));
    }

    // For HTTP/SSE, discover tools live via the MCP HTTP handshake.
    if server.transport == "http" || server.transport == "sse" {
        match discover_mcp_tools(&state, &claims.tenant_id, &name, &server).await {
            Ok(tools) => {
                let count = u32::try_from(tools.len()).unwrap_or(u32::MAX);
                return Ok(Json(McpToolsResponse { tools, count }));
            }
            Err(e) => {
                debug!(server = %name, error = %e, "list_tools: MCP discovery failed");
                return Ok(Json(McpToolsResponse {
                    tools: Vec::new(),
                    count: 0,
                }));
            }
        }
    }

    Ok(Json(McpToolsResponse {
        tools: server.tools.clone().unwrap_or_default(),
        count: u32::try_from(server.tools.as_ref().map_or(0, |t| t.len())).unwrap_or(u32::MAX),
    }))
}

/// GET /api/v1/mcp/{name}/resources — list resources exposed by an MCP server.
async fn list_resources(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpResourcesResponse>> {
    let server = fetch_mcp_server(&state, &name, &claims.tenant_id).await?;

    // For stdio transport, return empty (requires CLI runtime).
    if server.transport == "stdio" {
        return Ok(Json(McpResourcesResponse {
            resources: Vec::new(),
            count: 0,
        }));
    }

    // For HTTP/SSE, discover resources live via the MCP HTTP handshake.
    if server.transport == "http" || server.transport == "sse" {
        match discover_mcp_resources(&state, &claims.tenant_id, &name, &server).await {
            Ok(resources) => {
                let count = u32::try_from(resources.len()).unwrap_or(u32::MAX);
                return Ok(Json(McpResourcesResponse { resources, count }));
            }
            Err(e) => {
                debug!(server = %name, error = %e, "list_resources: MCP discovery failed");
                return Ok(Json(McpResourcesResponse {
                    resources: Vec::new(),
                    count: 0,
                }));
            }
        }
    }

    Ok(Json(McpResourcesResponse {
        resources: server.resources.clone().unwrap_or_default(),
        count: u32::try_from(server.resources.as_ref().map_or(0, |r| r.len())).unwrap_or(u32::MAX),
    }))
}

/// GET /api/v1/mcp/{name}/prompts — list prompts exposed by an MCP server.
async fn list_prompts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> Result<Json<McpPromptsResponse>> {
    let server = fetch_mcp_server(&state, &name, &claims.tenant_id).await?;

    // For stdio transport, return empty (requires CLI runtime).
    if server.transport == "stdio" {
        return Ok(Json(McpPromptsResponse {
            prompts: Vec::new(),
            count: 0,
        }));
    }

    // For HTTP/SSE, discover prompts live via the MCP HTTP handshake.
    if server.transport == "http" || server.transport == "sse" {
        match discover_mcp_prompts(&state, &claims.tenant_id, &name, &server).await {
            Ok(prompts) => {
                let count = u32::try_from(prompts.len()).unwrap_or(u32::MAX);
                return Ok(Json(McpPromptsResponse { prompts, count }));
            }
            Err(e) => {
                debug!(server = %name, error = %e, "list_prompts: MCP discovery failed");
                return Ok(Json(McpPromptsResponse {
                    prompts: Vec::new(),
                    count: 0,
                }));
            }
        }
    }

    Ok(Json(McpPromptsResponse {
        prompts: server.prompts.clone().unwrap_or_default(),
        count: u32::try_from(server.prompts.as_ref().map_or(0, |p| p.len())).unwrap_or(u32::MAX),
    }))
}

async fn fetch_mcp_server(state: &AppState, name: &str, tenant_id: &str) -> Result<McpServerInfo> {
    let server = db_get_server(&state.db, tenant_id, name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("MCP server '{name}' not found")))?;
    Ok(server)
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", routing_get(list))
        .route("/stats", routing_get(stats))
        .route("/", routing_post(add))
        .route("/{name}", routing_get(get))
        .route("/{name}", routing_delete(remove))
        .route("/{name}/toggle", routing_patch(toggle))
        .route("/{name}/retry", routing_post(retry_stdio_lifecycle))
        .route("/{name}", routing_patch(update))
        .route("/{name}/test", routing_post(test))
        .route("/{name}/test-connection", routing_post(test_connection))
        .route("/{name}/tools", routing_get(list_tools))
        .route("/{name}/resources", routing_get(list_resources))
        .route("/{name}/prompts", routing_get(list_prompts))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

/// Starts a background task that periodically tests all enabled MCP servers.
/// Interval is controlled by the `MCP_CHECK_INTERVAL_SECS` env var (default: 300s).
pub fn start_periodic_mcp_checker(state: crate::state::AppState) {
    let recovery_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE mcp_server_registry
             SET status = 'queued', connection_status = 'disconnected',
                 revision = revision + 1, lease_expires_at = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE enabled = 1 AND transport = 'stdio'
               AND status IN ('installing', 'starting', 'discovering')",
        )
        .execute(&recovery_state.db)
        .await
        {
            tracing::warn!(%error, "failed to recover interrupted stdio MCP lifecycle records");
            return;
        }
        let queued = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT tenant_id, name, revision FROM mcp_server_registry
             WHERE enabled = 1 AND transport = 'stdio' AND status = 'queued'",
        )
        .fetch_all(&recovery_state.db)
        .await;
        match queued {
            Ok(rows) => {
                for (tenant_id, name, revision) in rows {
                    spawn_stdio_lifecycle(
                        recovery_state.clone(),
                        tenant_id,
                        name,
                        revision,
                        "system".to_string(),
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to load queued stdio MCP lifecycle records");
            }
        }
    });

    let interval_secs: u64 = std::env::var("MCP_CHECK_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    if interval_secs == 0 {
        tracing::warn!("MCP_CHECK_INTERVAL_SECS=0, periodic MCP checker disabled");
        return;
    }

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // skip the immediate first tick during server startup
        loop {
            interval.tick().await;

            let servers: Vec<(String, String)> = match sqlx::query_as::<_, (String, String)>(
                "SELECT tenant_id, name FROM mcp_server_registry WHERE enabled = 1 AND transport IN ('http', 'sse') AND url IS NOT NULL AND url != ''",
            )
            .fetch_all(&state.db)
            .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("periodic MCP check: failed to fetch servers: {e}");
                    continue;
                }
            };

            if servers.is_empty() {
                continue;
            }

            tracing::debug!("periodic MCP check: testing {} servers", servers.len());

            for (tenant_id, name) in servers {
                spawn_connection_test(state.clone(), tenant_id, name);
            }
        }
    });

    tracing::info!("periodic MCP checker started (interval: {interval_secs}s)");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mcp_test_db() -> sqlx::SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        sqlx::query(
            "CREATE TABLE mcp_server_registry (\
             id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, name TEXT NOT NULL, \
             transport TEXT NOT NULL, auth_type TEXT, auth_token TEXT, extra_headers TEXT, \
             timeout_ms INTEGER, oauth_config TEXT, command TEXT, args TEXT, url TEXT, env TEXT, \
             enabled INTEGER NOT NULL, connection_status TEXT, status TEXT NOT NULL, last_error TEXT, \
             revision INTEGER NOT NULL DEFAULT 1, attempt_count INTEGER NOT NULL DEFAULT 0, \
             last_attempt_at TEXT, lease_expires_at TEXT, tools_json TEXT, \
             created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("MCP registry schema");
        db
    }

    #[test]
    fn stdio_env_is_redacted_and_preserved_during_edit() {
        let existing = BTreeMap::from([
            ("API_TOKEN".to_string(), "secret-value".to_string()),
            ("REGION".to_string(), "ap-southeast-1".to_string()),
        ]);
        let redacted = redact_mcp_env(&existing);
        assert_eq!(
            redacted.get("API_TOKEN").map(String::as_str),
            Some(REDACTED_MCP_ENV_VALUE)
        );

        let requested = BTreeMap::from([
            ("API_TOKEN".to_string(), REDACTED_MCP_ENV_VALUE.to_string()),
            ("REGION".to_string(), "eu-west-1".to_string()),
        ]);
        let merged = merge_redacted_mcp_env(requested, &existing);
        assert_eq!(
            merged.get("API_TOKEN").map(String::as_str),
            Some("secret-value")
        );
        assert_eq!(merged.get("REGION").map(String::as_str), Some("eu-west-1"));
    }

    #[test]
    fn stdio_install_status_is_based_only_on_the_executable_basename() {
        assert!(stdio_command_requires_install("uvx"));
        assert!(stdio_command_requires_install("uvx.exe"));
        assert!(stdio_command_requires_install("npx"));
        assert!(stdio_command_requires_install("npx.cmd"));
        assert!(!stdio_command_requires_install("uv"));
        assert!(!stdio_command_requires_install("node"));
        assert!(!stdio_command_requires_install("my-npx-wrapper"));
    }

    #[tokio::test]
    async fn stdio_env_round_trips_through_sqlite_without_exposing_values() {
        let db = mcp_test_db().await;
        let request = AddMcpServerRequest {
            name: "weather".to_string(),
            transport: "stdio".to_string(),
            command: Some("uv".to_string()),
            args: Some(vec!["run".to_string(), "weather.py".to_string()]),
            env: Some(BTreeMap::from([(
                "WEATHER_TOKEN".to_string(),
                "secret-value".to_string(),
            )])),
            url: None,
            enabled: true,
            auth_type: None,
            auth_token: None,
            extra_headers: None,
            timeout_ms: Some(60_000),
        };

        let inserted = db_insert_server(&db, "tenant-1", &request)
            .await
            .expect("insert stdio MCP server");
        assert_eq!(inserted.status, "queued");
        assert_eq!(
            inserted.env.get("WEATHER_TOKEN").map(String::as_str),
            Some(REDACTED_MCP_ENV_VALUE)
        );
        let stored_env: String = sqlx::query_scalar(
            "SELECT env FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
        )
        .bind("tenant-1")
        .bind("weather")
        .fetch_one(&db)
        .await
        .expect("stored MCP env");
        assert_eq!(
            parse_mcp_env(&stored_env)
                .get("WEATHER_TOKEN")
                .map(String::as_str),
            Some("secret-value")
        );

        let updated = db_update_server(
            &db,
            "tenant-1",
            "weather",
            &UpdateMcpServerRequest {
                name: None,
                transport: None,
                command: None,
                args: None,
                env: Some(BTreeMap::from([(
                    "WEATHER_TOKEN".to_string(),
                    REDACTED_MCP_ENV_VALUE.to_string(),
                )])),
                url: None,
                enabled: None,
                auth_type: None,
                auth_token: None,
                extra_headers: None,
                timeout_ms: None,
            },
        )
        .await
        .expect("update stdio MCP server")
        .expect("updated server");
        assert_eq!(
            updated.env.get("WEATHER_TOKEN").map(String::as_str),
            Some(REDACTED_MCP_ENV_VALUE)
        );
        let preserved_env: String = sqlx::query_scalar(
            "SELECT env FROM mcp_server_registry WHERE tenant_id = ? AND name = ?",
        )
        .bind("tenant-1")
        .bind("weather")
        .fetch_one(&db)
        .await
        .expect("preserved MCP env");
        assert_eq!(
            parse_mcp_env(&preserved_env)
                .get("WEATHER_TOKEN")
                .map(String::as_str),
            Some("secret-value")
        );
    }

    #[tokio::test]
    async fn stdio_revision_invalidates_old_lifecycle_workers_on_update_and_disable() {
        let db = mcp_test_db().await;
        let request = AddMcpServerRequest {
            name: "fetch".to_string(),
            transport: "stdio".to_string(),
            command: Some("uvx".to_string()),
            args: Some(vec!["mcp-server-fetch".to_string()]),
            env: None,
            url: None,
            enabled: true,
            auth_type: None,
            auth_token: None,
            extra_headers: None,
            timeout_ms: Some(60_000),
        };
        let inserted = db_insert_server(&db, "tenant-1", &request)
            .await
            .expect("insert stdio lifecycle fixture");
        assert_eq!(inserted.status, "queued");
        let revision_1 = db_server_revision(&db, "tenant-1", "fetch")
            .await
            .expect("load first revision");
        assert!(stdio_lifecycle_is_current(&db, "tenant-1", "fetch", revision_1).await);

        let updated = db_update_server(
            &db,
            "tenant-1",
            "fetch",
            &UpdateMcpServerRequest {
                name: None,
                transport: None,
                command: None,
                args: Some(vec!["mcp-server-fetch@latest".to_string()]),
                env: None,
                url: None,
                enabled: None,
                auth_type: None,
                auth_token: None,
                extra_headers: None,
                timeout_ms: None,
            },
        )
        .await
        .expect("update lifecycle fixture")
        .expect("updated lifecycle fixture");
        assert_eq!(updated.status, "queued");
        let revision_2 = db_server_revision(&db, "tenant-1", "fetch")
            .await
            .expect("load second revision");
        assert!(revision_2 > revision_1);
        assert!(!stdio_lifecycle_is_current(&db, "tenant-1", "fetch", revision_1).await);
        assert!(stdio_lifecycle_is_current(&db, "tenant-1", "fetch", revision_2).await);

        let disabled = db_toggle_server(&db, "tenant-1", "fetch", false)
            .await
            .expect("disable lifecycle fixture")
            .expect("disabled lifecycle fixture");
        assert_eq!(disabled.status, "disabled");
        let revision_3 = db_server_revision(&db, "tenant-1", "fetch")
            .await
            .expect("load disabled revision");
        assert!(revision_3 > revision_2);
        assert!(!stdio_lifecycle_is_current(&db, "tenant-1", "fetch", revision_2).await);
        assert!(!stdio_lifecycle_is_current(&db, "tenant-1", "fetch", revision_3).await);
    }
}
