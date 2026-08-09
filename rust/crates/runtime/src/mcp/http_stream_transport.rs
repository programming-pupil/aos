//! Streamable HTTP transport for remote MCP servers.
//!
//! Implements the MCP 2025-11-25 "Streamable HTTP" specification:
//! - Single HTTP endpoint (`POST /mcp`) for all JSON-RPC requests
//! - `Accept: application/json, text/event-stream` — server chooses response format
//! - Short operations (initialize, tools/list) return complete JSON
//! - Long operations (tools/call) may return SSE streams
//! - Each request is stateless; servers may track sessions via cookies
//!
//! ## Auto-Detection
//!
//! When `auto_detect_sse_fallback` is enabled (default: true), if a server returns
//! an SSE response where JSON is expected, the transport transparently switches to
//! SSE mode. This means users do not need to know which transport variant a platform uses.

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

/// Configuration for an HTTP-based MCP server connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpStreamTransportConfig {
    pub server_name: String,
    /// Single endpoint URL (e.g. `https://api.example.com/mcp`).
    pub url: String,
    /// Extra HTTP headers injected on every request.
    pub extra_headers: BTreeMap<String, String>,
    /// Authentication configuration.
    pub auth: McpAuthConfig,
    /// Request-level timeout in ms. Defaults to 60000.
    pub timeout_ms: Option<u64>,
    /// Auto-detect SSE fallback: if the server responds with SSE for a JSON request,
    /// automatically switch to SSE mode for subsequent requests.
    pub auto_detect_sse_fallback: bool,
    /// Skip TLS certificate verification (for development only).
    pub insecure: bool,
}

/// An active HTTP Streamable MCP server connection.
///
/// Thread-safe (`Send + Sync`). Holds a shared `reqwest::Client`; session state
/// (`initialized`, `server_info`, `use_sse`, `session_id`) is protected by `RwLock`.
pub struct HttpStreamTransport {
    config: HttpStreamTransportConfig,
    client: Client,
    initialized: RwLock<bool>,
    server_info: RwLock<Option<McpInitializeResult>>,
    use_sse: RwLock<bool>,
    /// Server-assigned session ID, returned in `Mcp-Session-Id` header during initialize.
    /// Must be included in all subsequent requests to maintain session state.
    session_id: RwLock<Option<String>>,
    next_id: Arc<AtomicU64>,
}

