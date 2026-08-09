//! Universal MCP transport layer.
//!
//! This module provides a unified, transport-agnostic interface for connecting to
//! Model Context Protocol (MCP) servers regardless of the underlying protocol:
//! - **Stdio** — local child process with LSP-framed stdin/stdout
//! - **`HttpStream`** — Streamable HTTP (MCP 2025 standard, single endpoint)
//! - **`Sse`** — HTTP + Server-Sent Events (legacy platforms)
//!
//! ## Core Types
//!
//! - [`McpTransport`] — the unified trait all transports implement
//! - [`McpTransportKind`] — categorises a transport by its underlying protocol
//! - [`McpTransportError`] — all transport errors in one enum
//! - [`McpAuthConfig`] — authentication strategy (none / bearer token / `OAuth2`)
//! - [`McpHealthStatus`] — health probe result
//!
//! ## Creating a Transport
//!
//! ```ignore
//! use runtime::mcp::{
//!     McpAuthConfig, McpHealthStatus, McpTransport, McpTransportKind,
//!     http_stream_transport::HttpStreamTransportConfig,
//! };
//!
//! let config = HttpStreamTransportConfig {
//!     server_name: "github".into(),
//!     url: "https://api.example.com/mcp".into(),
//!     extra_headers: BTreeMap::new(),
//!     auth: McpAuthConfig::BearerToken("my-token".into()),
//!     timeout_ms: Some(60_000),
//!     auto_detect_sse_fallback: true,
//!     insecure: false,
//! };
//! let transport = HttpStreamTransport::new(config).unwrap();
//! let tools = transport.list_tools().await.unwrap();
//! ```

pub mod http_stream_transport;
pub mod session;
pub mod sse_transport;
pub mod stdio_transport;
pub mod transport;
pub mod types;
pub mod utils;

pub use http_stream_transport::HttpStreamTransport;
pub use http_stream_transport::HttpStreamTransportConfig;
pub use session::{
    HttpStreamTransportBuilder, McpServerSession, McpServerSessionManager, McpSessionError,
    McpSessionResult, SseTransportBuilder, StdioTransportBuilder,
};
pub use sse_transport::SseTransport;
pub use sse_transport::SseTransportConfig;
pub use stdio_transport::StdioTransport;
pub use stdio_transport::StdioTransportConfig;
pub use transport::{
    McpAuthConfig, McpHealthStatus, McpTransport, McpTransportError, McpTransportKind,
};
pub use types::{
    default_initialize_params, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    McpInitializeClientInfo, McpInitializeParams, McpInitializeResult, McpInitializeServerInfo,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpPaginatedParams, McpReadResourceParams, McpReadResourceResult, McpResource,
    McpResourceContents, McpTool, McpToolCallContent, McpToolCallParams, McpToolCallResult,
};
pub use utils::{
    mcp_server_signature, mcp_tool_name, mcp_tool_prefix, normalize_name_for_mcp,
    scoped_mcp_config_hash, unwrap_ccr_proxy_url, write_audit_log,
};
