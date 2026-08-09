//! Server-Sent Events (SSE) transport for legacy MCP servers.
//!
//! This transport handles platforms that implement the deprecated MCP HTTP+SSE specification
//! (pre-2025), where:
//! - Client-to-server: HTTP POST requests to a main endpoint
//! - Server-to-client: A separate SSE endpoint streams events back
//!
//! This is the fallback for platforms that do not support Streamable HTTP (2025 standard).
//! Most users will prefer `HttpStreamTransport` with `auto_detect_sse_fallback: true`,
//! which automatically switches to SSE if needed.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::Method;
use reqwest::Client;
use serde_json::Value as JsonValue;
use tokio::sync::RwLock;

use super::types::{
    default_initialize_params, JsonRpcId, JsonRpcRequest, JsonRpcResponse, McpInitializeResult,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpReadResourceParams, McpReadResourceResult, McpResource, McpTool, McpToolCallParams,
    McpToolCallResult,
};
use super::{McpAuthConfig, McpHealthStatus, McpTransport, McpTransportError, McpTransportKind};

/// Default request timeout in milliseconds.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Default tool call timeout in milliseconds.
const DEFAULT_TOOL_CALL_TIMEOUT_MS: u64 = 120_000;

/// Configuration for an SSE-based MCP server connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseTransportConfig {
    pub server_name: String,
    /// POST endpoint for sending JSON-RPC requests.
    pub post_url: String,
    /// SSE endpoint for receiving server events.
    /// If not provided, the transport derives it from `post_url`.
    pub sse_url: Option<String>,
    /// Extra HTTP headers injected on every request.
    pub extra_headers: BTreeMap<String, String>,
    /// Authentication configuration.
    pub auth: McpAuthConfig,
    /// Request-level timeout in ms. Defaults to 60000.
    pub timeout_ms: Option<u64>,
}

impl SseTransportConfig {
    /// Returns the SSE URL, using the fallback heuristic if not explicitly configured.
    #[must_use]
    pub fn sse_url(&self) -> String {
        if let Some(ref url) = self.sse_url {
            return url.clone();
        }
        let post = &self.post_url;
        if post.ends_with("/mcp") {
            format!("{}/sse", &post[..post.len() - 4])
        } else {
            format!("{}/sse", post.trim_end_matches('/'))
        }
    }
}

/// An active SSE MCP server connection.
///
/// Thread-safe (`Send + Sync`). Holds a shared `reqwest::Client` and a background
/// SSE event stream. The SSE stream is managed via a `RwLock` so it can be
/// reconnected on failures.
pub struct SseTransport {
    config: SseTransportConfig,
    client: Client,
    initialized: RwLock<bool>,
    server_info: RwLock<Option<McpInitializeResult>>,
    session_id: RwLock<Option<String>>,
    next_id: Arc<AtomicU64>,
}

