//! Stdio transport for local MCP servers.
//!
//! Spawns a child process and communicates using newline-delimited JSON-RPC over
//! the process's stdin/stdout pipes, as required by the MCP stdio transport.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use super::types::{
    default_initialize_params, JsonRpcId, JsonRpcRequest, JsonRpcResponse, McpInitializeResult,
    McpListResourcesParams, McpListResourcesResult, McpListToolsParams, McpListToolsResult,
    McpReadResourceParams, McpReadResourceResult, McpResource, McpTool, McpToolCallParams,
    McpToolCallResult,
};
use super::{McpHealthStatus, McpTransport, McpTransportError, McpTransportKind};

/// Default timeout for MCP initialization handshake (ms).
const STDIOT_INITIALIZE_TIMEOUT_MS: u64 = 60_000;
/// Default timeout for tool/resource listing (ms).
const STDIOT_LIST_TIMEOUT_MS: u64 = 30_000;
/// Default timeout for tool calls (ms).
const STDIOT_TOOL_CALL_TIMEOUT_MS: u64 = 60_000;
const STDERR_TAIL_BYTES: usize = 8_192;
const STDERR_DETAIL_CHARS: usize = 3_500;

/// Configuration for a stdio-based MCP server connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioTransportConfig {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub initialize_timeout_ms: Option<u64>,
    pub list_timeout_ms: Option<u64>,
    pub tool_call_timeout_ms: Option<u64>,
}

/// An active stdio MCP server connection.
///
/// Thread-safe (`Send + Sync`) because it wraps shared mutable state (`initialized`,
/// `process`, `next_request_id`) behind a `Mutex`.
pub struct StdioTransport {
    config: StdioTransportConfig,
    /// The spawned child process, if alive.
    process: Mutex<Option<Child>>,
    /// Buffered stdout reader.
    stdout: Mutex<Option<BufReader<ChildStdout>>>,
    /// Owned stdin writer.
    stdin: Mutex<Option<ChildStdin>>,
    /// Whether `initialize` has completed successfully.
    initialized: Mutex<bool>,
    /// Cached server info from the initialize response.
    server_info: Mutex<Option<McpInitializeResult>>,
    /// Per-connection request ID counter.
    next_id: Mutex<u64>,
    /// Bounded stderr tail used to explain process startup/protocol failures.
    stderr_tail: Arc<StdMutex<VecDeque<u8>>>,
}

impl StdioTransport {
    /// Creates a new stdio transport and immediately spawns the child process.
    pub fn new(mut config: StdioTransportConfig) -> io::Result<Self> {
        if apply_known_uvx_compatibility(&config.command, &mut config.args) {
            tracing::info!(
                server_name = %config.server_name,
                package = "mcp-server-fetch",
                constraint = "mcp<2",
                "applied MCP stdio upstream compatibility constraint"
            );
        }
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("stdio MCP process missing stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("stdio MCP process missing stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("stdio MCP process missing stderr pipe"))?;
        let stderr_tail = Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        Self::spawn_stderr_collector(stderr, stderr_tail.clone());

        Ok(Self {
            config,
            process: Mutex::new(Some(child)),
            stdout: Mutex::new(Some(BufReader::new(stdout))),
            stdin: Mutex::new(Some(stdin)),
            initialized: Mutex::new(false),
            server_info: Mutex::new(None),
            next_id: Mutex::new(1),
            stderr_tail,
        })
    }

    fn spawn_stderr_collector(mut stderr: ChildStderr, stderr_tail: Arc<StdMutex<VecDeque<u8>>>) {
        std::mem::drop(tokio::spawn(async move {
            let mut chunk = [0_u8; 1_024];
            loop {
                let read = match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let mut tail = stderr_tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for byte in &chunk[..read] {
                    if tail.len() == STDERR_TAIL_BYTES {
                        tail.pop_front();
                    }
                    tail.push_back(*byte);
                }
            }
        }));
    }

    fn stderr_excerpt(&self) -> Option<String> {
        let bytes = self
            .stderr_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut value = String::from_utf8_lossy(&bytes).trim().to_string();
        if value.is_empty() {
            return None;
        }
        for secret in self.config.env.values().filter(|secret| secret.len() >= 4) {
            value = value.replace(secret, "[REDACTED]");
        }
        value = crate::data_protection::protect_sensitive_text(
            &value,
            crate::data_protection::configured_data_protection_mode(),
        )
        .value;
        let value = value.chars().take(STDERR_DETAIL_CHARS).collect::<String>();
        (!value.is_empty()).then_some(value)
    }

