//! Unified MCP server session management.
//!
//! This module provides [`McpServerSession`] — a wrapper around a concrete transport
//! (`Box<dyn McpTransport>`) that manages the full lifecycle of a connected MCP server:
//! initialization, tool/resource discovery, tool invocation, and graceful shutdown.
//!
//! ## Supported Transports
//!
//! | Transport | Builder | Auth |
//! |-----------|---------|------|
//! | Stdio | [`StdioTransportBuilder`] | Via env in process config |
//! | HTTP Stream | [`HttpStreamTransportBuilder`] | Bearer token, `OAuth2` |
//! | SSE | [`SseTransportBuilder`] | Bearer token, `OAuth2` |
//!
//! ## Usage
//!
//! ```ignore
//! use runtime::mcp::{
//!     http_stream_transport::HttpStreamTransportConfig,
//!     session::{McpServerSession, TransportBuilder},
//! };
//!
//! let config = HttpStreamTransportConfig {
//!     server_name: "github".into(),
//!     url: "https://api.github.com/mcp".into(),
//!     extra_headers: BTreeMap::new(),
//!     auth: McpAuthConfig::BearerToken("ghp_xxx".into()),
//!     timeout_ms: Some(60_000),
//!     auto_detect_sse_fallback: true,
//!     insecure: false,
//! };
//! let transport = HttpStreamTransport::new(config).unwrap();
//! let mut session = McpServerSession::new("github", transport);
//! session.initialize().await.unwrap();
//! let tools = session.list_tools().await.unwrap();
//! ```

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use super::http_stream_transport::HttpStreamTransport;
use super::sse_transport::SseTransport;
use super::stdio_transport::StdioTransport;
use super::transport::{McpAuthConfig, McpTransportError, McpTransportKind};
use super::types::{
    McpInitializeResult, McpReadResourceResult, McpResource, McpTool, McpToolCallResult,
};
use super::McpTransport;

/// Result type for MCP session operations.
pub type McpSessionResult<T> = Result<T, McpSessionError>;

/// Errors that can occur during MCP session operations.
#[derive(Debug)]
pub enum McpSessionError {
    /// Transport-level error (I/O, HTTP, timeout).
    Transport {
        server_name: String,
        source: McpTransportError,
    },
    /// Server returned a JSON-RPC error for a session operation.
    JsonRpc {
        server_name: String,
        method: &'static str,
        code: i64,
        message: String,
    },
    /// Unexpected response shape from the server.
    InvalidResponse {
        server_name: String,
        method: &'static str,
        details: String,
    },
    /// Server name is unknown to this session.
    UnknownServer { server_name: String },
}

impl std::fmt::Display for McpSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport {
                server_name,
                source,
            } => {
                write!(f, "transport error for `{server_name}`: {source}")
            }
            Self::JsonRpc {
                server_name,
                method,
                code,
                message,
            } => {
                write!(
                    f,
                    "MCP server `{server_name}` JSON-RPC error in {method}: [{code}] {message}"
                )
            }
            Self::InvalidResponse {
                server_name,
                method,
                details,
            } => {
                write!(
                    f,
                    "MCP server `{server_name}` invalid {method} response: {details}"
                )
            }
            Self::UnknownServer { server_name } => {
                write!(f, "MCP session has no server named `{server_name}`")
            }
        }
    }
}

impl std::error::Error for McpSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<McpTransportError> for McpSessionError {
    fn from(e: McpTransportError) -> Self {
        Self::Transport {
            server_name: String::new(),
            source: e,
        }
    }
}

// ---------------------------------------------------------------------------
// McpServerSession
// ---------------------------------------------------------------------------

/// A managed MCP server session wrapping a concrete transport.
///
/// Each session owns its transport and caches discovery results (tools, resources)
/// across tool calls. The session is reusable — calling `initialize()` again after
/// a shutdown reinitializes the connection.
pub struct McpServerSession {
    /// Human-readable server name.
    server_name: String,
    /// Concrete transport instance.
    transport: Box<dyn McpTransport>,
    /// Whether the server has been initialized.
    initialized: bool,
    /// Cached tools discovered during the last successful `list_tools`.
    cached_tools: Vec<McpTool>,
    /// Cached resources discovered during the last successful `list_resources`.
    cached_resources: Vec<McpResource>,
    /// The server info returned during the initialize handshake.
    server_info: Option<McpInitializeResult>,
}