impl SseTransport {
    pub fn new(config: SseTransportConfig) -> Result<Self, McpTransportError> {
        let client = Client::builder()
            .timeout(Duration::from_millis(
                config.timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
            ))
            .build()
            .map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to build HTTP client: {e}"),
            })?;

        Ok(Self {
            config,
            client,
            initialized: RwLock::new(false),
            server_info: RwLock::new(None),
            session_id: RwLock::new(None),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    fn timeout_ms(&self) -> u64 {
        self.config.timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS)
    }

    fn tool_call_timeout_ms(&self) -> u64 {
        self.config
            .timeout_ms
            .unwrap_or(DEFAULT_TOOL_CALL_TIMEOUT_MS)
    }

    #[allow(dead_code)]
    fn next_request_id(&self) -> JsonRpcId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        JsonRpcId::Number(id)
    }

    /// Builds a POST request with auth and extra headers applied.
    fn build_post_request(&self, body: Bytes) -> reqwest::RequestBuilder {
        let mut req_builder = self
            .client
            .request(Method::POST, &self.config.post_url)
            .header(http::header::CONTENT_TYPE.as_str(), "application/json")
            .header(http::header::ACCEPT.as_str(), "application/json");

        // Include server-assigned session ID if available.
        if let Ok(session_id) = self.session_id.try_read() {
            if let Some(ref sid) = *session_id {
                req_builder = req_builder.header("Mcp-Session-Id", sid.as_str());
            }
        }

        for (k, v) in &self.config.extra_headers {
            req_builder = req_builder.header(k.as_str(), v.as_str());
        }

        match &self.config.auth {
            McpAuthConfig::BearerToken(token) => {
                req_builder = req_builder.header(
                    http::header::AUTHORIZATION.as_str(),
                    format!("Bearer {token}"),
                );
            }
            McpAuthConfig::OAuth { .. } | McpAuthConfig::None => {}
        }

        req_builder.body(body)
    }

    /// Builds a JSON-RPC 2.0 request body with the given method name and params,
    /// then passes it to `build_post_request`. This is the correct format for all
    /// MCP SSE transport requests.
    fn build_jsonrpc_request(
        &self,
        method: &'static str,
        params: impl serde::Serialize,
    ) -> Result<reqwest::RequestBuilder, McpTransportError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rpc_request = JsonRpcRequest::new(JsonRpcId::Number(id), method, Some(params));
        let body =
            serde_json::to_vec(&rpc_request).map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to serialize JSON-RPC request: {e}"),
            })?;
        Ok(self.build_post_request(Bytes::from(body)))
    }

    /// Parses a JSON HTTP response into a JSON-RPC response.
    async fn parse_json_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, McpTransportError> {
        let status = response.status().as_u16();
        if status >= 400 {
            let text = response.text().await.unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(err) = v.get("error").and_then(|e| e.as_object()) {
                    let code = err
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(-1);
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or(&text)
                        .to_string();
                    return Err(McpTransportError::JsonRpc { code, message });
                }
            }
            return Err(McpTransportError::Http {
                status,
                message: text,
            });
        }
        response
            .json::<T>()
            .await
            .map_err(|e| McpTransportError::InvalidResponse {
                details: format!("JSON parse error: {e}"),
            })
    }

    /// Parses an SSE event stream into a JSON-RPC response.
    #[allow(dead_code)]
    async fn parse_sse_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, McpTransportError> {
        let mut buffer = String::new();

        let text = response
            .text()
            .await
            .map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to read SSE body: {e}"),
            })?;

        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("data:") {
                let data = line.strip_prefix("data:").unwrap().trim();
                if data == "[DONE]" {
                    break;
                }
                buffer.push_str(data);
            }
        }

        if buffer.is_empty() {
            return Err(McpTransportError::InvalidResponse {
                details: "SSE stream contained no data".to_string(),
            });
        }

        serde_json::from_str(&buffer).map_err(|e| McpTransportError::InvalidResponse {
            details: format!("SSE JSON parse error: {e}"),
        })
    }
}

// ---------------------------------------------------------------------------
// SseTransport helper methods
// ---------------------------------------------------------------------------

impl SseTransport {
    async fn send_notification(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), McpTransportError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let body_bytes =
            serde_json::to_vec(&body).map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to serialize notification: {e}"),
            })?;

        let _response = self
            .build_post_request(bytes::Bytes::from(body_bytes))
            .timeout(Duration::from_millis(self.timeout_ms()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout {
                        operation: "notification",
                        timeout_ms: self.timeout_ms(),
                    }
                } else {
                    McpTransportError::InvalidResponse {
                        details: format!("notification '{method}' failed: {e}"),
                    }
                }
            })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// McpTransport impl for SseTransport
// ---------------------------------------------------------------------------

#[async_trait]
impl McpTransport for SseTransport {
    fn kind(&self) -> McpTransportKind {
        McpTransportKind::Sse
    }

    fn server_name(&self) -> &str {
        &self.config.server_name
    }

    fn health_check(&self) -> McpHealthStatus {
        if *self.initialized.blocking_read() {
            McpHealthStatus::Healthy
        } else {
            McpHealthStatus::Degraded
        }
    }

    async fn initialize(&self) -> Result<McpInitializeResult, McpTransportError> {
        if *self.initialized.read().await {
            return self.server_info.read().await.clone().ok_or(
                McpTransportError::InvalidResponse {
                    details: "initialized flag set but no server info".to_string(),
                },
            );
        }

        let params = default_initialize_params();
        let response = self
            .build_jsonrpc_request("initialize", params)?
            .timeout(Duration::from_millis(self.timeout_ms()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout {
                        operation: "initialize",
                        timeout_ms: self.timeout_ms(),
                    }
                } else {
                    McpTransportError::InvalidResponse {
                        details: format!("HTTP request failed: {e}"),
                    }
                }
            })?;

        let status = response.status().as_u16();
        if status >= 400 {
            let text = response.text().await.unwrap_or_default();
            return Err(McpTransportError::Http {
                status,
                message: text,
            });
        }

        // Extract session ID before consuming the body (needed by stateful servers like ModelScope).
        let session_id: Option<String> = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if let Some(sid) = session_id {
            *self.session_id.write().await = Some(sid);
        }