    async fn take_request_id(id: &Mutex<u64>) -> JsonRpcId {
        let mut guard = id.lock().await;
        let id_val = *guard;
        *guard = id_val.saturating_add(1);
        JsonRpcId::Number(id_val)
    }

    async fn write_message(&self, payload: &[u8]) -> io::Result<()> {
        let mut stdin_guard = self.stdin.lock().await;
        let stdin = stdin_guard
            .as_mut()
            .ok_or_else(|| io::Error::other("transport has been shut down"))?;
        stdin.write_all(payload).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }

    async fn read_message(&self) -> io::Result<Vec<u8>> {
        let mut stdout_guard = self.stdout.lock().await;
        let stdout = stdout_guard
            .as_mut()
            .ok_or_else(|| io::Error::other("transport has been shut down"))?;
        loop {
            let mut line = String::new();
            stdout.read_line(&mut line).await?;
            if line.is_empty() {
                // Give the independent stderr drain a scheduling point after the
                // child closes both pipes so startup errors are diagnosable.
                tokio::time::sleep(Duration::from_millis(10)).await;
                let stderr = self
                    .stderr_excerpt()
                    .map(|detail| format!("; stderr: {detail}"))
                    .unwrap_or_default();
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("MCP stdio process exited before returning a JSON-RPC message{stderr}"),
                ));
            }
            let payload = line.trim_end_matches(['\r', '\n']);
            if !payload.trim().is_empty() {
                return Ok(payload.as_bytes().to_vec());
            }
        }
    }

    async fn send_notification(
        &self,
        method: &'static str,
        params: impl Serialize,
    ) -> io::Result<()> {
        let body = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.write_message(&body).await
    }

    async fn send_request<R: DeserializeOwned>(
        &self,
        method: &'static str,
        timeout_ms: u64,
        params: Option<impl Serialize>,
    ) -> io::Result<JsonRpcResponse<R>> {
        let id = Self::take_request_id(&self.next_id).await;
        let request = JsonRpcRequest::new(id.clone(), method, params);
        let body = serde_json::to_vec(&request)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        self.write_message(&body).await?;

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{method} timed out"),
                ));
            }
            let payload = timeout(remaining, self.read_message())
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, format!("{method} timed out"))
                })??;
            let value: JsonValue = serde_json::from_slice(&payload)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            // Servers may emit notifications (for example log/progress events)
            // while a request is in flight. They are not the response to this id.
            if value.get("id").is_none() && value.get("method").is_some() {
                continue;
            }
            let response: JsonRpcResponse<R> = serde_json::from_value(value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if response.jsonrpc != "2.0" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported jsonrpc version `{}`", response.jsonrpc),
                ));
            }
            if response.id != id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "mismatched response id: expected {id:?}, got {:?}",
                        response.id
                    ),
                ));
            }
            return Ok(response);
        }
    }

    fn tool_call_timeout_ms(&self) -> u64 {
        self.config
            .tool_call_timeout_ms
            .unwrap_or(STDIOT_TOOL_CALL_TIMEOUT_MS)
    }

    fn initialize_timeout_ms(&self) -> u64 {
        self.config
            .initialize_timeout_ms
            .unwrap_or(STDIOT_INITIALIZE_TIMEOUT_MS)
            .clamp(1_000, 600_000)
    }

    fn list_timeout_ms(&self) -> u64 {
        self.config
            .list_timeout_ms
            .unwrap_or(STDIOT_LIST_TIMEOUT_MS)
            .clamp(1_000, 600_000)
    }
}

fn apply_known_uvx_compatibility(command: &str, args: &mut Vec<String>) -> bool {
    let is_uvx = std::path::Path::new(command)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("uvx"));
    if !is_uvx
        || !args.iter().any(|arg| {
            arg == "mcp-server-fetch"
                || arg
                    .strip_prefix("mcp-server-fetch==")
                    .is_some_and(|version| !version.is_empty())
        })
    {
        return false;
    }
    let has_mcp_constraint = args.iter().enumerate().any(|(index, arg)| {
        arg.strip_prefix("--with=")
            .is_some_and(|value| value.trim_start().starts_with("mcp"))
            || (arg == "--with"
                && args
                    .get(index + 1)
                    .is_some_and(|value| value.trim_start().starts_with("mcp")))
    });
    if has_mcp_constraint {
        return false;
    }
    args.splice(0..0, ["--with".to_string(), "mcp<2".to_string()]);
    true
}