impl HttpStreamTransport {
    pub fn new(config: HttpStreamTransportConfig) -> Result<Self, McpTransportError> {
        tracing::debug!(
            server = %config.server_name,
            url = %config.url,
            auth = ?match &config.auth {
                McpAuthConfig::BearerToken(t) => Some(format!("Bearer {}", &t[..t.len().min(8)])),
                _ => None,
            },
            extra_headers_count = %config.extra_headers.len(),
            "HttpStreamTransport::new: CREATING",
        );
        let client = Client::builder()
            .timeout(Duration::from_millis(
                config.timeout_ms.unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
            ))
            .danger_accept_invalid_certs(config.insecure)
            .build()
            .map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to build HTTP client: {e}"),
            })?;

        Ok(Self {
            config,
            client,
            initialized: RwLock::new(false),
            server_info: RwLock::new(None),
            use_sse: RwLock::new(false),
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

    /// Builds a `reqwest::RequestBuilder` with auth and extra headers applied.
    /// If `session_id` is provided, the `Mcp-Session-Id` header is set — required by
    /// stateful servers like `ModelScope` for all requests after `initialize`.
    fn build_request(
        &self,
        body: Bytes,
        accept_sse: bool,
        session_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut req_builder = self
            .client
            .request(Method::POST, &self.config.url)
            .header(http::header::CONTENT_TYPE.as_str(), "application/json");

        // Always send both JSON and SSE in Accept, even when accept_sse=false.
        // Some servers (e.g. ModelScope) require both and will reject with -32600
        // if text/event-stream is missing, regardless of the actual response format.
        req_builder = req_builder.header(
            http::header::ACCEPT.as_str(),
            "application/json, text/event-stream",
        );

        // Add server-assigned session ID if provided (required by stateful servers like ModelScope).
        if let Some(sid) = session_id {
            req_builder = req_builder.header("Mcp-Session-Id", sid);
        }

        let mut extra_header_entries: Vec<(&String, &String)> = vec![];
        for (k, v) in &self.config.extra_headers {
            req_builder = req_builder.header(k.as_str(), v.as_str());
            extra_header_entries.push((k, v));
        }

        let auth_header = match &self.config.auth {
            McpAuthConfig::BearerToken(token) => {
                req_builder = req_builder.header(
                    http::header::AUTHORIZATION.as_str(),
                    format!("Bearer {token}"),
                );
                Some(format!("Bearer {}", &token[..token.len().min(8)]))
            }
            _ => None,
        };

        tracing::debug!(
            server = %self.config.server_name,
            url = %self.config.url,
            accept_sse = %accept_sse,
            session_id = ?session_id,
            extra_headers = ?extra_header_entries,
            auth = ?auth_header,
            body_preview = %String::from_utf8_lossy(&body).chars().take(300).collect::<String>(),
            "HttpStreamTransport: building request",
        );

        req_builder.body(body)
    }

    /// Builds a JSON-RPC 2.0 request body with the given method name and params,
    /// then passes it to `build_request`. This is the correct format for all
    /// MCP HTTP transport requests.
    fn build_jsonrpc_request(
        &self,
        method: &'static str,
        params: impl serde::Serialize,
        accept_sse: bool,
    ) -> Result<reqwest::RequestBuilder, McpTransportError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rpc_request = JsonRpcRequest::new(JsonRpcId::Number(id), method, Some(params));
        let body =
            serde_json::to_vec(&rpc_request).map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to serialize JSON-RPC request: {e}"),
            })?;
        // For stateful servers (e.g. ModelScope), read session ID from the lock
        // so all requests after initialize carry the Mcp-Session-Id header.
        let session_id = self.session_id.try_read().ok().and_then(|g| g.clone());
        Ok(self.build_request(Bytes::from(body), accept_sse, session_id.as_deref()))
    }

    /// Checks whether the response is an SSE stream by Content-Type header.
    #[allow(dead_code)]
    fn is_sse_response(response: &reqwest::Response) -> bool {
        response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"))
    }

    /// Extracts the `Mcp-Session-Id` header from the response without consuming the body.
    fn extract_session_id(response: &reqwest::Response) -> Option<String> {
        response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    /// Parses a plain JSON response body into a JSON-RPC response.
    #[allow(dead_code)]
    async fn parse_json_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, McpTransportError> {
        let status = response.status().as_u16();
        let body_text = response.text().await.unwrap_or_default();

        if status >= 400 {
            // Try to parse as JSON-RPC error first so callers get structured error details.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_text) {
                if let Some(err) = v.get("error").and_then(|e| e.as_object()) {
                    let code = err
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(-1);
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or(body_text.as_str())
                        .to_string();
                    return Err(McpTransportError::JsonRpc { code, message });
                }
            }
            return Err(McpTransportError::Http {
                status,
                message: body_text,
            });
        }
        // HTTP 200 — parse as JSON-RPC response. Deserialize into a generic Value
        // first so we can extract JSON-RPC error fields without losing detail on
        // type deserialization failure.
        let generic: serde_json::Value = match serde_json::from_str(&body_text) {
            Ok(v) => v,
            Err(e) => {
                return Err(McpTransportError::InvalidResponse {
                    details: format!("JSON parse error: {e} (body: {})", &body_text),
                });
            }
        };

        // If the JSON-RPC response has an error field, extract it with full detail.
        if let Some(err_obj) = generic.get("error").and_then(|e| e.as_object()) {
            let code = err_obj
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            let message = err_obj
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(body_text.as_str())
                .to_string();
            return Err(McpTransportError::JsonRpc { code, message });
        }

        // Deserialize into the target type.
        serde_json::from_str(&body_text).map_err(|e| McpTransportError::InvalidResponse {
            details: format!("JSON deserialize error: {e} (body: {})", &body_text),
        })
    }

    /// Parses an SSE stream into a JSON-RPC response.
    #[allow(dead_code)]
    async fn parse_sse_response<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, McpTransportError> {
        let text = response
            .text()
            .await
            .map_err(|e| McpTransportError::InvalidResponse {
                details: format!("failed to read SSE body: {e}"),
            })?;

        let mut buffer = String::new();
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

    #[allow(dead_code)]
    fn next_request_id(&self) -> JsonRpcId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        JsonRpcId::Number(id)
    }

    /// Sends a JSON-RPC notification (no `id` field, fire-and-forget).
    /// For stateful servers like `ModelScope`, the Mcp-Session-Id header is required
    /// on all requests after initialize. Pass `session_id` explicitly to avoid
    /// lock contention races with concurrent writes in `initialize`.
    async fn send_notification(
        &self,
        method: &str,
        params: serde_json::Value,
        session_id: Option<&str>,
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
            .build_request(Bytes::from(body_bytes), false, session_id)
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
        // Notifications are fire-and-forget; we don't parse the response.
        Ok(())
    }
}

#[async_trait]
impl McpTransport for HttpStreamTransport {
    fn kind(&self) -> McpTransportKind {
        McpTransportKind::HttpStream
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
        tracing::debug!(
            server = %self.config.server_name,
            url = %self.config.url,
            auth = ?match &self.config.auth {
                McpAuthConfig::BearerToken(t) => Some(format!("Bearer {}", &t[..t.len().min(8)])),
                _ => None,
            },
            extra_headers_count = %self.config.extra_headers.len(),
            "HttpStreamTransport::initialize: STARTING",
        );
        if *self.initialized.read().await {
            return self.server_info.read().await.clone().ok_or(
                McpTransportError::InvalidResponse {
                    details: "initialized flag set but no server info".to_string(),
                },
            );
        }

        let params = default_initialize_params();
        let response = self
            .build_jsonrpc_request("initialize", params, true)?
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

        let status = response.status();
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(none)");
        let mcp_session = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok());
        tracing::debug!(
            server = %self.config.server_name,
            status = %status.as_u16(),
            content_type = %content_type,
            mcp_session_id = ?mcp_session,
            "HttpStreamTransport::initialize: response received",
        );