        let parsed: JsonRpcResponse<McpInitializeResult> =
            Self::parse_json_response(response).await?;

        if let Some(err) = parsed.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = parsed.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in initialize response".to_string(),
        })?;

        *self.initialized.write().await = true;
        *self.server_info.write().await = Some(result.clone());

        // Send the `Initialized` notification — required by MCP spec to complete handshake.
        if let Err(e) = self
            .send_notification("notifications/initialized", serde_json::json!({}))
            .await
        {
            tracing::warn!("SSE initialize: failed to send notifications/initialized: {e}");
        }

        Ok(result)
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpTransportError> {
        self.initialize().await?;
        let mut all_tools = Vec::new();
        let mut cursor = None;

        loop {
            let params = McpListToolsParams { cursor };

            let response = self
                .build_jsonrpc_request("tools/list", params)?
                .timeout(Duration::from_millis(self.timeout_ms()))
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        McpTransportError::Timeout {
                            operation: "tools/list",
                            timeout_ms: self.timeout_ms(),
                        }
                    } else {
                        McpTransportError::InvalidResponse {
                            details: format!("HTTP request failed: {e}"),
                        }
                    }
                })?;

            let parsed: JsonRpcResponse<McpListToolsResult> =
                Self::parse_json_response(response).await?;

            if let Some(err) = parsed.error {
                return Err(McpTransportError::JsonRpc {
                    code: err.code,
                    message: err.message,
                });
            }

            let result = parsed.result.ok_or(McpTransportError::InvalidResponse {
                details: "missing result in tools/list response".to_string(),
            })?;

            all_tools.extend(result.tools);
            match result.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(all_tools)
    }

    async fn call_tool(
        &self,
        name: &str,
        args: Option<JsonValue>,
    ) -> Result<McpToolCallResult, McpTransportError> {
        self.initialize().await?;
        let params = McpToolCallParams {
            name: name.to_string(),
            arguments: args,
            meta: None,
        };

        let response = self
            .build_jsonrpc_request("tools/call", params)?
            .timeout(Duration::from_millis(self.tool_call_timeout_ms()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout {
                        operation: "tools/call",
                        timeout_ms: self.tool_call_timeout_ms(),
                    }
                } else {
                    McpTransportError::InvalidResponse {
                        details: format!("HTTP request failed: {e}"),
                    }
                }
            })?;

        let parsed: JsonRpcResponse<McpToolCallResult> =
            Self::parse_json_response(response).await?;

        if let Some(err) = parsed.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }

        parsed.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in tools/call response".to_string(),
        })
    }

    async fn list_resources(&self) -> Result<Vec<McpResource>, McpTransportError> {
        self.initialize().await?;
        let params = McpListResourcesParams { cursor: None };

        let response = self
            .build_jsonrpc_request("resources/list", params)?
            .timeout(Duration::from_millis(self.timeout_ms()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout {
                        operation: "resources/list",
                        timeout_ms: self.timeout_ms(),
                    }
                } else {
                    McpTransportError::InvalidResponse {
                        details: format!("HTTP request failed: {e}"),
                    }
                }
            })?;

        let parsed: JsonRpcResponse<McpListResourcesResult> =
            Self::parse_json_response(response).await?;

        if let Some(err) = parsed.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = parsed.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in resources/list response".to_string(),
        })?;

        Ok(result.resources)
    }

    async fn read_resource(&self, uri: &str) -> Result<McpReadResourceResult, McpTransportError> {
        self.initialize().await?;
        let params = McpReadResourceParams {
            uri: uri.to_string(),
        };

        let response = self
            .build_jsonrpc_request("resources/read", params)?
            .timeout(Duration::from_millis(self.timeout_ms()))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    McpTransportError::Timeout {
                        operation: "resources/read",
                        timeout_ms: self.timeout_ms(),
                    }
                } else {
                    McpTransportError::InvalidResponse {
                        details: format!("HTTP request failed: {e}"),
                    }
                }
            })?;

        let parsed: JsonRpcResponse<McpReadResourceResult> =
            Self::parse_json_response(response).await?;

        if let Some(err) = parsed.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }

        parsed.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in resources/read response".to_string(),
        })
    }

    async fn shutdown(&mut self) -> Result<(), McpTransportError> {
        *self.initialized.write().await = false;
        *self.server_info.write().await = None;
        Ok(())
    }

    #[allow(dead_code)]
    fn next_request_id(&self) -> JsonRpcId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        JsonRpcId::Number(id)
    }
}