#[async_trait]
impl McpTransport for StdioTransport {
    fn kind(&self) -> McpTransportKind {
        McpTransportKind::Stdio
    }

    fn server_name(&self) -> &str {
        &self.config.server_name
    }

    fn health_check(&self) -> McpHealthStatus {
        match (self.process.try_lock(), self.initialized.try_lock()) {
            (Ok(mut process_guard), Ok(initialized_guard)) => {
                let alive = process_guard
                    .as_mut()
                    .is_some_and(|child| child.try_wait().is_ok_and(|status| status.is_none()));
                if alive {
                    if *initialized_guard {
                        McpHealthStatus::Healthy
                    } else {
                        McpHealthStatus::Degraded
                    }
                } else {
                    McpHealthStatus::Unhealthy
                }
            }
            _ => McpHealthStatus::Degraded,
        }
    }

    async fn initialize(&self) -> Result<McpInitializeResult, McpTransportError> {
        if *self.initialized.lock().await {
            return self.server_info.lock().await.clone().ok_or(
                McpTransportError::InvalidResponse {
                    details: "initialized flag set but no server info".to_string(),
                },
            );
        }

        let params = default_initialize_params();
        let timeout_ms = self.initialize_timeout_ms();
        let response: JsonRpcResponse<McpInitializeResult> = timeout(
            Duration::from_millis(timeout_ms),
            self.send_request("initialize", timeout_ms, Some(params)),
        )
        .await
        .map_err(|_| McpTransportError::Timeout {
            operation: "initialize",
            timeout_ms,
        })?
        .map_err(McpTransportError::from)?;

        if let Some(err) = &response.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message.clone(),
            });
        }

        let result = response.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in initialize response".to_string(),
        })?;

        self.send_notification("notifications/initialized", serde_json::json!({}))
            .await
            .map_err(McpTransportError::from)?;

        *self.initialized.lock().await = true;
        *self.server_info.lock().await = Some(result.clone());

        Ok(result)
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, McpTransportError> {
        self.initialize().await?;
        let mut all_tools = Vec::new();
        let mut cursor = None;

        loop {
            let timeout_ms = self.list_timeout_ms();
            let response: JsonRpcResponse<McpListToolsResult> = timeout(
                Duration::from_millis(timeout_ms),
                self.send_request(
                    "tools/list",
                    timeout_ms,
                    Some(McpListToolsParams { cursor }),
                ),
            )
            .await
            .map_err(|_| McpTransportError::Timeout {
                operation: "tools/list",
                timeout_ms,
            })?
            .map_err(McpTransportError::from)?;

            if let Some(err) = &response.error {
                return Err(McpTransportError::JsonRpc {
                    code: err.code,
                    message: err.message.clone(),
                });
            }

            let result = response.result.ok_or(McpTransportError::InvalidResponse {
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
        let timeout_ms = self.tool_call_timeout_ms();

        let response: JsonRpcResponse<McpToolCallResult> = timeout(
            Duration::from_millis(timeout_ms),
            self.send_request(
                "tools/call",
                timeout_ms,
                Some(McpToolCallParams {
                    name: name.to_string(),
                    arguments: args,
                    meta: None,
                }),
            ),
        )
        .await
        .map_err(|_| McpTransportError::Timeout {
            operation: "tools/call",
            timeout_ms,
        })?
        .map_err(McpTransportError::from)?;

        if let Some(err) = &response.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message.clone(),
            });
        }

        response.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in tools/call response".to_string(),
        })
    }

    async fn list_resources(&self) -> Result<Vec<McpResource>, McpTransportError> {
        self.initialize().await?;
        let mut all_resources = Vec::new();
        let mut cursor = None;

        loop {
            let timeout_ms = self.list_timeout_ms();
            let response: JsonRpcResponse<McpListResourcesResult> = timeout(
                Duration::from_millis(timeout_ms),
                self.send_request(
                    "resources/list",
                    timeout_ms,
                    Some(McpListResourcesParams { cursor }),
                ),
            )
            .await
            .map_err(|_| McpTransportError::Timeout {
                operation: "resources/list",
                timeout_ms,
            })?
            .map_err(McpTransportError::from)?;

            if let Some(err) = &response.error {
                return Err(McpTransportError::JsonRpc {
                    code: err.code,
                    message: err.message.clone(),
                });
            }

            let result = response.result.ok_or(McpTransportError::InvalidResponse {
                details: "missing result in resources/list response".to_string(),
            })?;

            all_resources.extend(result.resources);
            match result.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(all_resources)
    }

    async fn read_resource(&self, uri: &str) -> Result<McpReadResourceResult, McpTransportError> {
        self.initialize().await?;

        let timeout_ms = self.list_timeout_ms();
        let response: JsonRpcResponse<McpReadResourceResult> = timeout(
            Duration::from_millis(timeout_ms),
            self.send_request(
                "resources/read",
                timeout_ms,
                Some(McpReadResourceParams {
                    uri: uri.to_string(),
                }),
            ),
        )
        .await
        .map_err(|_| McpTransportError::Timeout {
            operation: "resources/read",
            timeout_ms,
        })?
        .map_err(McpTransportError::from)?;

        if let Some(err) = &response.error {
            return Err(McpTransportError::JsonRpc {
                code: err.code,
                message: err.message.clone(),
            });
        }

        response.result.ok_or(McpTransportError::InvalidResponse {
            details: "missing result in resources/read response".to_string(),
        })
    }

    async fn shutdown(&mut self) -> Result<(), McpTransportError> {
        *self.initialized.lock().await = false;
        *self.server_info.lock().await = None;

        let mut process_guard = self.process.lock().await;
        if let Some(child) = process_guard.as_mut() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        *process_guard = None;

        let mut stdout_guard = self.stdout.lock().await;
        *stdout_guard = None;

        let mut stdin_guard = self.stdin.lock().await;
        *stdin_guard = None;

        Ok(())
    }

    fn next_request_id(&self) -> JsonRpcId {
        let mut guard = self
            .next_id
            .try_lock()
            .expect("next_request_id helper should not be called while locked");
        let id_val = *guard;
        *guard = id_val.saturating_add(1);
        JsonRpcId::Number(id_val)
    }
}

