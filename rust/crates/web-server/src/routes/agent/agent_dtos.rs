use super::*;

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub project_id: Option<String>,
    pub model: Option<String>,
    /// Source app that created this session: "chat" or "agent".
    pub source: Option<String>,
    /// Scenario tag used to filter API keys (e.g. "agent", "chat", "nl2sql").
    /// The API key must have NULL scenarios (all) or contain this value.
    pub scenario: Option<String>,
    /// UI locale used for default session naming.
    pub locale: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    /// Filter sessions by source (e.g. "chat" or "agent"). If omitted, returns all.
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetSessionHistoryQuery {
    /// Turn cursor (exclusive). Omit to load latest page.
    pub before_turn_cursor: Option<usize>,
    /// Max turns in one page (soft cap, bounded on server).
    pub limit_turns: Option<usize>,
    /// Max approximate payload bytes in one page (hard cap, bounded on server).
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session: SessionDto,
}

#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub session_id: String,
    pub name: String,
    pub user_id: String,
    pub tenant_id: String,
    pub workspace: String,
    pub model: String,
    pub created_at: String,
    pub is_pinned: bool,
    pub is_bookmarked: bool,
    pub source: String,
    /// Names of MCP servers active for this session.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,
    /// Names of Skills active for this session.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Permission mode.
    pub permission_mode: String,
}

/// DTO for session history response — maps internal message types to API format.
#[derive(Debug, Serialize)]
pub struct SessionHistoryResponse {
    pub session_id: String,
    pub messages: Vec<MessageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<SessionHistoryPageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm_research: Option<PmSessionHistoryReplayDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub super_assistant_turns: Option<Vec<SuperAssistantTurnMessageMetadataDto>>,
}