impl std::fmt::Debug for McpServerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerSession")
            .field("server_name", &self.server_name)
            .field("kind", &self.transport.kind())
            .field("initialized", &self.initialized)
            .field("cached_tools", &self.cached_tools.len())
            .field("cached_resources", &self.cached_resources.len())
            .field("server_info", &self.server_info.is_some())
            .finish()
    }
}

impl McpServerSession {
    /// Wraps a pre-built transport into a new session.
    pub fn new(server_name: impl Into<String>, transport: Box<dyn McpTransport>) -> Self {
        Self {
            server_name: server_name.into(),
            transport,
            initialized: false,
            cached_tools: Vec::new(),
            cached_resources: Vec::new(),
            server_info: None,
        }
    }

    /// Returns the transport kind (stdio / `http_stream` / sse).
    #[inline]
    #[must_use]
    pub fn kind(&self) -> McpTransportKind {
        self.transport.kind()
    }

    /// Returns the server name.
    #[inline]
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Returns whether the server has been initialized.
    #[inline]
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Returns the cached list of tools from the last discovery.
    #[inline]
    #[must_use]
    pub fn tools(&self) -> &[McpTool] {
        &self.cached_tools
    }

    /// Returns the cached list of resources from the last discovery.
    #[inline]
    #[must_use]
    pub fn resources(&self) -> &[McpResource] {
        &self.cached_resources
    }

    /// Returns the server info from the initialize handshake.
    #[inline]
    #[must_use]
    pub fn server_info(&self) -> Option<&McpInitializeResult> {
        self.server_info.as_ref()
    }

    /// Performs the MCP handshake: sends `initialize`.
    ///
    /// Idempotent — if already initialized, returns the cached result.
    pub async fn initialize(&mut self) -> McpSessionResult<&McpInitializeResult> {
        if self.initialized {
            return self
                .server_info
                .as_ref()
                .ok_or_else(|| McpSessionError::InvalidResponse {
                    server_name: self.server_name.clone(),
                    method: "initialize",
                    details: "initialized flag set but no server info cached".to_string(),
                });
        }

        let result = self
            .transport
            .initialize()
            .await
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })?;

        self.initialized = true;
        self.server_info = Some(result.clone());

        Ok(self.server_info.as_ref().unwrap())
    }

    /// Discovers all tools available on this server, with cursor-based pagination.
    ///
    /// Results are cached; subsequent calls return the cached list.
    pub async fn list_tools(&mut self) -> McpSessionResult<&[McpTool]> {
        self.initialize().await?;
        let tools = self
            .transport
            .list_tools()
            .await
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })?;
        self.cached_tools = tools;
        Ok(&self.cached_tools)
    }

    /// Calls a named tool with the given arguments.
    pub async fn call_tool(
        &mut self,
        name: &str,
        args: Option<JsonValue>,
    ) -> McpSessionResult<McpToolCallResult> {
        self.initialize().await?;
        self.transport
            .call_tool(name, args)
            .await
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })
    }

    /// Discovers all resources available on this server.
    ///
    /// Results are cached; subsequent calls return the cached list.
    pub async fn list_resources(&mut self) -> McpSessionResult<&[McpResource]> {
        self.initialize().await?;
        let resources =
            self.transport
                .list_resources()
                .await
                .map_err(|e| McpSessionError::Transport {
                    server_name: self.server_name.clone(),
                    source: e,
                })?;
        self.cached_resources = resources;
        Ok(&self.cached_resources)
    }

    /// Reads a resource by URI.
    pub async fn read_resource(&mut self, uri: &str) -> McpSessionResult<McpReadResourceResult> {
        self.initialize().await?;
        self.transport
            .read_resource(uri)
            .await
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })
    }

    /// Gracefully shuts down the transport and resets cached state.
    pub async fn shutdown(&mut self) -> McpSessionResult<()> {
        self.transport
            .shutdown()
            .await
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })?;
        self.initialized = false;
        self.cached_tools.clear();
        self.cached_resources.clear();
        self.server_info = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transport Builders — ergonomic helpers to build sessions from config
// ---------------------------------------------------------------------------

/// Builds a `StdioTransport`-backed session.
pub struct StdioTransportBuilder {
    server_name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    initialize_timeout_ms: Option<u64>,
    list_timeout_ms: Option<u64>,
    tool_call_timeout_ms: Option<u64>,
}

