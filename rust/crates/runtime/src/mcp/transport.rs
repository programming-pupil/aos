//! Unified transport abstraction for MCP servers.
//!
//! All MCP server connections — regardless of the underlying transport — implement
//! this trait. This design allows `McpServerManager` to manage stdio, HTTP, and SSE
//! servers through a single, unified interface without any transport-specific branching.
//!
//! ## Transport Types
//!
//! | Type | Module | Description |
//! |------|--------|-------------|
//! | `Stdio` | `stdio_transport` | Child process with LSP-framed stdio |
//! | `HttpStream` | `http_stream_transport` | Streamable HTTP (MCP 2025 standard) |
//! | `Sse` | `sse_transport` | HTTP + Server-Sent Events (legacy platforms) |
//!
//! ## Protocol Flow
//!
//! 1. `initialize()` — send JSON-RPC `initialize`, read `Initialized` notification
//! 2. `list_tools()` — discover available tools (with cursor pagination)
//! 3. `call_tool(name, args)` — invoke a named tool
//! 4. `list_resources()` / `read_resource(uri)` — resource operations
//! 5. `shutdown()` — graceful termination
//!
//! Each operation is idempotent-safe (can be retried on transient failures).

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use super::types::{
    JsonRpcId, McpInitializeResult, McpReadResourceResult, McpTool, McpToolCallResult,
};

/// Identifies the kind of underlying transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransportKind {
    /// Local child process communicating over stdin/stdout.
    Stdio,
    /// HTTP with Streamable transport (2025 standard, single endpoint).
    HttpStream,
    /// HTTP with Server-Sent Events (legacy platforms, bidirectional).
    Sse,
}

impl McpTransportKind {
    /// Returns a static string representation of this transport kind.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::HttpStream => "http_stream",
            Self::Sse => "sse",
        }
    }
}

/// Authentication strategy for remote MCP servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAuthConfig {
    /// No authentication.
    None,
    /// Bearer token sent as `Authorization: Bearer <token>` header.
    BearerToken(String),
    /// `OAuth2` with PKCE (interactive flow, callback required).
    OAuth {
        client_id: Option<String>,
        callback_port: Option<u16>,
        auth_server_metadata_url: Option<String>,
        xaa: Option<bool>,
    },
}

impl McpAuthConfig {
    /// Returns true if this auth config requires interactive user flow (`OAuth`).
    #[must_use]
    pub fn requires_user_interaction(&self) -> bool {
        matches!(self, Self::OAuth { .. })
    }

    /// Builds the HTTP headers required for this authentication.
    pub fn apply_to_headers(&self, headers: &mut BTreeMap<String, String>) {
        match self {
            Self::BearerToken(token) => {
                headers.insert("Authorization".to_string(), format!("Bearer {token}"));
            }
            Self::None | Self::OAuth { .. } => {}
        }
    }
}

/// Health status of a connected transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpHealthStatus {
    /// Transport is connected and responsive.
    Healthy,
    /// Transport has been used but may need reconnection.
    Degraded,
    /// Transport has failed and cannot recover without intervention.
    Unhealthy,
}

/// Unified interface for all MCP transport implementations.
///
/// Implementors must be safe to share across threads (`Send + Sync`) and must
/// manage their own connection lifecycle. The `McpServerManager` calls these
/// methods; transport implementors do not need to know about the manager.
///
/// All async methods use `#[async_trait]` so that `Box<dyn McpTransport>` can
/// be used as a trait object.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// The transport kind (stdio / `http_stream` / sse).
    fn kind(&self) -> McpTransportKind;

    /// Human-readable server name, used in logs and error messages.
    fn server_name(&self) -> &str;

    /// Returns the current health of the transport.
    fn health_check(&self) -> McpHealthStatus;

    /// Performs the MCP handshake: sends `initialize`, waits for the response,
    /// then sends the `Initialized` notification. Returns server info.
    ///
    /// Idempotent — calling twice returns the cached result without re-initializing.
    async fn initialize(&self) -> Result<McpInitializeResult, McpTransportError>;

    /// Lists all tools available on this server. Handles cursor pagination internally.
    async fn list_tools(&self) -> Result<Vec<McpTool>, McpTransportError>;

    /// Calls a named tool with the given arguments. Returns the tool result.
    async fn call_tool(
        &self,
        name: &str,
        args: Option<JsonValue>,
    ) -> Result<McpToolCallResult, McpTransportError>;

    /// Lists all resources available on this server.
    async fn list_resources(&self) -> Result<Vec<super::types::McpResource>, McpTransportError>;

    /// Reads the content of a resource identified by `uri`.
    async fn read_resource(&self, uri: &str) -> Result<McpReadResourceResult, McpTransportError>;

    /// Gracefully shuts down the transport, releasing all resources.
    async fn shutdown(&mut self) -> Result<(), McpTransportError>;

    /// Returns a unique ID for the next JSON-RPC request.
    fn next_request_id(&self) -> JsonRpcId;
}

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

/// Errors that can occur during any transport operation.
#[derive(Debug)]
pub enum McpTransportError {
    /// I/O error — connection dropped, pipe broken, etc.
    Io { source: std::io::Error },
    /// HTTP layer error (for HTTP-based transports).
    Http { status: u16, message: String },
    /// The server returned a JSON-RPC error response.
    JsonRpc { code: i64, message: String },
    /// Server returned a response that could not be parsed.
    InvalidResponse { details: String },
    /// Operation exceeded its timeout.
    Timeout {
        operation: &'static str,
        timeout_ms: u64,
    },
    /// Authentication failed (token invalid/expired, OAuth rejected).
    AuthFailed { reason: String },
    /// Authentication is required but no credentials were configured.
    AuthMissing { message: String },
    /// Transport does not support this operation.
    UnsupportedOperation { operation: &'static str },
    /// Transport was shut down.
    Shutdown,
}

impl std::fmt::Display for McpTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { source } => write!(f, "I/O error: {source}"),
            Self::Http { status, message } => {
                write!(f, "HTTP error {status}: {message}")
            }
            Self::JsonRpc { code, message } => {
                write!(f, "JSON-RPC error {code}: {message}")
            }
            Self::InvalidResponse { details } => {
                write!(f, "invalid server response: {details}")
            }
            Self::Timeout {
                operation,
                timeout_ms,
            } => {
                write!(f, "{operation} timed out after {timeout_ms}ms")
            }
            Self::AuthFailed { reason } => write!(f, "authentication failed: {reason}"),
            Self::AuthMissing { message } => write!(f, "authentication required: {message}"),
            Self::UnsupportedOperation { operation } => {
                write!(f, "transport does not support {operation}")
            }
            Self::Shutdown => write!(f, "transport has been shut down"),
        }
    }
}

impl std::error::Error for McpTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for McpTransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io { source: e }
    }
}