#[derive(Debug, Serialize)]
pub struct SuperAssistantTurnMessageMetadataDto {
    pub turn_id: String,
    pub model: String,
    pub final_text: String,
    pub route_capability: Option<String>,
    pub adversarial_run_id: Option<String>,
    pub judge_model: Option<String>,
    pub winner_model: Option<String>,
    pub winner_reason: Option<String>,
    pub attribution_task_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nl2sql_audits: Vec<SuperAssistantNl2sqlAuditDto>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SuperAssistantNl2sqlAuditDto {
    pub tool_call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub progress_events: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct SessionHistoryPageDto {
    pub before_turn_cursor: usize,
    pub next_before_turn_cursor: Option<usize>,
    pub has_more: bool,
    pub returned_turns: usize,
    pub total_turns: usize,
    pub limit_turns: usize,
    pub max_bytes: usize,
    pub approx_payload_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct PmSessionHistoryReplayDto {
    pub task_id: String,
    pub status: String,
    pub events: Vec<PmResearchTaskEvent>,
}

// `Deserialize` + `PartialEq` are derived on the session-history message DTOs so
// the conversation-history wire format can be round-tripped
// (`serialize → parse → serialize`) for the Super_Assistant serialization
// guarantees (Requirement 7.2). Optional fields omitted from the serialized form
// carry `#[serde(default)]` so parsing back is total and matches the frontend
// `AgentHistoryMessage` interface (snake_case).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MessageDto {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCallBlockDto>>,
    #[serde(default)]
    pub tool_result: Option<ToolResultBlockDto>,
    #[serde(default)]
    pub usage: Option<UsageBlockDto>,
    /// Extended thinking/reasoning content (e.g. Claude's internal reasoning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_task_status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ToolCallBlockDto {
    pub id: String,
    pub name: String,
    pub input: String,
    /// Tool execution result — populated when tool results are returned separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolResultBlockDto>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ToolResultBlockDto {
    pub tool_use_id: String,
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct UsageBlockDto {
    pub input: u32,
    pub output: u32,
    pub cache_creation: u32,
    pub cache_read: u32,
}

impl From<SessionHandle> for SessionDto {
    fn from(h: SessionHandle) -> Self {
        Self {
            session_id: h.session_id,
            name: h.name,
            user_id: h.user_id,
            tenant_id: h.tenant_id,
            workspace: h.workspace.to_string_lossy().to_string(),
            model: h.model,
            created_at: h.created_at.to_rfc3339(),
            is_pinned: h.is_pinned,
            is_bookmarked: h.is_bookmarked,
            source: h.source,
            mcp_servers: h.session_metadata.mcp_servers.clone(),
            skills: h.session_metadata.skills.clone(),
            permission_mode: h.session_metadata.permission_mode.clone(),
        }
    }
}

impl From<SessionInfo> for SessionDto {
    fn from(s: SessionInfo) -> Self {
        Self {
            session_id: s.session_id,
            name: s.name,
            user_id: String::new(),
            tenant_id: String::new(),
            workspace: s.workspace.to_string_lossy().to_string(),
            model: s.model,
            created_at: s.created_at.to_rfc3339(),
            is_pinned: s.is_pinned,
            is_bookmarked: s.is_bookmarked,
            source: s.source,
            mcp_servers: s.session_metadata.mcp_servers.clone(),
            skills: s.session_metadata.skills.clone(),
            permission_mode: s.session_metadata.permission_mode.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RunTurnRequest {
    pub message: String,
    #[expect(dead_code)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnOptions {
    #[serde(default)]
    pub search_mode: ChatSearchMode,
    #[serde(default)]
    pub search_enabled: bool,
    #[serde(default)]
    pub file_context: Option<ChatFileContextOptions>,
    #[serde(default)]
    pub memory_mode: ChatMemoryMode,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatSearchMode {
    /// Backward-compatible legacy value. AI Chat no longer auto-enables web
    /// search; legacy `auto` requests are treated as `off` by the resolver.
    Auto,
    On,
    #[default]
    Off,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileContextOptions {
    #[serde(default)]
    pub mode: ChatFileContextMode,
    #[serde(default)]
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub strict_grounding: bool,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatFileContextMode {
    #[default]
    None,
    Selected,
    AllAttached,
    Workspace,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatMemoryMode {
    #[default]
    Auto,
    Off,
    PinnedOnly,
}

#[derive(Debug, Serialize)]
pub struct RunTurnResponse {
    pub session_id: String,
    pub text: String,
    pub tool_calls: Vec<ToolCallDto>,
    pub usage: UsageDto,
    pub iterations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacted: Option<agent_gateway::CompactionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SessionActivatedDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm_quality: Option<PmAnswerQualityDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm_report: Option<PmReportArtifactDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSessionTurnResponse {
    pub session_id: String,
    pub cancelled: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmTaskImageInput {
    pub url: String,
    #[serde(default, alias = "file_id")]
    pub file_id: Option<String>,
    #[serde(default, alias = "media_type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "size")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmTaskDocumentInput {
    pub url: String,
    #[serde(default, alias = "file_id")]
    pub file_id: Option<String>,
    #[serde(default, alias = "media_type")]
    pub media_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, alias = "size")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PmTaskInputContext {
    #[serde(default)]
    pub images: Vec<PmTaskImageInput>,
    #[serde(default)]
    pub documents: Vec<PmTaskDocumentInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPmResearchTaskRequest {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub images: Vec<PmTaskImageInput>,
    #[serde(default, alias = "attachments")]
    pub documents: Vec<PmTaskDocumentInput>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPmResearchTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelPmResearchTaskResponse {
    pub task_id: String,
    pub status: String,
    pub cancel_requested: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumePmResearchTaskResponse {
    pub task_id: String,
    pub status: String,
    pub restarted: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PmResearchTaskEvent {
    pub task_id: String,
    pub session_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PmResearchTaskStreamEvent {
    pub task_id: String,
    pub session_id: String,
    pub stage: String,
    pub sequence: u64,
    pub delta: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmResearchTaskStatusResponse {
    pub task_id: String,
    pub session_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub attempt: Option<usize>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    pub detail: Option<serde_json::Value>,
    pub response: Option<serde_json::Value>,
    pub error: Option<String>,
    pub cancel_requested: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmStrategyRecordRequest {
    pub route: String,
    pub channel: Option<String>,
    pub variant: Option<String>,
    pub passed: bool,
    pub citation_count: Option<f64>,
    pub domain_count: Option<f64>,
    pub tool_call_count: Option<f64>,
    pub retrieve_duration_ms: Option<f64>,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmStrategyLeaderboardItem {
    pub route: String,
    pub channel: Option<String>,
    pub run_count: i64,
    pub success_rate: f64,
    pub avg_quality: f64,
    pub avg_cost: f64,
    pub avg_retrieve_duration_ms: f64,
    pub score: f64,
    pub last_run_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmStrategyLeaderboardResponse {
    pub rows: Vec<PmStrategyLeaderboardItem>,
}

#[derive(Debug, Serialize)]
pub struct ToolCallDto {
    pub index: u32,
    pub tool_name: String,
    pub source: String,
    pub source_name: String,
    pub input: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionActivatedDto {
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub permission_mode: String,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct UsageDto {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_read_tokens: u32,
    pub total_tokens: u32,
    pub estimated_cost_usd: f64,
    pub model: String,
}

impl From<agent_gateway::ToolCallRecord> for ToolCallDto {
    fn from(tc: agent_gateway::ToolCallRecord) -> Self {
        Self {
            index: tc.index,
            tool_name: tc.tool_name,
            source: tc.source,
            source_name: tc.source_name,
            input: tc.input,
            output: tc.output,
            is_error: tc.is_error,
            duration_ms: tc.duration_ms,
        }
    }
}

impl From<TokenUsageRecord> for UsageDto {
    fn from(u: TokenUsageRecord) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_tokens: u.cache_creation_tokens,
            cache_read_tokens: u.cache_read_tokens,
            total_tokens: u.total_tokens,
            estimated_cost_usd: u.estimated_cost_usd,
            model: u.model,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: u16,
}

#[cfg(test)]
mod tests {
    use super::{MessageDto, ToolCallBlockDto, ToolResultBlockDto, UsageBlockDto};

    #[test]
    fn message_dto_serializes_to_history_wire_shape() {
        let msg = MessageDto {
            role: "assistant".to_string(),
            content: "here is the answer".to_string(),
            tool_calls: Some(vec![ToolCallBlockDto {
                id: "call-1".to_string(),
                name: "search".to_string(),
                input: "{\"q\":\"rust\"}".to_string(),
                result: Some(ToolResultBlockDto {
                    tool_use_id: "call-1".to_string(),
                    tool_name: "search".to_string(),
                    output: "ok".to_string(),
                    is_error: false,
                }),
            }]),
            tool_result: None,
            usage: Some(UsageBlockDto {
                input: 10,
                output: 20,
                cache_creation: 0,
                cache_read: 0,
            }),
            thinking: Some("reasoning".to_string()),
            pm_task_id: None,
            pm_task_status: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // snake_case field names match the frontend `AgentHistoryMessage`.
        assert!(json.contains("\"tool_calls\""));
        assert!(json.contains("\"tool_use_id\":\"call-1\""));
        assert!(json.contains("\"is_error\":false"));
        // pm_task_id / pm_task_status omitted when absent.
        assert!(!json.contains("pm_task_id"));
    }

    #[test]
    fn message_dto_round_trips() {
        let msg = MessageDto {
            role: "user".to_string(),
            content: "please migrate to Postgres".to_string(),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            pm_task_id: Some("pm-1".to_string()),
            pm_task_status: Some("completed".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: MessageDto = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, parsed);
        // serialize → parse → serialize is identity-preserving (Req 7.2).
        assert_eq!(json, serde_json::to_string(&parsed).unwrap());
    }

    // --- Property 14: serialization round-trip identity (Req 7.2) -----------
    //
    // For any conversation-history message, `serialize → parse` yields an
    // equivalent object (round-trip identity). This is the conversation-history
    // arm of Property 14; the memory record (`AgentMemoryItem`) and trace events
    // (`RouteDecisionEvent` / `SessionCompactedEvent`) are covered by the
    // sibling properties in `routes/super_assistant.rs`.
    mod prop_roundtrip {
        use super::super::*;
        use proptest::prelude::*;

        prop_compose! {
            fn arb_tool_result()(
                tool_use_id in any::<String>(),
                tool_name in any::<String>(),
                output in any::<String>(),
                is_error in any::<bool>(),
            ) -> ToolResultBlockDto {
                ToolResultBlockDto { tool_use_id, tool_name, output, is_error }
            }
        }

        prop_compose! {
            fn arb_tool_call()(
                id in any::<String>(),
                name in any::<String>(),
                input in any::<String>(),
                result in proptest::option::of(arb_tool_result()),
            ) -> ToolCallBlockDto {
                ToolCallBlockDto { id, name, input, result }
            }
        }

        prop_compose! {
            fn arb_usage()(
                input in any::<u32>(),
                output in any::<u32>(),
                cache_creation in any::<u32>(),
                cache_read in any::<u32>(),
            ) -> UsageBlockDto {
                UsageBlockDto { input, output, cache_creation, cache_read }
            }
        }

        prop_compose! {
            fn arb_message_dto()(
                role in any::<String>(),
                content in any::<String>(),
                tool_calls in proptest::option::of(
                    proptest::collection::vec(arb_tool_call(), 0..4)
                ),
                tool_result in proptest::option::of(arb_tool_result()),
                usage in proptest::option::of(arb_usage()),
                thinking in proptest::option::of(any::<String>()),
                pm_task_id in proptest::option::of(any::<String>()),
                pm_task_status in proptest::option::of(any::<String>()),
            ) -> MessageDto {
                MessageDto {
                    role,
                    content,
                    tool_calls,
                    tool_result,
                    usage,
                    thinking,
                    pm_task_id,
                    pm_task_status,
                }
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            // Feature: super-assistant-hub, Property 14: 序列化往返恒等
            // Validates: Requirements 7.2
            #[test]
            fn prop_roundtrip_message_dto(msg in arb_message_dto()) {
                let json = serde_json::to_string(&msg).unwrap();
                let parsed: MessageDto = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&msg, &parsed);
                prop_assert_eq!(json, serde_json::to_string(&parsed).unwrap());
            }
        }
    }
}