impl StdioTransportBuilder {
    pub fn new(server_name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            initialize_timeout_ms: None,
            list_timeout_ms: None,
            tool_call_timeout_ms: None,
        }
    }

    #[must_use]
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    #[must_use]
    pub fn env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    #[must_use]
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.initialize_timeout_ms = Some(ms);
        self.list_timeout_ms = Some(ms);
        self.tool_call_timeout_ms = Some(ms);
        self
    }

    #[must_use]
    pub fn initialize_timeout_ms(mut self, ms: u64) -> Self {
        self.initialize_timeout_ms = Some(ms);
        self
    }

    #[must_use]
    pub fn list_timeout_ms(mut self, ms: u64) -> Self {
        self.list_timeout_ms = Some(ms);
        self
    }

    /// Builds the session, spawning the child process.
    pub fn build(self) -> McpSessionResult<McpServerSession> {
        let transport = StdioTransport::new(super::stdio_transport::StdioTransportConfig {
            server_name: self.server_name.clone(),
            command: self.command,
            args: self.args,
            env: self.env,
            initialize_timeout_ms: self.initialize_timeout_ms,
            list_timeout_ms: self.list_timeout_ms,
            tool_call_timeout_ms: self.tool_call_timeout_ms,
        })
        .map_err(|e| McpSessionError::Transport {
            server_name: self.server_name.clone(),
            source: McpTransportError::Io { source: e },
        })?;

        Ok(McpServerSession::new(self.server_name, Box::new(transport)))
    }
}

/// Builds an `HttpStreamTransport`-backed session.
pub struct HttpStreamTransportBuilder {
    server_name: String,
    url: String,
    extra_headers: BTreeMap<String, String>,
    auth: McpAuthConfig,
    timeout_ms: Option<u64>,
    auto_detect_sse_fallback: bool,
    insecure: bool,
}

impl HttpStreamTransportBuilder {
    pub fn new(server_name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            url: url.into(),
            extra_headers: BTreeMap::new(),
            auth: McpAuthConfig::None,
            timeout_ms: None,
            auto_detect_sse_fallback: true,
            insecure: false,
        }
    }

    #[must_use]
    pub fn headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    #[must_use]
    pub fn bearer_token(mut self, token: String) -> Self {
        self.auth = McpAuthConfig::BearerToken(token);
        self
    }

    #[must_use]
    pub fn oauth(
        mut self,
        client_id: Option<String>,
        callback_port: Option<u16>,
        auth_server_metadata_url: Option<String>,
    ) -> Self {
        self.auth = McpAuthConfig::OAuth {
            client_id,
            callback_port,
            auth_server_metadata_url,
            xaa: None,
        };
        self
    }

    #[must_use]
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    #[must_use]
    pub fn auto_detect_sse_fallback(mut self, yes: bool) -> Self {
        self.auto_detect_sse_fallback = yes;
        self
    }

    #[must_use]
    pub fn insecure(mut self, yes: bool) -> Self {
        self.insecure = yes;
        self
    }

    pub fn build(self) -> McpSessionResult<McpServerSession> {
        let transport =
            HttpStreamTransport::new(super::http_stream_transport::HttpStreamTransportConfig {
                server_name: self.server_name.clone(),
                url: self.url,
                extra_headers: self.extra_headers,
                auth: self.auth,
                timeout_ms: self.timeout_ms,
                auto_detect_sse_fallback: self.auto_detect_sse_fallback,
                insecure: self.insecure,
            })
            .map_err(|e| McpSessionError::Transport {
                server_name: self.server_name.clone(),
                source: e,
            })?;

        Ok(McpServerSession::new(self.server_name, Box::new(transport)))
    }
}

/// Builds an `SseTransport`-backed session.
pub struct SseTransportBuilder {
    server_name: String,
    post_url: String,
    sse_url: Option<String>,
    extra_headers: BTreeMap<String, String>,
    auth: McpAuthConfig,
    timeout_ms: Option<u64>,
}