impl StdioTransport {
    pub fn command(&self) -> &str {
        &self.config.command
    }
    pub fn args(&self) -> &[String] {
        &self.config.args
    }
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.config.env
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn uvx_fetch_applies_current_mcp_sdk_compatibility_without_overriding_user_constraint() {
        let mut args = vec!["mcp-server-fetch".to_string()];
        assert!(apply_known_uvx_compatibility("uvx", &mut args));
        assert_eq!(args, vec!["--with", "mcp<2", "mcp-server-fetch"]);

        let mut explicit = vec![
            "--with".to_string(),
            "mcp==1.25.0".to_string(),
            "mcp-server-fetch".to_string(),
        ];
        assert!(!apply_known_uvx_compatibility("uvx", &mut explicit));
        assert_eq!(explicit[1], "mcp==1.25.0");
    }

    #[tokio::test]
    #[ignore = "requires uvx and network access to resolve mcp-server-fetch"]
    async fn real_uvx_fetch_from_original_user_config_initializes_and_lists_tools() {
        let mut transport = StdioTransport::new(StdioTransportConfig {
            server_name: "fetch".to_string(),
            command: "uvx".to_string(),
            args: vec!["mcp-server-fetch".to_string()],
            env: BTreeMap::new(),
            initialize_timeout_ms: Some(180_000),
            list_timeout_ms: Some(60_000),
            tool_call_timeout_ms: Some(60_000),
        })
        .expect("spawn uvx mcp-server-fetch from the original user JSON");

        let tools = transport
            .list_tools()
            .await
            .expect("initialize mcp-server-fetch and list tools");
        assert!(tools.iter().any(|tool| tool.name == "fetch"));
        transport.shutdown().await.expect("shutdown real fetch MCP");
    }