        // Extract session ID before any locking to avoid race conditions.
        // The session ID must be included in the notifications/initialized request.
        let captured_session_id = Self::extract_session_id(&response);

        // Capture session ID before consuming the body (required by servers like ModelScope).
        if let Some(ref sid) = captured_session_id {
            *self.session_id.write().await = Some(sid.clone());
        }

        let use_sse = Self::is_sse_response(&response);
        if use_sse {
            *self.use_sse.write().await = true;
        }

        let parsed: JsonRpcResponse<McpInitializeResult> = if use_sse {
            Self::parse_sse_response(response).await?
        } else {
            Self::parse_json_response(response).await?
        };

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
        // NOTE: for ModelScope and other stateful servers, the Mcp-Session-Id MUST be included.
        // Pass captured_session_id explicitly to avoid lock contention races.
        tracing::info!(
            server = %self.config.server_name,
            session_id = ?captured_session_id,
            "HttpStreamTransport::initialize: sending notifications/initialized",
        );
        if let Err(e) = self
            .send_notification(
                "notifications/initialized",
                serde_json::json!({}),
                captured_session_id.as_deref(),
            )
            .await
        {
            tracing::warn!(
                server = %self.config.server_name,
                error = %e,
                "HttpStreamTransport::initialize: notifications/initialized failed (server may be stateless)",
            );
        }

        Ok(result)
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpTransportError> {
        self.initialize().await?;
        let mut all_tools = Vec::new();
        let mut cursor = None;

        loop {
            let params = McpListToolsParams { cursor };
            let accept_sse = *self.use_sse.read().await || self.config.auto_detect_sse_fallback;

            let response = self
                .build_jsonrpc_request("tools/list", params, accept_sse)?
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

            let is_sse = Self::is_sse_response(&response);
            if is_sse && !*self.use_sse.read().await && self.config.auto_detect_sse_fallback {
                *self.use_sse.write().await = true;
            }

            let parsed: JsonRpcResponse<McpListToolsResult> = if is_sse {
                Self::parse_sse_response(response).await?
            } else {
                Self::parse_json_response(response).await?
            };

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

        let use_sse_detected = *self.use_sse.read().await;
        tracing::info!(
            server = %self.config.server_name,
            tool_count = all_tools.len(),
            use_sse = %use_sse_detected,
            "HttpStreamTransport::list_tools: discovery complete",
        );

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
        let accept_sse = *self.use_sse.read().await || self.config.auto_detect_sse_fallback;

        let response = self
            .build_jsonrpc_request("tools/call", params, accept_sse)?
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

        let status = response.status();
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(none)");
        tracing::debug!(
            server = %self.config.server_name,
            tool = %name,
            status = %status.as_u16(),
            content_type = %content_type,
            "HttpStreamTransport::call_tool: response received",
        );

        let is_sse = Self::is_sse_response(&response);
        if is_sse && !*self.use_sse.read().await && self.config.auto_detect_sse_fallback {
            *self.use_sse.write().await = true;
        }

        // Read the full body so we can log it before parsing.
        let body_bytes =
            response
                .bytes()
                .await
                .map_err(|e| McpTransportError::InvalidResponse {
                    details: format!("failed to read response body: {e}"),
                })?;
        let body_text = String::from_utf8_lossy(&body_bytes);
        tracing::debug!(
            server = %self.config.server_name,
            tool = %name,
            body = %body_text,
            "HttpStreamTransport::call_tool: response body",
        );

        // Deserialize. If it fails, check if body is a JSON-RPC error.
        let parsed: Result<JsonRpcResponse<McpToolCallResult>, _> =
            serde_json::from_slice(&body_bytes);

        if let Err(e) = &parsed {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_text) {
                if let Some(err_obj) = v.get("error").and_then(|e| e.as_object()) {
                    let code = err_obj
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(-1);
                    let message = err_obj
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_else(|| body_text.as_ref())
                        .to_string();
                    return Err(McpTransportError::JsonRpc { code, message });
                }
            }
            return Err(McpTransportError::InvalidResponse {
                details: format!("JSON deserialize error: {e} (body: {})", &body_text),
            });
        }

        let parsed = parsed.unwrap();
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
        let accept_sse = *self.use_sse.read().await || self.config.auto_detect_sse_fallback;

        let response = self
            .build_jsonrpc_request("resources/list", params, accept_sse)?
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
        let accept_sse = *self.use_sse.read().await || self.config.auto_detect_sse_fallback;

        let response = self
            .build_jsonrpc_request("resources/read", params, accept_sse)?
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