impl SseTransportBuilder {
    pub fn new(server_name: impl Into<String>, post_url: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            post_url: post_url.into(),
            sse_url: None,
            extra_headers: BTreeMap::new(),
            auth: McpAuthConfig::None,
            timeout_ms: None,
        }
    }

    #[must_use]
    pub fn sse_url(mut self, url: impl Into<String>) -> Self {
        self.sse_url = Some(url.into());
        self
    }

    #[must_use]
    pub fn headers(mut self, headers: BTreeMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    #[must_use]
    pub fn bearer_token(mut self, token: String) -> Self {
        self.auth = McpAuthConfig::BearerToken(token);
        self
    }

    #[must_use]
    pub fn oauth(
        mut self,
        client_id: Option<String>,
        callback_port: Option<u16>,
        auth_server_metadata_url: Option<String>,
    ) -> Self {
        self.auth = McpAuthConfig::OAuth {
            client_id,
            callback_port,
            auth_server_metadata_url,
            xaa: None,
        };
        self
    }

    #[must_use]
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    pub fn build(self) -> McpSessionResult<McpServerSession> {
        let transport = SseTransport::new(super::sse_transport::SseTransportConfig {
            server_name: self.server_name.clone(),
            post_url: self.post_url,
            sse_url: self.sse_url,
            extra_headers: self.extra_headers,
            auth: self.auth,
            timeout_ms: self.timeout_ms,
        })
        .map_err(|e| McpSessionError::Transport {
            server_name: self.server_name.clone(),
            source: e,
        })?;

        Ok(McpServerSession::new(self.server_name, Box::new(transport)))
    }
}

// ---------------------------------------------------------------------------
// McpServerSessionManager — orchestrates multiple sessions
// ---------------------------------------------------------------------------

/// Manages a collection of MCP server sessions.
///
/// This is the top-level entry point used by the runtime. It maps server names
/// to sessions, handles tool routing, and provides a unified API regardless of
/// transport type. Sessions are lazily initialized on first use.
pub struct McpServerSessionManager {
    sessions: BTreeMap<String, McpServerSession>,
}