    #[tokio::test]
    async fn stdio_uses_ndjson_and_sends_initialized_before_listing_tools() {
        let server = r#"
IFS= read -r initialize
case "$initialize" in *'"method":"initialize"'*) ;; *) exit 21 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1.0"}}}'
IFS= read -r initialized
case "$initialized" in *'"method":"notifications/initialized"'*) ;; *) exit 22 ;; esac
IFS= read -r list_tools
case "$list_tools" in *'"method":"tools/list"'*) ;; *) exit 23 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"weather","description":"fixture weather","inputSchema":{"type":"object"}}]}}'
"#;
        let mut transport = StdioTransport::new(StdioTransportConfig {
            server_name: "fixture".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), server.to_string()],
            env: BTreeMap::new(),
            initialize_timeout_ms: Some(2_000),
            list_timeout_ms: Some(2_000),
            tool_call_timeout_ms: Some(2_000),
        })
        .expect("spawn fixture MCP server");

        let tools = transport.list_tools().await.expect("list fixture tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "weather");
        transport.shutdown().await.expect("shutdown fixture server");
    }

    #[tokio::test]
    async fn stdio_initialize_honors_configured_timeout_beyond_ten_seconds() {
        let server = r#"
sleep 12
IFS= read -r initialize
case "$initialize" in *'"method":"initialize"'*) ;; *) exit 31 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"slow-fixture","version":"1.0"}}}'
IFS= read -r initialized
case "$initialized" in *'"method":"notifications/initialized"'*) ;; *) exit 32 ;; esac
IFS= read -r list_tools
case "$list_tools" in *'"method":"tools/list"'*) ;; *) exit 33 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
"#;
        let mut transport = StdioTransport::new(StdioTransportConfig {
            server_name: "slow-fixture".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), server.to_string()],
            env: BTreeMap::new(),
            initialize_timeout_ms: Some(20_000),
            list_timeout_ms: Some(2_000),
            tool_call_timeout_ms: Some(2_000),
        })
        .expect("spawn delayed fixture MCP server");

        let tools = transport
            .list_tools()
            .await
            .expect("configured initialize timeout should allow a 12 second cold start");
        assert!(tools.is_empty());
        transport
            .shutdown()
            .await
            .expect("shutdown delayed fixture");
    }

    #[tokio::test]
    async fn stdio_tool_discovery_uses_its_own_timeout() {
        let server = r#"
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"list-timeout-fixture","version":"1.0"}}}'
IFS= read -r initialized
IFS= read -r list_tools
sleep 2
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}'
"#;
        let mut transport = StdioTransport::new(StdioTransportConfig {
            server_name: "list-timeout-fixture".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), server.to_string()],
            env: BTreeMap::new(),
            initialize_timeout_ms: Some(2_000),
            list_timeout_ms: Some(100),
            tool_call_timeout_ms: Some(2_000),
        })
        .expect("spawn tool discovery timeout fixture");

        let error = transport
            .list_tools()
            .await
            .expect_err("tools/list must use the shorter discovery timeout");
        assert!(error.to_string().contains("tools/list"));
        transport
            .shutdown()
            .await
            .expect("shutdown timeout fixture");
    }

    #[tokio::test]
    async fn stdio_process_failure_includes_bounded_redacted_stderr() {
        let server = r#"
printf '%s\n' "startup failed: API_KEY=$MCP_TEST_SECRET" >&2
i=0
while [ "$i" -lt 600 ]; do
  printf 'diagnostic-%04d-xxxxxxxxxxxxxxxx\n' "$i" >&2
  i=$((i + 1))
done
printf '%s\n' "final failure: API_KEY=$MCP_TEST_SECRET" >&2
exit 17
"#;
        let secret = "sk-sensitive-mcp-fixture-value";
        let mut transport = StdioTransport::new(StdioTransportConfig {
            server_name: "stderr-fixture".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), server.to_string()],
            env: BTreeMap::from([("MCP_TEST_SECRET".to_string(), secret.to_string())]),
            initialize_timeout_ms: Some(2_000),
            list_timeout_ms: Some(2_000),
            tool_call_timeout_ms: Some(2_000),
        })
        .expect("spawn stderr fixture MCP server");

        let error = transport
            .initialize()
            .await
            .expect_err("fixture must fail before initialize response")
            .to_string();
        assert!(error.contains("stderr:"));
        assert!(error.contains("diagnostic-"));
        assert!(!error.contains(secret));
        assert!(error.chars().count() < 4_000);
        transport.shutdown().await.expect("shutdown stderr fixture");
    }
}
