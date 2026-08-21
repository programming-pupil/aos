//! SSE event types sent from the agent runtime to the browser.
//!
//! These events mirror the `AssistantEvent` stream from `ConversationRuntime`
//! but are serialized into SSE format for the browser.
//!
//! The frontend can render:
//! - Text deltas (streaming text)
//! - Thinking blocks (Claude's reasoning)
//! - Tool calls (bash, read, write, edit, etc.)
//! - Tool results
//! - Usage (token consumption)
//! - Session metadata (MCP servers, Skills, config) on session start
//! - Hot-reload notifications when MCP/Skills are updated

use runtime::TokenUsage;
use serde::{Deserialize, Serialize};

/// The SSE event envelope sent to the browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum AgentEvent {
    // ── Stream lifecycle ────────────────────────────────────────────────
    /// The runtime persisted a new turn checkpoint. Durable parent
    /// coordinators use this id to recover without duplicating user input.
    TurnStarted { runtime_turn_id: String },
    /// Sent when the stream starts.
    StreamStart { session_id: String },
    /// Sent when the stream ends successfully.
    StreamEnd {
        session_id: String,
        stop_reason: Option<String>,
    },
    /// Sent when an error occurs.
    Error { message: String },

    // ── Model output ─────────────────────────────────────────────────────
    /// A text delta (part of a streaming response).
    TextDelta { index: u32, text: String },
    /// A thinking block started (Claude's internal reasoning).
    ThinkingStart { index: u32 },
    /// Thinking content delta.
    ThinkingDelta { index: u32, text: String },
    /// Thinking block ended.
    ThinkingEnd { index: u32 },
    /// A text content block started.
    TextBlockStart { index: u32 },
    /// A text content block ended.
    TextBlockEnd { index: u32 },

    // ── Tool calls ──────────────────────────────────────────────────────
    /// The model requested a tool.
    ToolUseStart {
        index: u32,
        id: String,
        name: String,
    },
    /// Tool call input (JSON).
    ToolUseInput { index: u32, input: String },
    /// Tool call completed.
    ToolUseEnd { index: u32 },
    /// Tool result from the executor.
    ToolResult {
        index: u32,
        tool_name: String,
        /// The raw JSON input the model sent with this tool call.
        input: String,
        output: String,
        is_error: bool,
    },
    /// The turn is durably suspended until the authenticated owner resolves
    /// this request. No raw tool input is exposed to the browser.
    ApprovalRequired {
        turn_id: String,
        invocation_id: String,
        tool_name: String,
        current_mode: String,
        required_mode: String,
        reason: Option<String>,
    },

    // ── Token usage ─────────────────────────────────────────────────────
    /// Token usage from a completed turn.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_tokens: u32,
        cache_read_tokens: u32,
    },

    // ── Session state ───────────────────────────────────────────────────
    /// Session was compacted (summary generated).
    SessionCompacted {
        summary: String,
        removed_messages: usize,
    },
    /// Iteration count update.
    Iteration { count: usize },

    // ── Progress ───────────────────────────────────────────────────────
    /// Hook progress event (plugin hooks, etc.).
    HookProgress {
        phase: String,
        detail: Option<String>,
    },

    // ── Ping ────────────────────────────────────────────────────────────
    /// Keep-alive ping (sent every 30s during long operations).
    Ping { timestamp: i64 },

    // ── Session metadata (emitted once at session start) ─────────────────
    /// Emitted when a session is first created — informs the frontend which
    /// MCP servers and Skills are active for this session. Enables the UI to
    /// display the active configuration tags without needing a separate API call.
    SessionActivated {
        /// Human-readable name of the MCP servers active in this session.
        mcp_servers: Vec<String>,
        /// Names of the Skills active in this session.
        skills: Vec<String>,
        /// Permission mode in effect (e.g. `read_only`, `workspace_write`, `danger_full_access`).
        permission_mode: String,
        /// Model being used.
        model: String,
    },

    /// Emitted when the session's runtime was hot-reloaded before the turn
    /// (e.g. because an operator added/removed a MCP server or Skill from the `WebUI`).
    /// The UI should update the active configuration display.
    ConfigHotReload {
        /// Updated list of active MCP servers.
        mcp_servers: Vec<String>,
        /// Updated list of active Skills.
        skills: Vec<String>,
        /// Updated permission mode.
        permission_mode: String,
        /// Updated model.
        model: String,
    },
}