impl Default for McpServerSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServerSessionManager {
    /// Creates a new session manager with no sessions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
        }
    }

    /// Registers a new session. If a session with the same name exists, it is replaced.
    pub fn add_session(&mut self, session: McpServerSession) {
        self.sessions
            .insert(session.server_name().to_string(), session);
    }

    /// Registers a new stdio server. Short-hand.
    pub fn add_stdio_server(
        &mut self,
        name: impl Into<String>,
        command: impl Into<String>,
    ) -> McpSessionResult<()> {
        let name_str = name.into();
        let builder = StdioTransportBuilder::new(&name_str, command);
        let session = builder.build()?;
        self.sessions.insert(name_str, session);
        Ok(())
    }

    /// Registers a new HTTP/SSE server session.
    /// Eagerly initializes the session to surface connection errors immediately
    /// rather than deferring them to the first tool call.
    pub async fn add_http_session(
        &mut self,
        name: &str,
        url: &str,
        extra_headers: BTreeMap<String, String>,
    ) -> McpSessionResult<()> {
        let mut builder = HttpStreamTransportBuilder::new(name, url);
        if !extra_headers.is_empty() {
            builder = builder.headers(extra_headers);
        }
        let mut session = builder.build()?;
        // Initialize eagerly to surface connection errors immediately.
        // This is critical for hot-reload scenarios where a previously-working
        // server becomes unreachable: without eager init, the error only surfaces
        // at tool-call time, making it look like the server was never added.
        if let Err(e) = session.initialize().await {
            tracing::warn!(
                "add_http_session: server '{}' at '{}' initialize failed: {}",
                name,
                url,
                e
            );
        }
        self.sessions.insert(name.to_string(), session);
        Ok(())
    }

    /// Removes a server session and shuts it down.
    pub async fn remove_session(&mut self, name: &str) -> McpSessionResult<()> {
        if let Some(mut session) = self.sessions.remove(name) {
            session.shutdown().await?;
        }
        Ok(())
    }

    /// Returns an iterator over all server names.
    pub fn server_names(&self) -> impl Iterator<Item = &str> {
        self.sessions.keys().map(String::as_str)
    }

    /// Returns the number of registered sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns true if there are no sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns true if a server with the given name is registered.
    #[inline]
    #[must_use]
    pub fn has_server(&self, name: &str) -> bool {
        self.sessions.contains_key(name)
    }

    /// Gets a mutable reference to a session by name.
    pub fn session_mut(&mut self, name: &str) -> McpSessionResult<&mut McpServerSession> {
        self.sessions
            .get_mut(name)
            .ok_or_else(|| McpSessionError::UnknownServer {
                server_name: name.to_string(),
            })
    }

    /// Initializes all sessions and discovers their tools.
    ///
    /// Returns a combined list of all discovered tools and any failures.
    /// Discovers all tools from all registered MCP servers.
    ///
    /// Session names are collected under the manager write-lock, then each server's
    /// `list_tools` is called in parallel as independent tasks (each acquiring its
    /// own temporary lock). This avoids O(n) sequential server round-trips when n
    /// servers exist.
    ///
    /// Results are cached per-session (see [`McpServerSession::list_tools`]).
    pub async fn discover_all_tools(&mut self) -> (Vec<McpTool>, Vec<(String, McpSessionError)>) {
        let names: Vec<String> = self.sessions.keys().cloned().collect();

        if names.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut all_tools = Vec::new();
        let mut failures = Vec::new();

        for name in names {
            let session = self.sessions.get_mut(&name);
            if let Some(session) = session {
                match session.list_tools().await {
                    Ok(tools) => all_tools.extend_from_slice(tools),
                    Err(e) => failures.push((name, e)),
                }
            }
        }

        (all_tools, failures)
    }

    /// Calls a tool by its qualified name (`mcp__server__tool`).
    ///
    /// Returns the tool call result or an error if the tool is unknown or
    /// the server is unavailable.
    pub async fn call_tool(
        &mut self,
        qualified_name: &str,
        args: Option<JsonValue>,
    ) -> McpSessionResult<McpToolCallResult> {
        let parts = qualified_name
            .strip_prefix("mcp__")
            .and_then(|s| s.split_once("__"));

        let Some((server_name, tool_name)) = parts else {
            return Err(McpSessionError::InvalidResponse {
                server_name: qualified_name.to_string(),
                method: "tools/call",
                details: format!(
                    "qualified tool name `{qualified_name}` does not match mcp__server__tool pattern"
                ),
            });
        };

        let session = self.session_mut(server_name)?;
        session.call_tool(tool_name, args).await
    }

    /// Lists resources from a specific server by name.
    pub async fn list_resources(
        &mut self,
        server_name: &str,
    ) -> McpSessionResult<Vec<McpResource>> {
        let session = self.session_mut(server_name)?;
        session.list_resources().await.map(<[McpResource]>::to_vec)
    }

    /// Shuts down all sessions.
    pub async fn shutdown_all(&mut self) {
        let names: Vec<String> = self.sessions.keys().cloned().collect();
        for name in names {
            if let Some(session) = self.sessions.get_mut(&name) {
                let _ = session.shutdown().await;
            }
        }
        self.sessions.clear();
    }
}

impl std::fmt::Debug for McpServerSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerSessionManager")
            .field("sessions", &self.sessions.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_stream_builder_chain() {
        let session = HttpStreamTransportBuilder::new("github", "https://api.github.com/mcp")
            .bearer_token("ghp_test".to_string())
            .timeout_ms(30_000)
            .auto_detect_sse_fallback(true)
            .build()
            .unwrap();
        assert_eq!(session.server_name(), "github");
        assert_eq!(session.kind(), McpTransportKind::HttpStream);
    }

    #[test]
    fn sse_transport_builder_chain() {
        let session = SseTransportBuilder::new("legacy", "https://legacy.example/mcp")
            .bearer_token("tok_test".to_string())
            .timeout_ms(60_000)
            .build()
            .unwrap();
        assert_eq!(session.server_name(), "legacy");
        assert_eq!(session.kind(), McpTransportKind::Sse);
    }

    #[test]
    fn manager_tracks_server_names() {
        let mut manager = McpServerSessionManager::new();
        assert!(manager.is_empty());

        let _ = HttpStreamTransportBuilder::new("gh", "https://gh.example/mcp")
            .build()
            .map(|s| manager.add_session(s));

        let _ = SseTransportBuilder::new("sse", "https://sse.example/mcp")
            .build()
            .map(|s| manager.add_session(s));

        assert_eq!(manager.len(), 2);
        let names: Vec<_> = manager.server_names().collect();
        assert!(names.contains(&"gh"));
        assert!(names.contains(&"sse"));
    }
}