impl AgentEvent {
    /// Build the SSE `data:` line for this event.
    #[must_use]
    pub fn sse_data(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"type":"error","message":"json encode failed"}"#.to_string())
    }

    /// The SSE `event:` name (used for `EventSource` routing on the client).
    #[must_use]
    pub fn sse_event(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => "turn_started",
            Self::StreamStart { .. } => "stream_start",
            Self::StreamEnd { .. } => "stream_end",
            Self::Error { .. } => "error",
            Self::TextDelta { .. } => "text_delta",
            Self::ThinkingStart { .. } => "thinking_start",
            Self::ThinkingDelta { .. } => "thinking_delta",
            Self::ThinkingEnd { .. } => "thinking_end",
            Self::TextBlockStart { .. } => "text_block_start",
            Self::TextBlockEnd { .. } => "text_block_end",
            Self::ToolUseStart { .. } => "tool_use_start",
            Self::ToolUseInput { .. } => "tool_use_input",
            Self::ToolUseEnd { .. } => "tool_use_end",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::Usage { .. } => "usage",
            Self::SessionCompacted { .. } => "session_compacted",
            Self::Iteration { .. } => "iteration",
            Self::HookProgress { .. } => "hook_progress",
            Self::Ping { .. } => "ping",
            Self::SessionActivated { .. } => "session_activated",
            Self::ConfigHotReload { .. } => "config_hot_reload",
        }
    }

    /// Convert from `runtime::AssistantEvent`.
    #[must_use]
    pub fn from_runtime_event(event: runtime::AssistantEvent, block_index: u32) -> Option<Self> {
        use runtime::AssistantEvent::{
            MessageStop, PromptCache, SignatureDelta, TextDelta, ThinkingDelta, ToolUse, Usage,
        };
        match event {
            TextDelta(text) => Some(Self::TextDelta {
                index: block_index,
                text,
            }),
            ThinkingDelta(thinking) => Some(Self::ThinkingDelta {
                index: block_index,
                text: thinking,
            }),
            // The thinking-block signature is an internal artifact echoed back
            // to the provider on later turns; it is not surfaced to the client.
            SignatureDelta(_) => None,
            ToolUse { id, name, input: _ } => Some(Self::ToolUseStart {
                index: block_index,
                id,
                name,
            }),
            Usage(usage) => Some(Self::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_creation_tokens: usage.cache_creation_input_tokens,
                cache_read_tokens: usage.cache_read_input_tokens,
            }),
            PromptCache(evt) => Some(Self::HookProgress {
                phase: "prompt_cache".to_string(),
                detail: Some(format!(
                    "cache read: {} tokens, creation: {} tokens",
                    evt.current_cache_read_input_tokens, evt.token_drop
                )),
            }),
            MessageStop => None,
        }
    }
}

/// Metadata about the active configuration for a session.
/// Populated at session creation time and included in `SessionActivated` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// Names of MCP servers active in this session.
    pub mcp_servers: Vec<String>,
    /// Names of Skills active in this session.
    pub skills: Vec<String>,
    /// Effective permission mode.
    pub permission_mode: String,
    /// Model identifier.
    pub model: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    pub session_id: String,
    /// The final text response from the model.
    pub text: String,
    /// Full thinking/reasoning content from extended thinking blocks (e.g. Claude's internal reasoning).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// All tool calls made in this turn.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Token usage.
    pub usage: TokenUsageRecord,
    /// Whether the session was compacted.
    pub compacted: Option<CompactionRecord>,
    /// Number of model turns (iterations).
    pub iterations: usize,
    /// Active session metadata (MCP servers, Skills, permission mode, model).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SessionMetadata>,
    /// Whether the runtime was hot-reloaded before this turn (MCP/Skills changed).
    #[serde(default)]
    pub hot_reloaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub index: u32,
    pub tool_name: String,
    /// Source of the tool call: "mcp", "skill", or "builtin".
    pub source: String,
    /// For MCP tools: the server name. For skill tools: the skill name.
    /// Empty for built-in tools.
    pub source_name: String,
    pub input: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageRecord {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_read_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub model: String,
}

impl TokenUsageRecord {
    pub fn from_runtime(usage: TokenUsage, model: &str) -> Self {
        let total = usage.total_tokens();
        let pricing = runtime::pricing_for_model(model)
            .unwrap_or_else(runtime::ModelPricing::default_sonnet_tier);
        let cost = usage
            .estimate_cost_usd_with_pricing(pricing)
            .total_cost_usd();

        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            total_tokens: total,
            estimated_cost_usd: cost,
            model: model.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionRecord {
    pub summary: String,
    pub removed_messages: usize,
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_tail_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_compaction_endpoint_supported: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_compaction_endpoint_called: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_compaction_output_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_compaction_fallback_reason: Option<String>,
}

/// Internal result of `run_streaming_turn_streaming` — the turn summary plus accumulated
/// tool records (needed by the session manager to record usage and build `TurnResult`).
#[derive(Debug)]
pub struct StreamingTurnResult {
    pub turn_summary: runtime::TurnSummary,
    pub tool_records: Vec<ToolCallRecord>,
    pub hook_events: Vec<AgentEvent>,
}
