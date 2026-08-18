use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use telemetry::SessionTracer;

use crate::compact::{
    compact_session, compact_session_with_summary, CompactionConfig, CompactionResult,
};
use crate::config::RuntimeFeatureConfig;
use crate::execution_kernel::{
    AgentExecutionKernel, RuntimeApprovalDecision, RuntimeApprovalRequest,
    RuntimeApprovalResolution, RuntimeContextManifestInput, RuntimeContextSupplement,
    RuntimeContextSupplementRequest, RuntimeInteractionRequest, RuntimeModelBudgetStage,
    RuntimeToolContract, RuntimeToolIntent, RuntimeToolOutcome, RuntimeToolOutcomeKind,
    RuntimeTurnStart, RuntimeTurnTerminalStatus,
};
use crate::hooks::{HookAbortSignal, HookProgressReporter, HookRunResult, HookRunner};
use crate::permissions::{
    PermissionContext, PermissionOutcome, PermissionPolicy, PermissionPrompter,
};
use crate::session::{ContentBlock, ConversationMessage, MessageRole, Session, SessionTurnStatus};
use crate::token_estimator::{
    estimate_message_tokens, estimate_session_tokens, estimate_text_tokens,
};
use crate::trident::{trident_compact_session, TridentConfig};
use crate::usage::{TokenUsage, UsageTracker};

const DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD: u32 = 100_000;
const AUTO_COMPACTION_THRESHOLD_ENV_VAR: &str = "CLAUDE_CODE_AUTO_COMPACT_INPUT_TOKENS";
const TRIDENT_COMPACTION_ENV_VAR: &str = "AOS_RUNTIME_TRIDENT_COMPACTION";
const CONTEXT_COMPILER_MAX_TOKENS_ENV_VAR: &str = "AOS_CONTEXT_COMPILER_MAX_TOKENS";
static PROVIDER_REQUEST_GROUP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn context_compiler_max_tokens() -> u64 {
    std::env::var(CONTEXT_COMPILER_MAX_TOKENS_ENV_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(131_072)
        .clamp(1_024, 1_048_576)
}

#[derive(Debug)]
struct CompiledModelVisibleRequest {
    system_prompt: Vec<String>,
    messages: Vec<ConversationMessage>,
    context_packet: semantic_core::ContextPacket,
}

fn message_context_block(
    index: usize,
    message: &ConversationMessage,
    current_user_index: Option<usize>,
) -> semantic_core::ContextBlock {
    let is_current_user = current_user_index == Some(index);
    semantic_core::ContextBlock {
        block_id: format!("message:{index}"),
        source: if is_current_user {
            "current_user_request".to_string()
        } else {
            "recent_interaction".to_string()
        },
        content: serde_json::to_string(message).unwrap_or_else(|_| format!("message:{index}")),
        tokens: estimate_message_tokens(message) as u64,
        truncated: false,
        source_hash: hex::encode(Sha256::digest(
            serde_json::to_vec(message).unwrap_or_default(),
        )),
        policy_version: "context-compiler-v2".to_string(),
        layer: if is_current_user {
            semantic_core::PromptLayer::TaskPacket
        } else {
            semantic_core::PromptLayer::RecentInteraction
        },
        selection_reason: if is_current_user {
            "latest user objective is mandatory".into()
        } else {
            "recent interaction selected within remaining budget".into()
        },
        trust: semantic_core::ContextTrust::UntrustedData,
    }
}

#[allow(clippy::too_many_lines)]
fn compile_model_visible_context_with_budget(
    system_prompt: Vec<String>,
    messages: Vec<ConversationMessage>,
    supplement: &RuntimeContextSupplement,
    max_tokens: u64,
) -> Result<CompiledModelVisibleRequest, RuntimeError> {
    let current_user_index = messages
        .iter()
        .rposition(|message| matches!(message.role, MessageRole::User));
    let mut blocks =
        Vec::with_capacity(system_prompt.len() + messages.len() + supplement.blocks.len());
    for (index, section) in system_prompt.iter().enumerate() {
        blocks.push(semantic_core::ContextBlock {
            block_id: format!("system:{index}"),
            source: "stable_system".to_string(),
            content: section.clone(),
            tokens: estimate_text_tokens(section) as u64,
            truncated: false,
            source_hash: hex::encode(Sha256::digest(section.as_bytes())),
            policy_version: "context-compiler-v2".to_string(),
            layer: if index == 0 {
                semantic_core::PromptLayer::StableSystem
            } else {
                semantic_core::PromptLayer::DomainContract
            },
            selection_reason: if index == 0 {
                "stable system authority is mandatory".into()
            } else {
                "domain/runtime contract is mandatory".into()
            },
            trust: semantic_core::ContextTrust::Instruction,
        });
    }
    for (index, message) in messages.iter().enumerate() {
        blocks.push(message_context_block(index, message, current_user_index));
    }
    blocks.extend(supplement.blocks.iter().cloned());
    let packet = semantic_core::ContextCompiler
        .compile_for_model(
            semantic_core::ContextSelection {
                objective: "provider request".to_string(),
                envelope: supplement.envelope.clone(),
                blocks,
            },
            max_tokens,
        )
        .map_err(|error| {
            RuntimeError::new(format!("context compiler rejected request: {error}"))
        })?;
    let selected = packet
        .blocks
        .iter()
        .map(|block| block.block_id.as_str())
        .collect::<BTreeSet<_>>();
    let sections = system_prompt
        .into_iter()
        .enumerate()
        .filter_map(|(index, section)| {
            selected
                .contains(format!("system:{index}").as_str())
                .then_some(section)
        })
        .collect::<Vec<_>>();
    let selected_supplemental = supplement
        .blocks
        .iter()
        .filter(|block| selected.contains(block.block_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut selected_messages = messages
        .into_iter()
        .enumerate()
        .filter_map(|(index, message)| {
            selected
                .contains(format!("message:{index}").as_str())
                .then_some((index, message))
        })
        .collect::<Vec<_>>();
    // ToolUse/ToolResult is a transaction boundary: budget selection may keep
    // both halves or neither, but never expose a dangling protocol frame.
    let selected_tool_uses = selected_messages
        .iter()
        .flat_map(|(_, message)| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let selected_tool_results = selected_messages
        .iter()
        .flat_map(|(_, message)| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let complete_tool_transactions = selected_tool_uses
        .intersection(&selected_tool_results)
        .cloned()
        .collect::<BTreeSet<_>>();
    for (_, message) in &mut selected_messages {
        message.blocks.retain(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => {
                complete_tool_transactions.contains(tool_use_id)
            }
            ContentBlock::ToolUse { id, .. } => complete_tool_transactions.contains(id),
            ContentBlock::Text { .. } => true,
        });
    }
    selected_messages.retain(|(_, message)| !message.blocks.is_empty());
    let mut exact_blocks = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        exact_blocks.push(semantic_core::ContextBlock {
            block_id: format!("compiled-system:{index}"),
            source: if index == 0 {
                "stable_system".into()
            } else {
                "domain_contract".into()
            },
            content: section.clone(),
            tokens: estimate_text_tokens(section) as u64,
            truncated: false,
            source_hash: hex::encode(Sha256::digest(section.as_bytes())),
            policy_version: "context-compiler-v2".into(),
            layer: if index == 0 {
                semantic_core::PromptLayer::StableSystem
            } else {
                semantic_core::PromptLayer::DomainContract
            },
            selection_reason: "selected model-visible instruction section".into(),
            trust: semantic_core::ContextTrust::Instruction,
        });
    }

    // Governed state, Memory, Evidence, and Artifact excerpts are data. They
    // must never be promoted into the provider's system channel. Insert one
    // explicit data message immediately before the latest user objective so
    // the user request remains the final instruction-bearing message.
    let data_message = (!selected_supplemental.is_empty()).then(|| ConversationMessage {
        role: MessageRole::User,
        blocks: selected_supplemental
            .iter()
            .map(|block| ContentBlock::Text {
                text: block.content.clone(),
            })
            .collect(),
        thinking: None,
        thinking_signature: None,
        usage: None,
    });
    let insertion_index = selected_messages
        .iter()
        .position(|(index, _)| Some(*index) == current_user_index)
        .unwrap_or(0);
    let mut provider_messages =
        Vec::with_capacity(selected_messages.len() + usize::from(data_message.is_some()));
    for (position, (index, message)) in selected_messages.into_iter().enumerate() {
        if position == insertion_index {
            if let Some(data_message) = data_message.as_ref() {
                provider_messages.push(data_message.clone());
                exact_blocks.extend(selected_supplemental.iter().cloned());
            }
        }
        exact_blocks.push(message_context_block(index, &message, current_user_index));
        provider_messages.push(message);
    }
    if provider_messages.is_empty() {
        if let Some(data_message) = data_message {
            provider_messages.push(data_message);
            exact_blocks.extend(selected_supplemental);
        }
    }
    let mut envelope = packet.envelope;
    envelope.recent_messages = exact_blocks
        .iter()
        .filter(|block| {
            matches!(
                block.source.as_str(),
                "recent_interaction" | "current_user_request"
            )
        })
        .map(|block| semantic_core::ContextReference {
            id: block.block_id.clone(),
            version: None,
            content_hash: block.source_hash.clone(),
        })
        .collect();
    let mut context_packet = semantic_core::ContextCompiler
        .compile(
            semantic_core::ContextSelection {
                objective: packet.objective,
                envelope,
                blocks: exact_blocks,
            },
            max_tokens,
        )
        .map_err(|error| {
            RuntimeError::new(format!("context compiler rejected request: {error}"))
        })?;
    context_packet.manifest.snapshot_version = supplement.semantic_snapshot_version;
    Ok(CompiledModelVisibleRequest {
        system_prompt: sections,
        messages: provider_messages,
        context_packet,
    })
}

#[cfg(test)]
fn compile_model_visible_request_with_budget(
    system_prompt: Vec<String>,
    messages: Vec<ConversationMessage>,
    max_tokens: u64,
) -> Result<(Vec<String>, Vec<ConversationMessage>), RuntimeError> {
    let compiled = compile_model_visible_context_with_budget(
        system_prompt,
        messages,
        &RuntimeContextSupplement::default(),
        max_tokens,
    )?;
    Ok((compiled.system_prompt, compiled.messages))
}

fn should_auto_compact(
    input_tokens_since_compaction: u32,
    estimated_session_tokens: usize,
    threshold: u32,
) -> bool {
    input_tokens_since_compaction >= threshold
        || estimated_session_tokens >= usize::try_from(threshold).unwrap_or(usize::MAX)
}

fn stale_tool_alias_is_allowed(client: &impl ApiClient, tool_name: &str) -> bool {
    let canonical = tool_name.rsplit("__").next().unwrap_or(tool_name);
    canonical == "nl2sql_analyze" && client.is_tool_call_allowed("data_attribution_start")
}

/// Fully assembled request payload sent to the upstream model client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequest {
    pub system_prompt: Vec<String>,
    pub messages: Vec<ConversationMessage>,
    /// Stable identity carried through provider retargeting, native-search
    /// fallbacks and output-budget retries. The gateway persists every final
    /// wire attempt against this root before network dispatch.
    pub trace: ProviderRequestTrace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestTrace {
    pub turn_id: Option<String>,
    pub iteration: Option<usize>,
    pub request_group_id: String,
    pub context_manifest_key: Option<String>,
    pub context_manifest_id: Option<String>,
    pub prompt_manifest_id: Option<String>,
    pub context_manifest_hash: Option<String>,
}

impl ProviderRequestTrace {
    #[must_use]
    pub fn turn(turn_id: &str, iteration: usize) -> Self {
        Self {
            turn_id: Some(turn_id.to_string()),
            iteration: Some(iteration),
            request_group_id: format!("turn:{turn_id}:iteration:{iteration}"),
            context_manifest_key: Some(format!("{turn_id}:{iteration}")),
            context_manifest_id: None,
            prompt_manifest_id: None,
            context_manifest_hash: None,
        }
    }

    #[must_use]
    pub fn background(label: &str) -> Self {
        let sequence = PROVIDER_REQUEST_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            turn_id: None,
            iteration: None,
            request_group_id: format!("background:{label}:{sequence}"),
            context_manifest_key: None,
            context_manifest_id: None,
            prompt_manifest_id: None,
            context_manifest_hash: None,
        }
    }
}

/// Streamed events emitted while processing a single assistant turn.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AssistantEvent {
    TextDelta(String),
    /// A delta of extended thinking/reasoning content (e.g. Claude's internal reasoning).
    /// This is accumulated in real-time via SSE and also stored in the session.
    ThinkingDelta(String),
    /// The cryptographic signature for the preceding thinking block. Anthropic
    /// emits this once per thinking block and requires it to be echoed back on
    /// subsequent turns to preserve extended-thinking continuity.
    SignatureDelta(String),
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    Usage(TokenUsage),
    PromptCache(PromptCacheEvent),
    MessageStop,
}

/// Prompt-cache telemetry captured from the provider response stream.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromptCacheEvent {
    pub unexpected: bool,
    pub reason: String,
    pub previous_cache_read_input_tokens: u32,
    pub current_cache_read_input_tokens: u32,
    pub token_drop: u32,
}

/// Minimal streaming API contract required by [`ConversationRuntime`].
#[::async_trait::async_trait]
pub trait ApiClient: Send + Sync {
    /// Model-specific prompt variant selected by the production registry.
    /// Its sections are authoritative input, not audit-only metadata.
    fn prompt_variant(&self) -> Option<agent_protocol::PromptVariant> {
        None
    }

    fn context_domain(&self) -> String {
        "general".into()
    }

    /// Provider/model identifier and currently exposed tools for the durable
    /// prompt manifest. Generic clients may omit both.
    fn model_version(&self) -> Option<String> {
        None
    }

    fn active_tool_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// Effective provider context window. The compiler reserves headroom for
    /// tool schemas and output before constructing the actual request.
    fn context_window_tokens(&self) -> Option<u64> {
        None
    }

    /// Apply a per-iteration request policy before the next model call.
    ///
    /// The default keeps generic clients unchanged. Gateway clients use this
    /// seam to stop exposing exhausted tool families while still allowing the
    /// model to synthesize a final answer in the same runtime turn.
    fn prepare_model_iteration(
        &mut self,
        _iteration: usize,
        _completed_tool_names: &[String],
    ) -> Option<String> {
        None
    }

    /// Selects the protected budget class for the request after iteration
    /// policy has been applied. Generic clients only enter final synthesis
    /// when they explicitly emitted a close-out instruction; domain adapters
    /// may override this for verifier or user-visible recovery calls.
    fn model_budget_stage(
        &self,
        _iteration: usize,
        _completed_tool_names: &[String],
        iteration_instruction: Option<&str>,
    ) -> RuntimeModelBudgetStage {
        if iteration_instruction.is_some() {
            RuntimeModelBudgetStage::FinalSynthesis
        } else {
            RuntimeModelBudgetStage::General
        }
    }

    /// Whether a model-emitted tool call is still available for this iteration.
    /// Providers can emit a stale/hallucinated call even after its schema was
    /// removed; the runtime must enforce availability before execution.
    fn is_tool_call_allowed(&self, _tool_name: &str) -> bool {
        true
    }

    /// Removes a repeatedly failing tool from subsequent model iterations in
    /// the current turn. Clients that expose dynamic tool schemas should hide
    /// the tool immediately; the runtime also enforces the block locally.
    fn block_tool_for_turn(&mut self, _tool_name: &str) {}

    /// Activate tools returned by the capability router's discovery call for
    /// subsequent model iterations in this turn.
    fn activate_tool_candidates(&mut self, _tool_names: &[String]) {}

    async fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError>;

    async fn stream_with_reporter(
        &mut self,
        request: ApiRequest,
        reporter: &mut (dyn RuntimeEventReporter + Send),
    ) -> Result<Vec<AssistantEvent>, RuntimeError> {
        let events = self.stream(request).await?;
        replay_assistant_events_to_reporter(&events, reporter);
        Ok(events)
    }
}

fn latest_user_objective(messages: &[ConversationMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, MessageRole::User))
        .map(|message| {
            message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "continue current turn".into())
}

/// Trait implemented by tool dispatchers that execute model-requested tools.
pub trait ToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError>;

    fn tool_contract(&self, _tool_name: &str) -> Option<RuntimeToolContract> {
        None
    }

    /// Production registries opt into fail-closed contract enforcement. Test
    /// and compatibility executors remain explicit about not providing the
    /// production tool surface.
    fn requires_tool_contracts(&self) -> bool {
        false
    }

    /// Execute a tool with support for durable, externally-completed calls.
    ///
    /// Existing executors remain synchronous through the default implementation.
    /// Orchestrators may return [`ToolExecutionOutcome::Deferred`] for work that
    /// must outlive the current process/request (for example a research job).
    fn execute_outcome(&mut self, tool_name: &str, input: &str) -> ToolExecutionOutcome {
        ToolExecutionOutcome::Completed(self.execute(tool_name, input))
    }

    fn supports_parallel_tool_calls(&self, _tool_name: &str) -> bool {
        false
    }

    fn execute_batch(
        &mut self,
        requests: Vec<ToolExecutionRequest>,
    ) -> Vec<Result<String, ToolError>> {
        requests
            .into_iter()
            .map(|request| self.execute(&request.tool_name, &request.input))
            .collect()
    }
}

fn resolve_executor_tool_contract<E: ToolExecutor>(
    executor: &E,
    tool_name: &str,
) -> Result<RuntimeToolContract, RuntimeError> {
    crate::behavior_trace("TOOL-001");
    let contract = match executor.tool_contract(tool_name) {
        Some(contract) => contract,
        None if !executor.requires_tool_contracts() => {
            RuntimeToolContract::test_read_only(tool_name)
        }
        None => {
            return Err(RuntimeError::new(format!(
                "tool `{tool_name}` is unavailable because its production lifecycle contract is missing"
            )))
        }
    };
    contract.validate(tool_name)?;
    Ok(contract)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionRequest {
    pub tool_name: String,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolExecutionOutcome {
    Completed(Result<String, ToolError>),
    Deferred { metadata: String },
}

impl ToolExecutionOutcome {
    #[must_use]
    pub fn deferred(metadata: impl Into<String>) -> Self {
        Self::Deferred {
            metadata: metadata.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredToolUse {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: String,
    pub metadata: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredToolResult {
    pub tool_use_id: String,
    pub output: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredApprovalDecision {
    pub tool_use_id: String,
    pub decision: RuntimeApprovalDecision,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SuspendedTurn {
    pub turn_id: String,
    pub deferred_tools: Vec<DeferredToolUse>,
    pub partial_summary: TurnSummary,
}

#[derive(Debug, Clone)]
pub enum ResumableTurnOutcome {
    Completed(TurnSummary),
    Suspended(SuspendedTurn),
}

#[derive(Debug, Default)]
struct TurnAccumulator {
    assistant_messages: Vec<ConversationMessage>,
    tool_results: Vec<ConversationMessage>,
    prompt_cache_events: Vec<PromptCacheEvent>,
    iterations: usize,
    auto_compaction: Option<AutoCompactionEvent>,
    repeated_tool_failures: BTreeMap<String, usize>,
    failure_blocked_tools: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedToolUse {
    tool_use_id: String,
    tool_name: String,
    effective_input: String,
    pre_hook_result: HookRunResult,
    permission_outcome: PermissionOutcome,
    contract: RuntimeToolContract,
}

enum PreparedToolExecution {
    Completed(ConversationMessage),
    Deferred(DeferredToolUse),
}

impl PreparedToolUse {
    fn is_allowed_parallel<E: ToolExecutor>(&self, executor: &E) -> bool {
        matches!(self.permission_outcome, PermissionOutcome::Allow)
            && self.contract.can_parallel
            && executor.supports_parallel_tool_calls(&self.tool_name)
    }
}

/// Error returned when a tool invocation fails locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ToolError {}

/// Error returned when a conversation turn cannot be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    message: String,
}

impl RuntimeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RuntimeError {}

/// Summary of one completed runtime turn, including tool results and usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub assistant_messages: Vec<ConversationMessage>,
    pub tool_results: Vec<ConversationMessage>,
    pub prompt_cache_events: Vec<PromptCacheEvent>,
    pub iterations: usize,
    pub usage: TokenUsage,
    pub auto_compaction: Option<AutoCompactionEvent>,
}

/// Details about automatic session compaction applied during a turn.
///
/// This is the field-complete payload for the `session_compacted` event
/// (Req 1.5). It carries everything a higher layer needs to emit exactly one
/// observable/auditable compaction event with `removedMessages`,
/// `summaryTokens`, and `retainedTail`:
///
/// * [`removed_message_count`](Self::removed_message_count) — `removedMessages`,
///   the number of messages summarized out of the live context (always `> 0`
///   for an emitted event).
/// * [`summary_tokens`](Self::summary_tokens) — `summaryTokens`, the estimated
///   token size of the generated compaction summary.
/// * [`retained_tail`](Self::retained_tail) — `retainedTail`, the recent tail
///   messages preserved **verbatim** across compaction (the `retainedTailTokens`
///   window). These are reused directly from the shared compaction engine's
///   output, never re-derived, so callers can assert the tail is preserved
///   byte-for-byte.
/// * [`archived_messages`](Self::archived_messages) — the verbatim exact-archive
///   of the removed window (`archive_windows` semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCompactionEvent {
    pub removed_message_count: usize,
    pub archived_messages: Vec<ConversationMessage>,
    /// Estimated token count of the compaction summary (`summaryTokens`).
    pub summary_tokens: usize,
    /// The recent tail messages preserved verbatim across compaction
    /// (`retainedTail` / the `retainedTailTokens` window).
    pub retained_tail: Vec<ConversationMessage>,
}

/// Durable preparation returned by a [`CompactionHook`].  The opaque id binds
/// the pure source window to the eventual commit/abort decision without making
/// the runtime depend on a storage implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCompaction {
    pub transaction_id: String,
    pub replacement_summary: Option<String>,
}

/// Injection seam for the transactional compaction coordinator.
///
/// `conversation.rs` lives in the `runtime` crate and must not depend on the
/// `web-server` crate where the memory / persistence orchestration
/// (`orchestrate_compaction` / `extract_key_info` / `persist_key_info`) lives.
/// Higher layers implement this trait and inject it via
/// [`ConversationRuntime::with_compaction_hook`] so that, when the runtime's
/// auto-compaction trigger fires, the source and derived candidates are first
/// prepared without changing projections.  Only after the runtime validates
/// the replacement boundary and growth invariant may the hook atomically
/// commit Memory, cursor, exact archive, checkpoint and Ledger projections.
///
/// Reuse-first: the runtime never reimplements memory or persistence — it only
/// exposes this strategy point and reuses [`compact_session`] /
/// [`compact_session_with_summary`] to commit the pass.
#[::async_trait::async_trait]
pub trait CompactionHook: Send + Sync {
    /// Optional durable execution kernel paired with this session.  Keeping
    /// this accessor on the injected hook lets gateway adapters construct both
    /// boundaries without making `runtime` depend on a storage crate.
    fn execution_kernel(&self) -> Option<Arc<dyn AgentExecutionKernel>> {
        None
    }

    /// Prepare a stable compaction transaction after a pure window-discovery
    /// pass. Implementations must not mutate Memory/cursors or publish a
    /// checkpoint here; only an opaque `prepared` row is permitted.
    ///
    /// * `archived_messages` — the verbatim messages slated for removal.
    /// * `default_summary` — the runtime's heuristic summary for that window.
    ///
    async fn prepare_compaction(
        &self,
        archived_messages: &[ConversationMessage],
        default_summary: &str,
        trigger: &str,
    ) -> Result<PreparedCompaction, RuntimeError>;

    /// Atomically publish every durable side effect for a validated
    /// replacement. Returning success is the commit point after which the
    /// runtime may replace its in-memory Session.
    async fn commit_compaction(
        &self,
        prepared: &PreparedCompaction,
        result: &CompactionResult,
        trigger: &str,
    ) -> Result<(), RuntimeError>;

    /// Mark a prepared transaction aborted. This method must never create
    /// Memory/cursor/checkpoint side effects.
    async fn abort_compaction(
        &self,
        prepared: &PreparedCompaction,
        reason: &str,
    ) -> Result<(), RuntimeError>;
}

/// Coordinates the model loop, tool execution, hooks, and session updates.
/// Reports runtime events for real-time streaming to the browser.
pub trait RuntimeEventReporter: Send {
    fn on_turn_started(&mut self, _: &str) {}
    fn on_thinking_start(&mut self);
    fn on_thinking_delta(&mut self, text: &str);
    fn on_thinking_end(&mut self);
    fn on_tool_use_start(&mut self, name: &str);
    fn on_tool_input_delta(&mut self, partial_json: &str);
    fn on_tool_use_end(&mut self);
    /// Called when a tool call completes. The `output` may be empty if the tool was denied.
    /// The `input` is the raw JSON string the model sent with this tool call.
    fn on_tool_result(&mut self, name: &str, input: &str, output: &str, is_error: bool);
    fn on_context_management(&mut self, _: &ContextManagementReport) {}
    fn on_runtime_status(&mut self, _: &str, _: &str) {}
    fn on_text_delta(&mut self, text: &str);
}

/// No-op reporter for non-streaming runs.
impl RuntimeEventReporter for () {
    fn on_thinking_start(&mut self) {}
    fn on_thinking_delta(&mut self, _: &str) {}
    fn on_thinking_end(&mut self) {}
    fn on_tool_use_start(&mut self, _: &str) {}
    fn on_tool_input_delta(&mut self, _: &str) {}
    fn on_tool_use_end(&mut self) {}
    fn on_tool_result(&mut self, _: &str, _: &str, _: &str, _: bool) {}
    fn on_text_delta(&mut self, _: &str) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextManagementReport {
    pub original_tool_result_chars: usize,
    pub request_tool_result_chars: usize,
    pub compacted_tool_results: usize,
    pub preserved_recent_tool_results: usize,
}

#[derive(Clone)]
pub struct ConversationRuntime<C, T> {
    session: Session,
    api_client: C,
    tool_executor: T,
    permission_policy: PermissionPolicy,
    system_prompt: Vec<String>,
    max_iterations: usize,
    usage_tracker: UsageTracker,
    hook_runner: HookRunner,
    context_management: crate::config::RuntimeContextManagementConfig,
    auto_compaction_input_tokens_threshold: u32,
    last_auto_compaction_input_tokens: u32,
    hook_abort_signal: HookAbortSignal,
    hook_progress_reporter: Arc<Mutex<Option<Box<dyn HookProgressReporter + Send + Sync>>>>,
    session_tracer: Option<SessionTracer>,
    /// Optional "extract → persist → compact" strategy point injected by higher
    /// layers (Req 4.1 / 4.3 / 4.9). `None` preserves the default heuristic
    /// auto-compaction behavior.
    compaction_hook: Option<Arc<dyn CompactionHook>>,
    /// Authoritative durable execution boundary.  When configured, no tool is
    /// dispatched until its intent, capability and reservation are committed.
    execution_kernel: Option<Arc<dyn AgentExecutionKernel>>,
}

impl<C, T> ConversationRuntime<C, T>
where
    C: ApiClient,
    T: ToolExecutor,
{
    #[must_use]
    pub fn new(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<String>,
    ) -> Self {
        Self::new_with_features(
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            &RuntimeFeatureConfig::default(),
        )
    }

    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new_with_features(
        session: Session,
        api_client: C,
        tool_executor: T,
        permission_policy: PermissionPolicy,
        system_prompt: Vec<String>,
        feature_config: &RuntimeFeatureConfig,
    ) -> Self {
        let usage_tracker = UsageTracker::from_session(&session);
        Self {
            session,
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
            max_iterations: usize::MAX,
            usage_tracker,
            hook_runner: HookRunner::from_feature_config(feature_config),
            context_management: feature_config.context_management().clone(),
            auto_compaction_input_tokens_threshold: auto_compaction_threshold_from_env(),
            last_auto_compaction_input_tokens: 0,
            hook_abort_signal: HookAbortSignal::default(),
            hook_progress_reporter: Arc::new(Mutex::new(None)),
            session_tracer: None,
            compaction_hook: None,
            execution_kernel: None,
        }
    }

    #[must_use]
    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    #[must_use]
    pub fn with_auto_compaction_input_tokens_threshold(mut self, threshold: u32) -> Self {
        self.auto_compaction_input_tokens_threshold = threshold;
        self
    }

    #[must_use]
    pub fn with_hook_abort_signal(mut self, hook_abort_signal: HookAbortSignal) -> Self {
        self.hook_abort_signal = hook_abort_signal;
        self
    }

    #[must_use]
    pub fn with_hook_progress_reporter(
        mut self,
        hook_progress_reporter: Box<dyn HookProgressReporter + Send + Sync>,
    ) -> Self {
        self.hook_progress_reporter = Arc::new(Mutex::new(Some(hook_progress_reporter)));
        self
    }

    pub fn set_hook_progress_reporter(
        &mut self,
        hook_progress_reporter: Option<Box<dyn HookProgressReporter + Send + Sync>>,
    ) {
        self.hook_progress_reporter = Arc::new(Mutex::new(hook_progress_reporter));
    }

    pub fn append_system_prompt_sections(&mut self, sections: &[String]) -> usize {
        let previous_len = self.system_prompt.len();
        self.system_prompt.extend(sections.iter().cloned());
        previous_len
    }

    pub fn truncate_system_prompt_sections(&mut self, len: usize) {
        self.system_prompt.truncate(len);
    }

    #[must_use]
    pub fn system_prompt_sections(&self) -> &[String] {
        &self.system_prompt
    }

    #[must_use]
    pub fn with_session_tracer(mut self, session_tracer: SessionTracer) -> Self {
        self.session_tracer = Some(session_tracer);
        self
    }

    /// Inject the "extract → persist → compact" strategy point run at the
    /// auto-compaction trigger. Higher layers (e.g. the web-server
    /// `orchestrate_compaction` closure) implement [`CompactionHook`] to persist
    /// key info before compaction commits and protect pinned items (Req 4.1 /
    /// 4.3 / 4.9). With no hook set the runtime keeps its default heuristic
    /// compaction.
    #[must_use]
    pub fn with_compaction_hook(mut self, hook: Arc<dyn CompactionHook>) -> Self {
        self.compaction_hook = Some(hook);
        self
    }

    /// Set (or clear) the compaction hook after construction. See
    /// [`ConversationRuntime::with_compaction_hook`].
    pub fn set_compaction_hook(&mut self, hook: Option<Arc<dyn CompactionHook>>) {
        self.compaction_hook = hook;
    }

    #[must_use]
    pub fn with_execution_kernel(mut self, kernel: Arc<dyn AgentExecutionKernel>) -> Self {
        self.execution_kernel = Some(kernel);
        self
    }

    pub fn set_execution_kernel(&mut self, kernel: Option<Arc<dyn AgentExecutionKernel>>) {
        self.execution_kernel = kernel;
    }

    /// Append a message produced by an external orchestrator while preserving
    /// the execution ledger as the recovery authority.
    pub async fn append_visible_message(
        &mut self,
        message: ConversationMessage,
    ) -> Result<(), RuntimeError> {
        let mut projected_session = self.session.clone();
        projected_session
            .push_message(message.clone())
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        let kernel = self.execution_kernel.clone().ok_or_else(|| {
            RuntimeError::new(
                "cannot append an external visible message without a durable execution kernel",
            )
        })?;
        let message_payload = serde_json::to_vec(&message).map_err(|error| {
            RuntimeError::new(format!("cannot serialize visible message: {error}"))
        })?;
        let message_id = format!(
            "{}:{}:{}",
            self.session.session_id,
            self.session.messages.len(),
            hex::encode(Sha256::digest(message_payload))
        );
        kernel.record_visible_message(&message_id, &message).await?;
        self.session = projected_session;
        Ok(())
    }

    fn run_pre_tool_use_hook(&mut self, tool_name: &str, input: &str) -> HookRunResult {
        if let Some(reporter) = self.hook_progress_reporter.lock().unwrap().as_mut() {
            self.hook_runner.run_pre_tool_use_with_context(
                tool_name,
                input,
                Some(&self.hook_abort_signal),
                Some(reporter.as_mut()),
            )
        } else {
            self.hook_runner.run_pre_tool_use_with_context(
                tool_name,
                input,
                Some(&self.hook_abort_signal),
                None,
            )
        }
    }

    fn run_post_tool_use_hook(
        &mut self,
        tool_name: &str,
        input: &str,
        output: &str,
        is_error: bool,
    ) -> HookRunResult {
        if let Some(reporter) = self.hook_progress_reporter.lock().unwrap().as_mut() {
            self.hook_runner.run_post_tool_use_with_context(
                tool_name,
                input,
                output,
                is_error,
                Some(&self.hook_abort_signal),
                Some(reporter.as_mut()),
            )
        } else {
            self.hook_runner.run_post_tool_use_with_context(
                tool_name,
                input,
                output,
                is_error,
                Some(&self.hook_abort_signal),
                None,
            )
        }
    }

    fn run_post_tool_use_failure_hook(
        &mut self,
        tool_name: &str,
        input: &str,
        output: &str,
    ) -> HookRunResult {
        if let Some(reporter) = self.hook_progress_reporter.lock().unwrap().as_mut() {
            self.hook_runner.run_post_tool_use_failure_with_context(
                tool_name,
                input,
                output,
                Some(&self.hook_abort_signal),
                Some(reporter.as_mut()),
            )
        } else {
            self.hook_runner.run_post_tool_use_failure_with_context(
                tool_name,
                input,
                output,
                Some(&self.hook_abort_signal),
                None,
            )
        }
    }

    /// Run a session health probe to verify the runtime is functional after compaction.
    /// Returns Ok(()) if healthy, Err if the session appears broken.
    fn run_session_health_probe(&mut self) -> Result<(), String> {
        // Check if we have basic session integrity
        if self.session.messages.is_empty() && self.session.compaction.is_some() {
            // Freshly compacted with no messages - this is normal
            return Ok(());
        }

        // Verify tool executor is responsive with a non-destructive probe
        // Using glob_search with a pattern that won't match anything
        let probe_input = r#"{"pattern": "*.health-check-probe-"}"#;
        match self.tool_executor.execute("glob_search", probe_input) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Tool executor probe failed: {e}")),
        }
    }

    pub async fn run_turn<R: RuntimeEventReporter>(
        &mut self,
        user_input: impl Into<String>,
        prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        reporter: R,
    ) -> Result<TurnSummary, RuntimeError> {
        crate::behavior_trace("PROMPT-001");
        match self
            .run_turn_resumable(user_input, prompter, reporter)
            .await?
        {
            ResumableTurnOutcome::Completed(summary) => Ok(summary),
            ResumableTurnOutcome::Suspended(suspended) => Err(RuntimeError::new(format!(
                "turn {} suspended for deferred tools; use the resumable turn API",
                suspended.turn_id
            ))),
        }
    }

    pub async fn run_turn_resumable<R: RuntimeEventReporter>(
        &mut self,
        user_input: impl Into<String>,
        prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        mut reporter: R,
    ) -> Result<ResumableTurnOutcome, RuntimeError> {
        let user_input = user_input.into();

        // ROADMAP #38: Session-health canary - probe if context was compacted
        if self.session.compaction.is_some() {
            if let Err(error) = self.run_session_health_probe() {
                return Err(RuntimeError::new(format!(
                    "Session health probe failed after compaction: {error}. \
                     The session may be in an inconsistent state. \
                     Consider starting a fresh session with /session new."
                )));
            }
        }

        let turn_id = self
            .session
            .begin_turn(user_input.clone())
            .map_err(|error| RuntimeError::new(error.to_string()))?;
        if let Some(kernel) = self.execution_kernel.clone() {
            if let Err(error) = kernel
                .start_turn(RuntimeTurnStart {
                    turn_id: turn_id.clone(),
                    user_input: user_input.clone(),
                })
                .await
            {
                let _ = self.session.rollback_latest_turn_started_at(
                    self.session
                        .turns
                        .last()
                        .map_or(0, |turn| turn.start_message_count),
                );
                return Err(RuntimeError::new(format!(
                    "durable turn start failed before model execution: {error}"
                )));
            }
        }
        reporter.on_turn_started(&turn_id);
        self.record_turn_started(&user_input);
        self.session.push_user_text(user_input).map_err(|error| {
            let runtime_error = RuntimeError::new(error.to_string());
            self.complete_session_turn(&turn_id, SessionTurnStatus::Failed);
            runtime_error
        })?;

        let outcome = self
            .continue_resumable_turn(
                &turn_id,
                prompter,
                &mut reporter,
                TurnAccumulator::default(),
            )
            .await;
        outcome
    }

    /// Resume the latest suspended runtime turn by supplying every outstanding
    /// deferred tool result. Results are appended as real tool messages before
    /// the model loop continues, preserving provider tool-call ordering.
    pub async fn resume_turn_with_tool_results<R: RuntimeEventReporter>(
        &mut self,
        results: Vec<DeferredToolResult>,
        prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        mut reporter: R,
    ) -> Result<ResumableTurnOutcome, RuntimeError> {
        let turn = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, SessionTurnStatus::Suspended))
            .cloned()
            .ok_or_else(|| RuntimeError::new("no suspended runtime turn to resume"))?;
        let pending = pending_tool_uses_for_turn(&self.session, &turn);
        if pending.is_empty() {
            return Err(RuntimeError::new(
                "suspended runtime turn has no unmatched tool calls",
            ));
        }

        let mut by_id = results
            .into_iter()
            .map(|result| (result.tool_use_id.clone(), result))
            .collect::<std::collections::HashMap<_, _>>();
        if by_id.len() != pending.len()
            || pending
                .iter()
                .any(|(tool_use_id, _, _)| !by_id.contains_key(tool_use_id))
        {
            return Err(RuntimeError::new(
                "deferred tool results do not exactly match the suspended calls",
            ));
        }

        let mut accumulator = TurnAccumulator::default();
        for (tool_use_id, tool_name, input) in pending {
            let result = by_id
                .remove(&tool_use_id)
                .expect("pending deferred result was validated above");
            let projected = self
                .finish_tool_in_kernel(RuntimeToolOutcome {
                    turn_id: turn.turn_id.clone(),
                    invocation_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    input: input.clone(),
                    output: result.output,
                    iteration: 0,
                    outcome: if result.is_error {
                        RuntimeToolOutcomeKind::Failed
                    } else {
                        RuntimeToolOutcomeKind::Completed
                    },
                })
                .await?;
            reporter.on_tool_result(&tool_name, &input, &projected.model_output, result.is_error);
            reporter.on_tool_use_end();
            let message = ConversationMessage::tool_result(
                tool_use_id,
                tool_name,
                projected.model_output,
                result.is_error,
            );
            self.session
                .push_message(message.clone())
                .map_err(|error| {
                    RuntimeError::new(format!("failed to persist deferred tool result: {error}"))
                })?;
            self.record_tool_finished(0, &message);
            accumulator.tool_results.push(message);
        }
        self.complete_session_turn(&turn.turn_id, SessionTurnStatus::Running);
        self.continue_resumable_turn(&turn.turn_id, prompter, &mut reporter, accumulator)
            .await
    }

    /// Resolve externally approved tool calls and resume the same durable turn.
    /// Approved calls are executed only after the resolution and the normal
    /// intent/capability record have both committed. Explicit deny policy and
    /// current pre-tool hooks are re-evaluated immediately before execution.
    #[allow(clippy::too_many_lines)]
    pub async fn resume_turn_with_approval_decisions<R: RuntimeEventReporter>(
        &mut self,
        decisions: Vec<DeferredApprovalDecision>,
        prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        mut reporter: R,
    ) -> Result<ResumableTurnOutcome, RuntimeError> {
        let turn = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, SessionTurnStatus::Suspended))
            .cloned()
            .ok_or_else(|| RuntimeError::new("no suspended runtime turn to approve"))?;
        let pending = pending_tool_uses_for_turn(&self.session, &turn);
        let mut by_id = decisions
            .into_iter()
            .map(|decision| (decision.tool_use_id.clone(), decision))
            .collect::<std::collections::HashMap<_, _>>();
        if pending.is_empty()
            || by_id.is_empty()
            || by_id
                .keys()
                .any(|tool_use_id| !pending.iter().any(|pending| &pending.0 == tool_use_id))
        {
            return Err(RuntimeError::new(
                "approval decisions do not exactly match the suspended calls",
            ));
        }

        let mut accumulator = TurnAccumulator::default();
        let mut deferred_tools = Vec::new();
        for (tool_use_id, tool_name, input) in pending {
            let Some(decision) = by_id.remove(&tool_use_id) else {
                deferred_tools.push(DeferredToolUse {
                    tool_use_id,
                    tool_name,
                    input,
                    metadata: serde_json::json!({"kind":"external_completion"}).to_string(),
                });
                continue;
            };
            let resolution = RuntimeApprovalResolution {
                turn_id: turn.turn_id.clone(),
                invocation_id: tool_use_id.clone(),
                decision: decision.decision,
                reason: decision.reason.clone(),
            };
            let effective_decision = if let Some(kernel) = self.execution_kernel.clone() {
                kernel.resolve_approval(&resolution).await?
            } else {
                decision.decision
            };

            let pre_hook_result = self.run_pre_tool_use_hook(&tool_name, &input);
            let effective_input = pre_hook_result
                .updated_input()
                .map_or_else(|| input.clone(), ToOwned::to_owned);
            let permission_outcome =
                if !matches!(effective_decision, RuntimeApprovalDecision::Approved) {
                    PermissionOutcome::Deny {
                        reason: decision.reason.unwrap_or_else(|| match effective_decision {
                            RuntimeApprovalDecision::Denied => {
                                "user denied the approval request".to_string()
                            }
                            RuntimeApprovalDecision::Expired => {
                                "the approval request expired before execution".to_string()
                            }
                            RuntimeApprovalDecision::Cancelled => {
                                "the approval request was cancelled".to_string()
                            }
                            RuntimeApprovalDecision::Approved => unreachable!(),
                        }),
                    }
                } else if pre_hook_result.is_cancelled()
                    || pre_hook_result.is_failed()
                    || pre_hook_result.is_denied()
                {
                    PermissionOutcome::Deny {
                        reason: format_hook_message(
                            &pre_hook_result,
                            &format!("current policy blocked approved tool `{tool_name}`"),
                        ),
                    }
                } else {
                    self.permission_policy
                        .authorize_approved(&tool_name, &effective_input)
                };
            let contract = resolve_executor_tool_contract(&self.tool_executor, &tool_name)?;
            let prepared = PreparedToolUse {
                tool_use_id,
                tool_name,
                effective_input,
                pre_hook_result,
                permission_outcome,
                contract,
            };
            match self
                .execute_prepared_tool_use_outcome(&turn.turn_id, 0, &prepared, &mut reporter)
                .await?
            {
                PreparedToolExecution::Completed(message) => {
                    self.session
                        .push_message(message.clone())
                        .map_err(|error| {
                            RuntimeError::new(format!(
                                "failed to persist approved tool result: {error}"
                            ))
                        })?;
                    self.record_tool_finished(0, &message);
                    self.activate_tools_from_search_result(&message);
                    accumulator.tool_results.push(message);
                }
                PreparedToolExecution::Deferred(deferred) => deferred_tools.push(deferred),
            }
        }

        if !deferred_tools.is_empty() {
            let summary = turn_summary_from_accumulator(self, accumulator);
            self.complete_session_turn(&turn.turn_id, SessionTurnStatus::Suspended);
            self.finish_turn_in_kernel(
                &turn.turn_id,
                RuntimeTurnTerminalStatus::Suspended,
                Some("approved tools deferred to external completion"),
            )
            .await?;
            return Ok(ResumableTurnOutcome::Suspended(SuspendedTurn {
                turn_id: turn.turn_id,
                deferred_tools,
                partial_summary: summary,
            }));
        }

        self.complete_session_turn(&turn.turn_id, SessionTurnStatus::Running);
        self.continue_resumable_turn(&turn.turn_id, prompter, &mut reporter, accumulator)
            .await
    }

    /// Finish a suspended turn after a terminal deferred tool (for example
    /// `complete_turn`) has been accepted by the external orchestrator.
    ///
    /// This appends every outstanding `ToolResult` in original `ToolUse` order and
    /// the accepted final assistant text, then marks the same runtime turn
    /// complete without another model request.
    pub async fn finalize_suspended_turn_with_tool_results(
        &mut self,
        results: Vec<DeferredToolResult>,
        final_text: impl Into<String>,
    ) -> Result<TurnSummary, RuntimeError> {
        let turn = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, SessionTurnStatus::Suspended))
            .cloned()
            .ok_or_else(|| RuntimeError::new("no suspended runtime turn to finalize"))?;
        let pending = pending_tool_uses_for_turn(&self.session, &turn);
        let mut by_id = results
            .into_iter()
            .map(|result| (result.tool_use_id.clone(), result))
            .collect::<std::collections::HashMap<_, _>>();
        if pending.is_empty()
            || by_id.len() != pending.len()
            || pending
                .iter()
                .any(|(tool_use_id, _, _)| !by_id.contains_key(tool_use_id))
        {
            return Err(RuntimeError::new(
                "terminal deferred tool results do not exactly match the suspended calls",
            ));
        }

        let mut tool_results = Vec::with_capacity(pending.len());
        for (tool_use_id, tool_name, _) in pending {
            let result = by_id
                .remove(&tool_use_id)
                .expect("terminal deferred result was validated above");
            let projected = self
                .finish_tool_in_kernel(RuntimeToolOutcome {
                    turn_id: turn.turn_id.clone(),
                    invocation_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    input: String::new(),
                    output: result.output,
                    iteration: 0,
                    outcome: if result.is_error {
                        RuntimeToolOutcomeKind::Failed
                    } else {
                        RuntimeToolOutcomeKind::Completed
                    },
                })
                .await?;
            let message = ConversationMessage::tool_result(
                tool_use_id,
                tool_name,
                projected.model_output,
                result.is_error,
            );
            self.session
                .push_message(message.clone())
                .map_err(|error| {
                    RuntimeError::new(format!("failed to persist terminal tool result: {error}"))
                })?;
            self.record_tool_finished(0, &message);
            tool_results.push(message);
        }
        let final_text = final_text.into();
        let mut assistant_messages = Vec::new();
        if !final_text.trim().is_empty() {
            let message =
                ConversationMessage::assistant(vec![ContentBlock::Text { text: final_text }]);
            self.session
                .push_message(message.clone())
                .map_err(|error| {
                    RuntimeError::new(format!(
                        "failed to persist terminal assistant answer: {error}"
                    ))
                })?;
            assistant_messages.push(message);
        }
        self.complete_session_turn(&turn.turn_id, SessionTurnStatus::Completed);
        let summary = TurnSummary {
            assistant_messages,
            tool_results,
            prompt_cache_events: Vec::new(),
            iterations: 0,
            usage: self.usage_tracker.cumulative_usage(),
            auto_compaction: None,
        };
        self.record_turn_completed(&summary);
        self.finish_turn_in_kernel(&turn.turn_id, RuntimeTurnTerminalStatus::Completed, None)
            .await?;
        Ok(summary)
    }

    /// Reconstruct the latest suspended turn from the durable session
    /// checkpoint. Deferred metadata is advisory, so recovery rebuilds it from
    /// the unmatched `ToolUse` blocks and leaves metadata empty.
    #[must_use]
    pub fn suspended_turn_snapshot(&self) -> Option<SuspendedTurn> {
        let turn = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, SessionTurnStatus::Suspended))?;
        let deferred_tools = pending_tool_uses_for_turn(&self.session, turn)
            .into_iter()
            .map(|(tool_use_id, tool_name, input)| DeferredToolUse {
                tool_use_id,
                tool_name,
                input,
                metadata: String::new(),
            })
            .collect::<Vec<_>>();
        if deferred_tools.is_empty() {
            return None;
        }
        let turn_messages = self
            .session
            .messages
            .get(turn.start_message_count..)
            .unwrap_or_default();
        let assistant_messages = turn_messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .cloned()
            .collect::<Vec<_>>();
        let tool_results = turn_messages
            .iter()
            .filter(|message| {
                message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            })
            .cloned()
            .collect::<Vec<_>>();
        Some(SuspendedTurn {
            turn_id: turn.turn_id.clone(),
            deferred_tools,
            partial_summary: TurnSummary {
                assistant_messages,
                tool_results,
                prompt_cache_events: Vec::new(),
                iterations: 0,
                usage: self.usage_tracker.cumulative_usage(),
                auto_compaction: None,
            },
        })
    }

    /// Roll back the latest interrupted in-flight model turn before a durable
    /// coordinator retries the same user request after process restart.
    pub fn rollback_interrupted_turn(&mut self) -> Result<bool, RuntimeError> {
        let Some(turn) = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| matches!(turn.status, SessionTurnStatus::Running))
            .cloned()
        else {
            return Ok(false);
        };
        self.session
            .rollback_latest_turn_started_at(turn.start_message_count)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }

    /// Roll back one exact latest runtime turn after its durable parent failed
    /// to commit. This is deliberately id-based so recovery never removes an
    /// older completed turn merely because the user repeated the same prompt.
    pub fn rollback_turn_by_id(&mut self, turn_id: &str) -> Result<bool, RuntimeError> {
        let latest = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| !matches!(turn.status, SessionTurnStatus::RolledBack))
            .cloned();
        let Some(turn) = latest else {
            return Ok(false);
        };
        if turn.turn_id != turn_id {
            return Ok(false);
        }
        self.session
            .rollback_latest_turn_started_at(turn.start_message_count)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }

    async fn continue_resumable_turn<R: RuntimeEventReporter>(
        &mut self,
        turn_id: &str,
        prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        reporter: &mut R,
        accumulator: TurnAccumulator,
    ) -> Result<ResumableTurnOutcome, RuntimeError> {
        match self
            .continue_resumable_turn_inner(turn_id, prompter, reporter, accumulator)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                self.record_turn_failed(0, &error);
                self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                let detail = error.to_string();
                if let Err(terminal_error) = self
                    .finish_turn_in_kernel(
                        turn_id,
                        RuntimeTurnTerminalStatus::Failed,
                        Some(&detail),
                    )
                    .await
                {
                    return Err(RuntimeError::new(format!(
                        "{error}; durable failed-turn commit also failed: {terminal_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn continue_resumable_turn_inner<R: RuntimeEventReporter>(
        &mut self,
        turn_id: &str,
        mut prompter: Option<&mut (dyn PermissionPrompter + Send)>,
        reporter: &mut R,
        mut accumulator: TurnAccumulator,
    ) -> Result<ResumableTurnOutcome, RuntimeError> {
        loop {
            accumulator.iterations += 1;
            if accumulator.iterations > self.max_iterations {
                let error = RuntimeError::new(
                    "conversation loop exceeded the maximum number of iterations",
                );
                self.record_turn_failed(accumulator.iterations, &error);
                self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                return Err(error);
            }

            let (request_messages, context_report) = self.request_messages();
            if let Some(report) = context_report.as_ref() {
                reporter.on_context_management(report);
            }

            let completed_tool_names = accumulator
                .tool_results
                .iter()
                .flat_map(|message| message.blocks.iter())
                .filter_map(|block| match block {
                    ContentBlock::ToolResult { tool_name, .. } => Some(tool_name.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let iteration_instruction = self
                .api_client
                .prepare_model_iteration(accumulator.iterations, &completed_tool_names);
            let budget_stage = self.api_client.model_budget_stage(
                accumulator.iterations,
                &completed_tool_names,
                iteration_instruction.as_deref(),
            );
            if let Some(instruction) = iteration_instruction.as_deref() {
                reporter.on_runtime_status("tool_budget_synthesis", instruction);
            }
            let mut system_prompt = self.system_prompt.clone();
            system_prompt.extend(iteration_instruction);
            if !accumulator.failure_blocked_tools.is_empty() {
                system_prompt.push(format!(
                    "The server disabled these tools for the rest of this turn after repeated identical failures: {}. Do not retry them or turn their error text into a new user request. Return to the original latest user request, use other available evidence or tools when useful, and provide the best honest answer you can.",
                    accumulator
                        .failure_blocked_tools
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let prompt_variant = self.api_client.prompt_variant();
            if let Some(variant) = prompt_variant.as_ref() {
                if !variant.stable_system.trim().is_empty()
                    && !system_prompt
                        .iter()
                        .any(|value| value == &variant.stable_system)
                {
                    system_prompt.insert(0, variant.stable_system.clone());
                }
                if !variant.domain_contract.trim().is_empty()
                    && !system_prompt
                        .iter()
                        .any(|value| value == &variant.domain_contract)
                {
                    system_prompt.push(variant.domain_contract.clone());
                }
            }
            let hard_input_budget = self
                .api_client
                .context_window_tokens()
                .map_or_else(context_compiler_max_tokens, |window| {
                    window.saturating_mul(65) / 100
                })
                .min(context_compiler_max_tokens())
                .max(1);
            let active_tools = self.api_client.active_tool_names();
            let model_version = self.api_client.model_version();
            let objective = latest_user_objective(&request_messages);
            let domain = self.api_client.context_domain();
            let supplement = match self.execution_kernel.clone() {
                Some(kernel) => kernel
                    .load_context_supplement(RuntimeContextSupplementRequest {
                        turn_id: turn_id.to_string(),
                        iteration: accumulator.iterations,
                        objective,
                        domain,
                        max_input_tokens: hard_input_budget,
                        model_version: model_version.clone(),
                        active_tools: active_tools.clone(),
                    })
                    .await
                    .map_err(|error| {
                        self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                        RuntimeError::new(format!(
                            "failed to compile governed semantic context: {error}"
                        ))
                    })?,
                None => RuntimeContextSupplement::default(),
            };
            let compiled = compile_model_visible_context_with_budget(
                system_prompt,
                request_messages,
                &supplement,
                hard_input_budget,
            )?;
            let prompt_manifest = prompt_variant.as_ref().map(|variant| {
                let tool_schema_hash = hex::encode(Sha256::digest(
                    serde_json::to_vec(&active_tools).unwrap_or_default(),
                ));
                agent_protocol::PromptRegistry::manifest(
                    variant,
                    &tool_schema_hash,
                    hard_input_budget,
                    16_384,
                )
            });
            let mut request = ApiRequest {
                system_prompt: compiled.system_prompt,
                messages: compiled.messages,
                trace: ProviderRequestTrace::turn(turn_id, accumulator.iterations),
            };
            if let Some(kernel) = self.execution_kernel.clone() {
                let lineage = kernel
                    .record_context_manifest(RuntimeContextManifestInput {
                        turn_id: turn_id.to_string(),
                        iteration: accumulator.iterations,
                        budget_stage,
                        max_input_tokens: hard_input_budget,
                        estimated_tokens: request
                            .messages
                            .iter()
                            .map(crate::token_estimator::estimate_message_tokens)
                            .sum::<usize>()
                            + request
                                .system_prompt
                                .iter()
                                .map(|section| estimate_text_tokens(section))
                                .sum::<usize>(),
                        system_sections: request.system_prompt.clone(),
                        messages: request.messages.clone(),
                        model_version,
                        active_tools,
                        semantic_snapshot_version: compiled
                            .context_packet
                            .manifest
                            .snapshot_version,
                        context_packet: compiled.context_packet,
                        prompt_manifest,
                    })
                    .await
                    .map_err(|error| {
                        self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                        RuntimeError::new(format!(
                            "failed to commit model-visible context manifest: {error}"
                        ))
                    })?;
                request.trace.context_manifest_id = Some(lineage.context_manifest_id);
                request.trace.prompt_manifest_id = lineage.prompt_manifest_id;
                request.trace.context_manifest_hash = Some(lineage.context_manifest_hash);
            }
            let events = match self
                .api_client
                .stream_with_reporter(request, reporter)
                .await
            {
                Ok(events) => events,
                Err(error) => {
                    self.record_turn_failed(accumulator.iterations, &error);
                    self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                    return Err(error);
                }
            };
            let (assistant_message, usage, turn_prompt_cache_events) =
                match build_assistant_message(events) {
                    Ok(result) => result,
                    Err(error) => {
                        self.record_turn_failed(accumulator.iterations, &error);
                        self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                        return Err(error);
                    }
                };
            if let Some(usage) = usage {
                self.usage_tracker.record(usage);
            }
            accumulator
                .prompt_cache_events
                .extend(turn_prompt_cache_events);
            let pending_tool_uses = assistant_message
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, name, input } => {
                        Some((id.clone(), name.clone(), input.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            self.record_assistant_iteration(
                accumulator.iterations,
                &assistant_message,
                pending_tool_uses.len(),
            );

            if let Some(kernel) = self.execution_kernel.clone() {
                kernel
                    .record_assistant_message(turn_id, accumulator.iterations, &assistant_message)
                    .await
                    .map_err(|error| {
                        self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                        RuntimeError::new(format!("failed to commit assistant event: {error}"))
                    })?;
            }

            self.session
                .push_message(assistant_message.clone())
                .map_err(|error| {
                    let runtime_error = RuntimeError::new(error.to_string());
                    self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                    runtime_error
                })?;
            accumulator.assistant_messages.push(assistant_message);

            // In_Turn_Compaction trigger point (Req 1.1/1.2/1.6): this runs inside
            // the single-turn reasoning loop, after the assistant message is
            // committed and before pending tools / the next API call. When the
            // gate is crossed, `maybe_auto_compact` compacts in place and the loop
            // continues using the compacted `self.session` for the rest of this
            // turn (the turn is not terminated). Because the loop naturally passes
            // through this point on every iteration and the gate is re-evaluated
            // each time, a single long-horizon turn can trigger compaction more
            // than once (Req 1.6). Also covers the terminal (no-tool) iteration to
            // prevent unbounded session growth (#3106).
            match self.maybe_auto_compact().await {
                Ok(Some(compaction)) => {
                    accumulator.auto_compaction = Some(compaction);
                }
                Ok(None) => {}
                Err(error) => {
                    self.record_turn_failed(accumulator.iterations, &error);
                    self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                    return Err(error);
                }
            }

            if pending_tool_uses.is_empty() {
                break;
            }

            let mut prepared_tool_uses = Vec::with_capacity(pending_tool_uses.len());
            for (tool_use_id, tool_name, input) in pending_tool_uses {
                let tool_call_allowed = self.api_client.is_tool_call_allowed(&tool_name)
                    || stale_tool_alias_is_allowed(&self.api_client, &tool_name);
                let tool_call_allowed =
                    tool_call_allowed && !accumulator.failure_blocked_tools.contains(&tool_name);
                let pre_hook_result = self.run_pre_tool_use_hook(&tool_name, &input);
                let effective_input = pre_hook_result
                    .updated_input()
                    .map_or_else(|| input.clone(), ToOwned::to_owned);
                let permission_context = PermissionContext::new(
                    pre_hook_result.permission_override(),
                    pre_hook_result.permission_reason().map(ToOwned::to_owned),
                );

                let permission_outcome = if !tool_call_allowed {
                    PermissionOutcome::Deny {
                        reason: format!(
                            "tool `{tool_name}` is unavailable in the current model iteration; use the available evidence and finish the answer"
                        ),
                    }
                } else if pre_hook_result.is_cancelled() {
                    PermissionOutcome::Deny {
                        reason: format_hook_message(
                            &pre_hook_result,
                            &format!("PreToolUse hook cancelled tool `{tool_name}`"),
                        ),
                    }
                } else if pre_hook_result.is_failed() {
                    PermissionOutcome::Deny {
                        reason: format_hook_message(
                            &pre_hook_result,
                            &format!("PreToolUse hook failed for tool `{tool_name}`"),
                        ),
                    }
                } else if pre_hook_result.is_denied() {
                    PermissionOutcome::Deny {
                        reason: format_hook_message(
                            &pre_hook_result,
                            &format!("PreToolUse hook denied tool `{tool_name}`"),
                        ),
                    }
                } else if let Some(prompt) = prompter.as_mut() {
                    self.permission_policy.authorize_with_context(
                        &tool_name,
                        &effective_input,
                        &permission_context,
                        Some(*prompt),
                    )
                } else {
                    self.permission_policy.authorize_with_context(
                        &tool_name,
                        &effective_input,
                        &permission_context,
                        None,
                    )
                };

                let contract = resolve_executor_tool_contract(&self.tool_executor, &tool_name)?;
                prepared_tool_uses.push(PreparedToolUse {
                    tool_use_id,
                    tool_name,
                    effective_input,
                    pre_hook_result,
                    permission_outcome,
                    contract,
                });
            }

            let mut deferred_tools = Vec::new();
            let mut tool_index = 0;
            while tool_index < prepared_tool_uses.len() {
                let prepared = &prepared_tool_uses[tool_index];
                if !prepared.is_allowed_parallel(&self.tool_executor) {
                    match self
                        .execute_prepared_tool_use_outcome(
                            turn_id,
                            accumulator.iterations,
                            prepared,
                            reporter,
                        )
                        .await?
                    {
                        PreparedToolExecution::Completed(result_message) => {
                            self.observe_repeated_tool_failure(
                                &mut accumulator,
                                prepared,
                                &result_message,
                            );
                            self.session
                                .push_message(result_message.clone())
                                .map_err(|error| {
                                    let runtime_error = RuntimeError::new(error.to_string());
                                    self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                                    runtime_error
                                })?;
                            self.record_tool_finished(accumulator.iterations, &result_message);
                            self.activate_tools_from_search_result(&result_message);
                            accumulator.tool_results.push(result_message);
                        }
                        PreparedToolExecution::Deferred(deferred) => {
                            deferred_tools.push(deferred);
                        }
                    }
                    tool_index += 1;
                    continue;
                }

                let batch_start = tool_index;
                let mut batch_end = tool_index + 1;
                while batch_end < prepared_tool_uses.len()
                    && prepared_tool_uses[batch_end].is_allowed_parallel(&self.tool_executor)
                {
                    batch_end += 1;
                }

                for prepared in &prepared_tool_uses[batch_start..batch_end] {
                    self.authorize_tool_in_kernel(turn_id, accumulator.iterations, prepared)
                        .await?;
                    self.start_tool_in_kernel(turn_id, accumulator.iterations, prepared)
                        .await?;
                    self.record_tool_started(accumulator.iterations, &prepared.tool_name);
                    reporter.on_tool_use_start(&prepared.tool_name);
                    reporter.on_tool_input_delta(&prepared.effective_input);
                }

                let requests = prepared_tool_uses[batch_start..batch_end]
                    .iter()
                    .map(|prepared| ToolExecutionRequest {
                        tool_name: prepared.tool_name.clone(),
                        input: prepared.effective_input.clone(),
                    })
                    .collect::<Vec<_>>();
                let mut batch_results = self.tool_executor.execute_batch(requests).into_iter();

                for prepared in &prepared_tool_uses[batch_start..batch_end] {
                    let execution = batch_results.next().unwrap_or_else(|| {
                        Err(ToolError::new(
                            "parallel tool executor returned fewer results than requested",
                        ))
                    });
                    let result_message = self
                        .finalize_allowed_tool_use(
                            turn_id,
                            accumulator.iterations,
                            prepared,
                            execution,
                            reporter,
                        )
                        .await?;
                    self.observe_repeated_tool_failure(&mut accumulator, prepared, &result_message);
                    self.session
                        .push_message(result_message.clone())
                        .map_err(|error| {
                            let runtime_error = RuntimeError::new(error.to_string());
                            self.complete_session_turn(turn_id, SessionTurnStatus::Failed);
                            runtime_error
                        })?;
                    self.record_tool_finished(accumulator.iterations, &result_message);
                    self.activate_tools_from_search_result(&result_message);
                    accumulator.tool_results.push(result_message);
                }
                if batch_results.next().is_some() {
                    self.record_runtime_warning(
                        "parallel_tool_executor_extra_result",
                        "parallel tool executor returned more results than requested",
                    );
                }
                tool_index = batch_end;
            }

            if !deferred_tools.is_empty() {
                let summary = turn_summary_from_accumulator(self, accumulator);
                self.complete_session_turn(turn_id, SessionTurnStatus::Suspended);
                self.finish_turn_in_kernel(
                    turn_id,
                    RuntimeTurnTerminalStatus::Suspended,
                    Some("deferred tools awaiting external completion"),
                )
                .await?;
                return Ok(ResumableTurnOutcome::Suspended(SuspendedTurn {
                    turn_id: turn_id.to_string(),
                    deferred_tools,
                    partial_summary: summary,
                }));
            }
        }

        let summary = turn_summary_from_accumulator(self, accumulator);
        self.record_turn_completed(&summary);
        self.complete_session_turn(turn_id, SessionTurnStatus::Completed);
        self.finish_turn_in_kernel(turn_id, RuntimeTurnTerminalStatus::Completed, None)
            .await?;

        Ok(ResumableTurnOutcome::Completed(summary))
    }

    #[must_use]
    pub fn compact(&self, config: CompactionConfig) -> CompactionResult {
        compact_runtime_session(&self.session, config)
    }

    /// Run every compaction trigger through the same two-phase coordinator.
    /// Preparation may persist only an opaque transaction. The candidate is
    /// then checked against the exact discovered window and the growth guard;
    /// durable side effects commit before the in-memory Session is replaced.
    pub async fn compact_transactionally(
        &mut self,
        config: CompactionConfig,
        trigger: &str,
        summary_override: Option<String>,
    ) -> Result<Option<CompactionResult>, RuntimeError> {
        crate::behavior_trace("CMP-001");
        let baseline = compact_session(&self.session, config);
        if baseline.removed_message_count == 0 {
            return Ok(None);
        }
        let requested_summary = summary_override.unwrap_or_else(|| baseline.summary.clone());
        let hook = self.compaction_hook.clone();
        let prepared = match hook.as_ref() {
            Some(hook) => Some(
                hook.prepare_compaction(&baseline.archived_messages, &requested_summary, trigger)
                    .await?,
            ),
            None => None,
        };
        let replacement_summary = prepared
            .as_ref()
            .and_then(|item| item.replacement_summary.clone())
            .unwrap_or(requested_summary);
        let result = compact_session_with_summary(&self.session, config, replacement_summary);

        let abort = |reason: String| async {
            if let (Some(hook), Some(prepared)) = (hook.as_ref(), prepared.as_ref()) {
                hook.abort_compaction(prepared, &reason).await?;
            }
            Err(RuntimeError::new(reason))
        };
        if result.archived_messages != baseline.archived_messages
            || result.removed_message_count != baseline.removed_message_count
        {
            return abort("compaction source window changed after preparation".to_string()).await;
        }
        let source_tokens = result
            .archived_messages
            .iter()
            .map(crate::token_estimator::estimate_message_tokens)
            .sum::<usize>();
        let replacement_tokens = result
            .compacted_session
            .messages
            .first()
            .map(crate::token_estimator::estimate_message_tokens)
            .unwrap_or_default();
        if source_tokens == 0
            || replacement_tokens.saturating_mul(100) > source_tokens.saturating_mul(60)
        {
            return abort(format!(
                "framed compaction replacement exceeded the 60% proof budget ({replacement_tokens}/{source_tokens})"
            ))
            .await;
        }

        if let (Some(hook), Some(prepared)) = (hook.as_ref(), prepared.as_ref()) {
            if let Err(error) = hook.commit_compaction(prepared, &result, trigger).await {
                let _ = hook
                    .abort_compaction(prepared, &format!("commit failed: {error}"))
                    .await;
                return Err(error);
            }
        } else if let Some(kernel) = self.execution_kernel.clone() {
            kernel
                .checkpoint_session(&format!("{trigger}_compaction"), &result.compacted_session)
                .await?;
        }

        self.session = result.compacted_session.clone();
        // JSONL is an export sink, not the recovery authority. A failed export
        // must not roll back a compaction that already committed atomically.
        if let Some(path) = self.session.persistence_path().map(ToOwned::to_owned) {
            if let Err(error) = self.session.save_to_path(path) {
                tracing::warn!(trigger, error = %error, "session JSONL export failed after durable compaction commit");
            }
        }
        Ok(Some(result))
    }

    #[must_use]
    pub fn estimated_tokens(&self) -> usize {
        estimate_session_tokens(&self.session)
    }

    #[must_use]
    pub fn usage(&self) -> &UsageTracker {
        &self.usage_tracker
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn api_client_mut(&mut self) -> &mut C {
        &mut self.api_client
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    #[must_use]
    pub fn message_count(&self) -> usize {
        self.session.messages.len()
    }

    pub fn rollback_messages_to_len(&mut self, len: usize) -> Result<(), RuntimeError> {
        self.session
            .rollback_messages_to_len(len)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }

    pub fn rollback_latest_turn_started_at(&mut self, len: usize) -> Result<bool, RuntimeError> {
        self.session
            .rollback_latest_turn_started_at(len)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }

    /// Close the latest open durable turn after an outer coordinator observes
    /// cancellation or an upstream failure that interrupted the runtime task.
    /// This is intentionally explicit because a `tokio::select!` cancellation
    /// drops the in-flight future before the normal loop can emit a terminal
    /// event.
    pub async fn finish_latest_kernel_turn(
        &mut self,
        status: RuntimeTurnTerminalStatus,
        detail: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let Some(turn_id) = self
            .session
            .turns
            .iter()
            .rev()
            .find(|turn| {
                matches!(
                    turn.status,
                    SessionTurnStatus::Running
                        | SessionTurnStatus::Suspended
                        | SessionTurnStatus::RolledBack
                )
            })
            .map(|turn| turn.turn_id.clone())
        else {
            return Ok(());
        };
        let session_status = match status {
            RuntimeTurnTerminalStatus::Completed => SessionTurnStatus::Completed,
            RuntimeTurnTerminalStatus::Failed => SessionTurnStatus::Failed,
            RuntimeTurnTerminalStatus::Cancelled => SessionTurnStatus::Cancelled,
            RuntimeTurnTerminalStatus::Suspended => SessionTurnStatus::Suspended,
        };
        self.complete_session_turn(&turn_id, session_status);
        self.finish_turn_in_kernel(&turn_id, status, detail).await
    }

    pub fn rollback_last_turns(&mut self, turn_count: usize) -> Result<(), RuntimeError> {
        self.session
            .rollback_last_turns(turn_count)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }

    fn request_messages(&self) -> (Vec<ConversationMessage>, Option<ContextManagementReport>) {
        if !self.context_management.enabled() {
            return (self.session.messages.clone(), None);
        }
        compact_tool_results_for_request(&self.session.messages, &self.context_management)
    }

    /// Restore session conversation history from a persisted `Session` (e.g. loaded from disk
    /// during hot-reload after a server restart). This replaces the current session's
    /// message history while preserving the newly built runtime (tools, MCP connections, etc.).
    pub fn restore_session(&mut self, persisted: Session) {
        self.session.messages = persisted.messages;
        self.session.compaction = persisted.compaction;
        self.session.prompt_history = persisted.prompt_history;
        self.session.turns = persisted.turns;
        self.session.runtime_context = persisted.runtime_context;
        self.session.context_baseline = persisted.context_baseline;
        self.session.last_health_check_ms = persisted.last_health_check_ms;
    }

    #[must_use]
    pub fn fork_session(&self, branch_name: Option<String>) -> Session {
        self.session.fork(branch_name)
    }

    #[must_use]
    pub fn into_session(self) -> Session {
        self.session
    }

    /// Performs an `In_Turn_Compaction` check for the current turn (Req 1.1/1.2/1.3/1.6).
    ///
    /// Called from the turn's reasoning loop on every iteration, this evaluates the
    /// `Auto_Compact_Threshold` gate and, when crossed, compacts the session in place
    /// so the current turn can continue without terminating:
    ///
    /// 1. `compact_session` performs a pure, non-committing window discovery.
    /// 2. The injected `CompactionHook::before_compaction` extracts + persists key
    ///    info and may return a pinned-augmented summary.
    /// 3. `compact_session_with_summary` commits the compaction, reusing the shared
    ///    engine for tail preservation (`retainedTailTokens`) and verbatim archive
    ///    boundary handling — no parallel compaction/archive implementation.
    ///
    /// The compacted session replaces `self.session` and the loop resumes with it,
    /// so subsequent reasoning/tool calls in this turn use the compacted context.
    /// Because the loop re-evaluates the gate on each iteration, a single
    /// long-horizon turn may trigger this more than once (Req 1.6). The signature
    /// and the `CompactionHook` call order are intentionally unchanged; only the
    /// trigger semantics are documented/extended to the in-turn case.
    async fn maybe_auto_compact(&mut self) -> Result<Option<AutoCompactionEvent>, RuntimeError> {
        let cumulative_input_tokens = self.usage_tracker.cumulative_usage().input_tokens;
        let input_tokens_since_compaction =
            cumulative_input_tokens.saturating_sub(self.last_auto_compaction_input_tokens);
        // Some OpenAI-compatible streaming providers omit usage in intermediate
        // responses. Relying only on provider-reported cumulative input then lets
        // a tool-heavy turn grow past the context window before this gate opens.
        // The local estimate is deliberately checked as a second, independent
        // signal so in-turn compaction remains effective for those providers.
        let estimated_session_tokens = estimate_session_tokens(&self.session);
        if !should_auto_compact(
            input_tokens_since_compaction,
            estimated_session_tokens,
            self.auto_compaction_input_tokens_threshold,
        ) {
            return Ok(None);
        }

        let config = CompactionConfig {
            max_estimated_tokens: 0,
            ..CompactionConfig::default()
        };

        let result = match self.compact_transactionally(config, "auto", None).await {
            Ok(Some(result)) => result,
            Ok(None) => {
                self.last_auto_compaction_input_tokens = cumulative_input_tokens;
                return Ok(None);
            }
            Err(error) => {
                self.last_auto_compaction_input_tokens = cumulative_input_tokens;
                self.record_in_turn_compaction_failed(&error);
                return Ok(None);
            }
        };

        // Field-complete `session_compacted` payload (Req 1.5). All fields are
        // reused directly from the shared compaction engine's `CompactionResult`
        // — nothing is re-derived, so no parallel compaction/archive logic is
        // introduced:
        //   * `summaryTokens` — estimated size of the engine's summary.
        //   * `retainedTail` — the recent tail the engine preserved verbatim.
        //     `compact_session` / `compact_session_with_summary` place a single
        //     synthesized continuation system message at index 0 and append the
        //     preserved tail after it, so the tail is `messages[1..]`. Cloning it
        //     here lets the emitted event assert the `retainedTailTokens` window
        //     is preserved byte-for-byte.
        let removed_message_count = result.removed_message_count;
        let summary_tokens = estimate_text_tokens(&result.summary);
        let retained_tail: Vec<ConversationMessage> = result
            .compacted_session
            .messages
            .iter()
            .skip(1)
            .cloned()
            .collect();
        let archived_messages = result.archived_messages;

        self.last_auto_compaction_input_tokens = cumulative_input_tokens;

        // Emit exactly one field-complete `session_compacted` trace event per
        // committed compaction (Req 1.5). Because the turn loop calls this on
        // each iteration, a long-horizon turn that crosses the threshold N times
        // records N distinct events (Req 1.6), one per compaction.
        self.record_session_compacted(removed_message_count, summary_tokens, &retained_tail);

        Ok(Some(AutoCompactionEvent {
            removed_message_count,
            archived_messages,
            summary_tokens,
            retained_tail,
        }))
    }

    /// Records exactly one field-complete `session_compacted` trace event after a
    /// committed in-turn compaction (Req 1.5).
    ///
    /// The event carries the full observable/auditable field set — `removed_messages`
    /// (`removedMessages`), `summary_tokens` (`summaryTokens`), and the retained
    /// tail (`retainedTail`, reported both as a message count and an estimated
    /// token size of the preserved window) — so downstream observability and the
    /// `Zero_Loss` measurement can be reconciled against the actual compaction.
    fn record_session_compacted(
        &self,
        removed_message_count: usize,
        summary_tokens: usize,
        retained_tail: &[ConversationMessage],
    ) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let retained_tail_tokens: usize = retained_tail
            .iter()
            .map(crate::token_estimator::estimate_message_tokens)
            .sum();

        let mut attributes = Map::new();
        attributes.insert(
            "removed_messages".to_string(),
            Value::from(removed_message_count as u64),
        );
        attributes.insert(
            "summary_tokens".to_string(),
            Value::from(summary_tokens as u64),
        );
        attributes.insert(
            "retained_tail".to_string(),
            Value::from(retained_tail.len() as u64),
        );
        attributes.insert(
            "retained_tail_tokens".to_string(),
            Value::from(retained_tail_tokens as u64),
        );
        session_tracer.record("session_compacted", attributes);
    }

    /// Records a degraded in-turn compaction to the session trace (Req 1.7).
    ///
    /// Called when the injected [`CompactionHook`] surfaces an error during the
    /// "extract → persist" stage. The compaction is downgraded to "continue with
    /// the uncompacted context" (the caller returns `Ok(None)`); this trace entry
    /// preserves observability/auditability of the degraded path without
    /// aborting the turn.
    fn record_in_turn_compaction_failed(&self, error: &RuntimeError) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("error".to_string(), Value::String(error.to_string()));
        attributes.insert(
            "degraded".to_string(),
            Value::String("continued_with_uncompacted_context".to_string()),
        );
        session_tracer.record("in_turn_compaction_failed", attributes);
    }

    fn record_turn_started(&self, user_input: &str) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert(
            "user_input".to_string(),
            Value::String(user_input.to_string()),
        );
        session_tracer.record("turn_started", attributes);
    }

    fn record_assistant_iteration(
        &self,
        iteration: usize,
        assistant_message: &ConversationMessage,
        pending_tool_use_count: usize,
    ) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("iteration".to_string(), Value::from(iteration as u64));
        attributes.insert(
            "assistant_blocks".to_string(),
            Value::from(assistant_message.blocks.len() as u64),
        );
        attributes.insert(
            "pending_tool_use_count".to_string(),
            Value::from(pending_tool_use_count as u64),
        );
        session_tracer.record("assistant_iteration_completed", attributes);
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_prepared_tool_use_outcome<R: RuntimeEventReporter>(
        &mut self,
        turn_id: &str,
        iteration: usize,
        prepared: &PreparedToolUse,
        reporter: &mut R,
    ) -> Result<PreparedToolExecution, RuntimeError> {
        if prepared.tool_name.eq_ignore_ascii_case("AskUserQuestion")
            || prepared.tool_name.eq_ignore_ascii_case("ask_user_question")
        {
            let Some(kernel) = self.execution_kernel.clone() else {
                return Err(RuntimeError::new(
                    "user questions require a durable execution kernel",
                ));
            };
            let input = serde_json::from_str::<serde_json::Value>(&prepared.effective_input)
                .map_err(|error| RuntimeError::new(format!("invalid user question: {error}")))?;
            let question = input
                .get("question")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| RuntimeError::new("user question is empty"))?;
            let options = input
                .get("options")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            let request_schema_hash = stable_context_hash(&prepared.effective_input);
            let choice_schema_hash = (!options.is_empty()).then(|| {
                stable_context_hash(&serde_json::Value::Array(options.clone()).to_string())
            });
            let interaction_id = format!(
                "interaction:{}",
                stable_context_hash(&format!(
                    "{}:{}",
                    self.session.session_id, prepared.tool_use_id
                ))
            );
            let owner_user_id = self
                .session
                .user_id
                .clone()
                .ok_or_else(|| RuntimeError::new("durable user question has no owner"))?;
            let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);
            let expected_turn_revision = kernel.current_turn_revision(turn_id).await?;
            kernel
                .request_interaction(&RuntimeInteractionRequest {
                    interaction_id: interaction_id.clone(),
                    kind: agent_protocol::InteractionKind::UserQuestion,
                    turn_id: turn_id.to_string(),
                    invocation_id: prepared.tool_use_id.clone(),
                    owner_user_id,
                    allowed_responder_ids: Vec::new(),
                    capability_requirement: None,
                    request_schema_hash,
                    choice_schema_hash,
                    display_projection: serde_json::json!({
                        "question":question,
                        "options":options,
                    }),
                    idempotency_key: format!("user-question:{}", prepared.tool_use_id),
                    expected_turn_revision,
                    expires_at: Some(expires_at),
                })
                .await?;
            reporter.on_runtime_status("waiting_user_question", "waiting for the user response");
            return Ok(PreparedToolExecution::Deferred(DeferredToolUse {
                tool_use_id: prepared.tool_use_id.clone(),
                tool_name: prepared.tool_name.clone(),
                input: prepared.effective_input.clone(),
                metadata: serde_json::json!({
                    "kind":"user_question",
                    "interactionId":interaction_id,
                    "expiresAt":expires_at,
                })
                .to_string(),
            }));
        }
        if let PermissionOutcome::AwaitingApproval { request } = &prepared.permission_outcome {
            let Some(kernel) = self.execution_kernel.clone() else {
                return Err(RuntimeError::new(
                    "interactive approval requires a durable execution kernel",
                ));
            };
            kernel
                .request_approval(&RuntimeApprovalRequest {
                    turn_id: turn_id.to_string(),
                    invocation_id: prepared.tool_use_id.clone(),
                    tool_name: prepared.tool_name.clone(),
                    input: prepared.effective_input.clone(),
                    iteration,
                    request: request.clone(),
                    contract: prepared.contract.clone(),
                })
                .await?;
            reporter.on_runtime_status(
                "waiting_approval",
                &format!("tool `{}` is waiting for user approval", prepared.tool_name),
            );
            return Ok(PreparedToolExecution::Deferred(DeferredToolUse {
                tool_use_id: prepared.tool_use_id.clone(),
                tool_name: prepared.tool_name.clone(),
                input: prepared.effective_input.clone(),
                metadata: serde_json::json!({
                    "kind": "approval",
                    "currentMode": request.current_mode.as_str(),
                    "requiredMode": request.required_mode.as_str(),
                    "reason": request.reason,
                })
                .to_string(),
            }));
        }
        self.authorize_tool_in_kernel(turn_id, iteration, prepared)
            .await?;
        match &prepared.permission_outcome {
            PermissionOutcome::Allow => {
                self.start_tool_in_kernel(turn_id, iteration, prepared)
                    .await?;
                self.record_tool_started(iteration, &prepared.tool_name);
                reporter.on_tool_use_start(&prepared.tool_name);
                reporter.on_tool_input_delta(&prepared.effective_input);
                match self
                    .tool_executor
                    .execute_outcome(&prepared.tool_name, &prepared.effective_input)
                {
                    ToolExecutionOutcome::Completed(execution) => {
                        Ok(PreparedToolExecution::Completed(
                            self.finalize_allowed_tool_use(
                                turn_id, iteration, prepared, execution, reporter,
                            )
                            .await?,
                        ))
                    }
                    ToolExecutionOutcome::Deferred { metadata } => {
                        if !prepared.contract.supports_deferred {
                            let output = format!(
                                "tool `{}` violated its lifecycle contract by returning deferred work",
                                prepared.tool_name
                            );
                            let projected = self
                                .finish_tool_in_kernel(RuntimeToolOutcome {
                                    turn_id: turn_id.to_string(),
                                    invocation_id: prepared.tool_use_id.clone(),
                                    tool_name: prepared.tool_name.clone(),
                                    input: prepared.effective_input.clone(),
                                    output: output.clone(),
                                    iteration,
                                    outcome: RuntimeToolOutcomeKind::Failed,
                                })
                                .await?;
                            reporter.on_tool_result(
                                &prepared.tool_name,
                                &prepared.effective_input,
                                &output,
                                true,
                            );
                            reporter.on_tool_use_end();
                            return Ok(PreparedToolExecution::Completed(
                                ConversationMessage::tool_result(
                                    prepared.tool_use_id.clone(),
                                    prepared.tool_name.clone(),
                                    projected.model_output,
                                    true,
                                ),
                            ));
                        }
                        let _ = self
                            .finish_tool_in_kernel(RuntimeToolOutcome {
                                turn_id: turn_id.to_string(),
                                invocation_id: prepared.tool_use_id.clone(),
                                tool_name: prepared.tool_name.clone(),
                                input: prepared.effective_input.clone(),
                                output: metadata.clone(),
                                iteration,
                                outcome: RuntimeToolOutcomeKind::Deferred,
                            })
                            .await?;
                        Ok(PreparedToolExecution::Deferred(DeferredToolUse {
                            tool_use_id: prepared.tool_use_id.clone(),
                            tool_name: prepared.tool_name.clone(),
                            input: prepared.effective_input.clone(),
                            metadata,
                        }))
                    }
                }
            }
            PermissionOutcome::Deny { reason } => {
                self.record_tool_started(iteration, &prepared.tool_name);
                reporter.on_tool_use_start(&prepared.tool_name);
                reporter.on_tool_input_delta(&prepared.effective_input);
                let merged =
                    merge_hook_feedback(prepared.pre_hook_result.messages(), reason.clone(), true);
                reporter.on_tool_result(
                    &prepared.tool_name,
                    &prepared.effective_input,
                    &merged,
                    true,
                );
                reporter.on_tool_use_end();
                let projected = self
                    .finish_tool_in_kernel(RuntimeToolOutcome {
                        turn_id: turn_id.to_string(),
                        invocation_id: prepared.tool_use_id.clone(),
                        tool_name: prepared.tool_name.clone(),
                        input: prepared.effective_input.clone(),
                        output: merged,
                        iteration,
                        outcome: RuntimeToolOutcomeKind::Denied,
                    })
                    .await?;
                Ok(PreparedToolExecution::Completed(
                    ConversationMessage::tool_result(
                        prepared.tool_use_id.clone(),
                        prepared.tool_name.clone(),
                        projected.model_output,
                        true,
                    ),
                ))
            }
            PermissionOutcome::AwaitingApproval { .. } => {
                unreachable!("approval requests return before authorized intent processing")
            }
        }
    }

    async fn finalize_allowed_tool_use<R: RuntimeEventReporter>(
        &mut self,
        turn_id: &str,
        iteration: usize,
        prepared: &PreparedToolUse,
        execution: Result<String, ToolError>,
        reporter: &mut R,
    ) -> Result<ConversationMessage, RuntimeError> {
        let (mut output, mut is_error) = match execution {
            Ok(output) => (output, false),
            Err(error) => (error.to_string(), true),
        };
        output = merge_hook_feedback(prepared.pre_hook_result.messages(), output, false);

        let post_hook_result = if is_error {
            self.run_post_tool_use_failure_hook(
                &prepared.tool_name,
                &prepared.effective_input,
                &output,
            )
        } else {
            self.run_post_tool_use_hook(
                &prepared.tool_name,
                &prepared.effective_input,
                &output,
                false,
            )
        };
        if post_hook_result.is_denied()
            || post_hook_result.is_failed()
            || post_hook_result.is_cancelled()
        {
            is_error = true;
        }
        output = merge_hook_feedback(
            post_hook_result.messages(),
            output,
            post_hook_result.is_denied()
                || post_hook_result.is_failed()
                || post_hook_result.is_cancelled(),
        );

        let projected = self
            .finish_tool_in_kernel(RuntimeToolOutcome {
                turn_id: turn_id.to_string(),
                invocation_id: prepared.tool_use_id.clone(),
                tool_name: prepared.tool_name.clone(),
                input: prepared.effective_input.clone(),
                output,
                iteration,
                outcome: if is_error {
                    RuntimeToolOutcomeKind::Failed
                } else {
                    RuntimeToolOutcomeKind::Completed
                },
            })
            .await?;
        reporter.on_tool_result(
            &prepared.tool_name,
            &prepared.effective_input,
            &projected.model_output,
            is_error,
        );
        reporter.on_tool_use_end();
        Ok(ConversationMessage::tool_result(
            prepared.tool_use_id.clone(),
            prepared.tool_name.clone(),
            projected.model_output,
            is_error,
        ))
    }

    async fn authorize_tool_in_kernel(
        &self,
        turn_id: &str,
        iteration: usize,
        prepared: &PreparedToolUse,
    ) -> Result<(), RuntimeError> {
        let Some(kernel) = self.execution_kernel.clone() else {
            return Ok(());
        };
        let (authorized, denial_reason) = match &prepared.permission_outcome {
            PermissionOutcome::Allow => (true, None),
            PermissionOutcome::Deny { reason } => (false, Some(reason.clone())),
            PermissionOutcome::AwaitingApproval { .. } => {
                return Err(RuntimeError::new(
                    "approval request cannot be recorded as an authorized tool intent",
                ));
            }
        };
        kernel
            .authorize_tool(&RuntimeToolIntent::new_with_contract(
                turn_id,
                &prepared.tool_use_id,
                &prepared.tool_name,
                &prepared.effective_input,
                iteration,
                authorized,
                denial_reason,
                prepared.contract.clone(),
            ))
            .await
            .map_err(|error| {
                RuntimeError::new(format!(
                    "tool `{}` was not executed because durable authorization failed: {error}",
                    prepared.tool_name
                ))
            })
    }

    async fn start_tool_in_kernel(
        &self,
        turn_id: &str,
        iteration: usize,
        prepared: &PreparedToolUse,
    ) -> Result<(), RuntimeError> {
        let Some(kernel) = self.execution_kernel.clone() else {
            return Ok(());
        };
        kernel
            .start_tool(&RuntimeToolIntent::new_with_contract(
                turn_id,
                &prepared.tool_use_id,
                &prepared.tool_name,
                &prepared.effective_input,
                iteration,
                true,
                None,
                prepared.contract.clone(),
            ))
            .await
            .map_err(|error| {
                RuntimeError::new(format!(
                    "tool `{}` was not dispatched because its durable start transition failed: {error}",
                    prepared.tool_name
                ))
            })
    }

    async fn finish_tool_in_kernel(
        &self,
        outcome: RuntimeToolOutcome,
    ) -> Result<crate::RuntimeToolProjection, RuntimeError> {
        match self.execution_kernel.clone() {
            Some(kernel) => kernel.finish_tool(outcome).await,
            None => Ok(crate::RuntimeToolProjection::inline(outcome.output)),
        }
    }

    async fn finish_turn_in_kernel(
        &self,
        turn_id: &str,
        status: RuntimeTurnTerminalStatus,
        detail: Option<&str>,
    ) -> Result<(), RuntimeError> {
        match self.execution_kernel.clone() {
            Some(kernel) => {
                kernel
                    .finish_turn_with_checkpoint(turn_id, status, detail, &self.session)
                    .await
            }
            None => Ok(()),
        }
    }

    pub async fn checkpoint_session_in_kernel(&self, reason: &str) -> Result<(), RuntimeError> {
        match self.execution_kernel.clone() {
            Some(kernel) => kernel.checkpoint_session(reason, &self.session).await,
            None => Ok(()),
        }
    }

    fn observe_repeated_tool_failure(
        &mut self,
        accumulator: &mut TurnAccumulator,
        prepared: &PreparedToolUse,
        result_message: &ConversationMessage,
    ) {
        let Some((tool_name, output, true)) = message_tool_result(result_message) else {
            return;
        };
        let signature = format!("{tool_name}:{}", stable_context_hash(output));
        let count = accumulator
            .repeated_tool_failures
            .entry(signature)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        if *count < 2
            || !accumulator
                .failure_blocked_tools
                .insert(tool_name.to_string())
        {
            return;
        }
        self.api_client.block_tool_for_turn(tool_name);
        self.record_runtime_warning(
            "repeated_tool_failure_circuit_opened",
            &format!(
                "tool {tool_name} returned the same failure repeatedly; disabled for the remainder of this turn (latest input hash={})",
                stable_context_hash(&prepared.effective_input)
            ),
        );
    }

    fn record_runtime_warning(&self, name: &str, message: &str) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("message".to_string(), Value::String(message.to_string()));
        session_tracer.record(name, attributes);
    }

    fn activate_tools_from_search_result(&mut self, message: &ConversationMessage) {
        crate::behavior_trace("TOOL-002");
        let Some((tool_name, output, _)) = message_tool_result(message) else {
            return;
        };
        if !tool_name.eq_ignore_ascii_case("ToolSearch")
            && !tool_name.eq_ignore_ascii_case("tool_search")
        {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(output) else {
            return;
        };
        let matches = value
            .get("matches")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !matches.is_empty() {
            self.api_client.activate_tool_candidates(&matches);
            if let Some(tracer) = &self.session_tracer {
                let mut attributes = Map::new();
                attributes.insert("source".into(), Value::String("ToolSearch".into()));
                attributes.insert(
                    "tools".into(),
                    Value::Array(matches.into_iter().map(Value::String).collect()),
                );
                tracer.record("tool_capability_activated", attributes);
            }
        }
    }

    fn record_tool_started(&self, iteration: usize, tool_name: &str) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("iteration".to_string(), Value::from(iteration as u64));
        attributes.insert(
            "tool_name".to_string(),
            Value::String(tool_name.to_string()),
        );
        session_tracer.record("tool_execution_started", attributes);
    }

    fn record_tool_finished(&self, iteration: usize, result_message: &ConversationMessage) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let Some(ContentBlock::ToolResult {
            tool_name,
            is_error,
            ..
        }) = result_message.blocks.first()
        else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("iteration".to_string(), Value::from(iteration as u64));
        attributes.insert("tool_name".to_string(), Value::String(tool_name.clone()));
        attributes.insert("is_error".to_string(), Value::Bool(*is_error));
        session_tracer.record("tool_execution_finished", attributes);
    }

    fn record_turn_completed(&self, summary: &TurnSummary) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert(
            "iterations".to_string(),
            Value::from(summary.iterations as u64),
        );
        attributes.insert(
            "assistant_messages".to_string(),
            Value::from(summary.assistant_messages.len() as u64),
        );
        attributes.insert(
            "tool_results".to_string(),
            Value::from(summary.tool_results.len() as u64),
        );
        attributes.insert(
            "prompt_cache_events".to_string(),
            Value::from(summary.prompt_cache_events.len() as u64),
        );
        session_tracer.record("turn_completed", attributes);
    }

    fn record_turn_failed(&self, iteration: usize, error: &RuntimeError) {
        let Some(session_tracer) = &self.session_tracer else {
            return;
        };

        let mut attributes = Map::new();
        attributes.insert("iteration".to_string(), Value::from(iteration as u64));
        attributes.insert("error".to_string(), Value::String(error.to_string()));
        session_tracer.record("turn_failed", attributes);
    }

    fn complete_session_turn(&mut self, turn_id: &str, status: SessionTurnStatus) {
        if let Err(error) = self.session.complete_turn(turn_id, status) {
            if let Some(session_tracer) = &self.session_tracer {
                let mut attributes = Map::new();
                attributes.insert(
                    "error".to_string(),
                    Value::String(format!("failed to persist turn completion: {error}")),
                );
                attributes.insert(
                    "status".to_string(),
                    Value::String(status.as_str().to_string()),
                );
                session_tracer.record("turn_completion_persist_failed", attributes);
            }
        }
    }
}

fn turn_summary_from_accumulator<C, T>(
    runtime: &ConversationRuntime<C, T>,
    accumulator: TurnAccumulator,
) -> TurnSummary
where
    C: ApiClient,
    T: ToolExecutor,
{
    TurnSummary {
        assistant_messages: accumulator.assistant_messages,
        tool_results: accumulator.tool_results,
        prompt_cache_events: accumulator.prompt_cache_events,
        iterations: accumulator.iterations,
        usage: runtime.usage_tracker.cumulative_usage(),
        auto_compaction: accumulator.auto_compaction,
    }
}

fn pending_tool_uses_for_turn(
    session: &Session,
    turn: &crate::session::SessionTurn,
) -> Vec<(String, String, String)> {
    let messages = session
        .messages
        .get(turn.start_message_count..)
        .unwrap_or_default();
    let completed = messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } if !completed.contains(id) => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect()
}

fn compact_tool_results_for_request(
    messages: &[ConversationMessage],
    config: &crate::config::RuntimeContextManagementConfig,
) -> (Vec<ConversationMessage>, Option<ContextManagementReport>) {
    let tool_result_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| message_tool_result(message).map(|_| index))
        .collect::<Vec<_>>();
    if tool_result_indices.len() <= config.preserve_recent_tool_results() {
        return (messages.to_vec(), None);
    }

    let preserved = tool_result_indices
        .iter()
        .rev()
        .take(config.preserve_recent_tool_results())
        .copied()
        .collect::<BTreeSet<_>>();

    let mut original_tool_result_chars = 0usize;
    let mut request_tool_result_chars = 0usize;
    let mut compacted_tool_results = 0usize;
    let mut request_messages = Vec::with_capacity(messages.len());
    let mut seen_tool_result_hashes = HashSet::new();

    for (index, message) in messages.iter().enumerate() {
        let Some((tool_name, output, is_error)) = message_tool_result(message) else {
            request_messages.push(message.clone());
            continue;
        };
        original_tool_result_chars = original_tool_result_chars.saturating_add(output.len());
        let output_hash = stable_context_hash(output);
        let is_duplicate = !seen_tool_result_hashes.insert(format!("{tool_name}:{output_hash}"));
        let exceeds_budget = !preserved.contains(&index)
            && request_tool_result_chars.saturating_add(output.len())
                > config.request_tool_result_budget_chars();

        let should_compact = !preserved.contains(&index)
            && !is_error
            && is_compactable_tool_result(tool_name)
            && (is_duplicate || exceeds_budget || output.len() > config.large_tool_result_chars());

        if should_compact {
            let compact_output = compact_tool_result_output(
                tool_name,
                output,
                config.compacted_tool_result_chars(),
                is_duplicate,
                exceeds_budget,
            );
            request_tool_result_chars =
                request_tool_result_chars.saturating_add(compact_output.len());
            compacted_tool_results = compacted_tool_results.saturating_add(1);
            request_messages.push(replace_tool_result_output(message, &compact_output));
        } else {
            request_tool_result_chars = request_tool_result_chars.saturating_add(output.len());
            request_messages.push(message.clone());
        }
    }

    let report = (compacted_tool_results > 0).then_some(ContextManagementReport {
        original_tool_result_chars,
        request_tool_result_chars,
        compacted_tool_results,
        preserved_recent_tool_results: config.preserve_recent_tool_results(),
    });
    (request_messages, report)
}

fn message_tool_result(message: &ConversationMessage) -> Option<(&str, &str, bool)> {
    message.blocks.iter().find_map(|block| {
        if let ContentBlock::ToolResult {
            tool_name,
            output,
            is_error,
            ..
        } = block
        {
            Some((tool_name.as_str(), output.as_str(), *is_error))
        } else {
            None
        }
    })
}

fn is_compactable_tool_result(tool_name: &str) -> bool {
    matches!(tool_name, "read_file" | "grep_search" | "glob_search")
}

fn replace_tool_result_output(
    message: &ConversationMessage,
    compact_output: &str,
) -> ConversationMessage {
    let mut next = message.clone();
    for block in &mut next.blocks {
        if let ContentBlock::ToolResult { output, .. } = block {
            compact_output.clone_into(output);
        }
    }
    next
}

fn compact_tool_result_output(
    tool_name: &str,
    output: &str,
    max_chars: usize,
    duplicate: bool,
    budget_limited: bool,
) -> String {
    let output_hash = stable_context_hash(output);
    let original_chars = output.len();
    let recovery = tool_result_recovery_summary(tool_name, output);
    let reason = compact_reason(original_chars, duplicate, budget_limited);
    let preview = compact_text_preview(output, max_chars);
    format!(
        "[recoverable tool result compacted]\n\
         tool={tool_name}\n\
         reason={reason}\n\
         original_chars={original_chars}\n\
         output_hash={output_hash}\n\
         recovery={recovery}\n\
         note=This is a compact request view, not full evidence. Before exact code edits, quoted claims, or final verification, re-read the focused file range or rerun the focused search.\n\n\
         Preview:\n{preview}"
    )
}

fn compact_reason(original_chars: usize, duplicate: bool, budget_limited: bool) -> String {
    let mut reasons = Vec::new();
    if duplicate {
        reasons.push("duplicate");
    }
    if budget_limited {
        reasons.push("request_budget");
    }
    reasons.push(if duplicate || budget_limited {
        "large_or_recoverable"
    } else {
        "large"
    });
    format!("{}; original_chars={original_chars}", reasons.join("+"))
}

fn tool_result_recovery_summary(tool_name: &str, output: &str) -> String {
    let parsed = serde_json::from_str::<Value>(output).ok();
    match tool_name {
        "read_file" => parsed
            .as_ref()
            .and_then(read_file_recovery_summary)
            .unwrap_or_else(|| "rerun=read_file with focused path/offset/limit".to_string()),
        "grep_search" => parsed
            .as_ref()
            .and_then(grep_search_recovery_summary)
            .unwrap_or_else(|| {
                "rerun=grep_search with focused pattern/path/head_limit".to_string()
            }),
        "glob_search" => parsed
            .as_ref()
            .and_then(glob_search_recovery_summary)
            .unwrap_or_else(|| "rerun=glob_search with focused pattern/path".to_string()),
        _ => "rerun=call the same tool with a narrower input".to_string(),
    }
}

fn read_file_recovery_summary(value: &Value) -> Option<String> {
    let file = value.get("file")?;
    let path = file.get("filePath")?.as_str()?;
    let start_line = file.get("startLine").and_then(Value::as_u64).unwrap_or(1);
    let num_lines = file.get("numLines").and_then(Value::as_u64).unwrap_or(0);
    let total_lines = file.get("totalLines").and_then(Value::as_u64).unwrap_or(0);
    let end_line = if num_lines == 0 {
        start_line
    } else {
        start_line.saturating_add(num_lines).saturating_sub(1)
    };
    let offset = start_line.saturating_sub(1);
    let limit = num_lines.clamp(1, 400);
    Some(format!(
        "file={path}; lines={start_line}-{end_line}; total_lines={total_lines}; exact=read_file path={path} offset={offset} limit={limit}"
    ))
}

fn grep_search_recovery_summary(value: &Value) -> Option<String> {
    let filenames = compact_json_string_list(value.get("filenames")?, 8)?;
    let num_files = value.get("numFiles").and_then(Value::as_u64).unwrap_or(0);
    let num_matches = value.get("numMatches").and_then(Value::as_u64);
    let applied_limit = value.get("appliedLimit").and_then(Value::as_u64);
    let mut parts = vec![
        format!("matched_files={num_files}"),
        format!("files=[{filenames}]"),
    ];
    if let Some(matches) = num_matches {
        parts.push(format!("matches={matches}"));
    }
    if let Some(limit) = applied_limit {
        parts.push(format!("applied_limit={limit}"));
    }
    parts.push(
        "exact=rerun grep_search with focused pattern/path/head_limit, then read_file exact ranges"
            .to_string(),
    );
    Some(parts.join("; "))
}

fn glob_search_recovery_summary(value: &Value) -> Option<String> {
    let filenames = compact_json_string_list(value.get("filenames")?, 8)?;
    let num_files = value.get("numFiles").and_then(Value::as_u64).unwrap_or(0);
    Some(format!(
        "matched_files={num_files}; files=[{filenames}]; exact=rerun glob_search with focused pattern/path"
    ))
}

fn compact_json_string_list(value: &Value, max_items: usize) -> Option<String> {
    let array = value.as_array()?;
    let mut items = array
        .iter()
        .filter_map(Value::as_str)
        .take(max_items)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if array.len() > max_items {
        items.push(format!("...+{}", array.len().saturating_sub(max_items)));
    }
    Some(items.join(", "))
}

fn compact_text_preview(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let head_chars = max_chars.saturating_mul(2) / 3;
    let tail_chars = max_chars.saturating_sub(head_chars).saturating_sub(96);
    let head = text.chars().take(head_chars).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n\n[...middle omitted from request view...]\n\n{tail}")
}

fn stable_context_hash(value: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Reads the automatic compaction threshold from the environment.
#[must_use]
pub fn auto_compaction_threshold_from_env() -> u32 {
    parse_auto_compaction_threshold(
        std::env::var(AUTO_COMPACTION_THRESHOLD_ENV_VAR)
            .ok()
            .as_deref(),
    )
}

#[must_use]
pub fn trident_compaction_enabled_from_env() -> bool {
    std::env::var(TRIDENT_COMPACTION_ENV_VAR)
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "classic" | "legacy"
            )
        })
        .unwrap_or(true)
}

fn compact_runtime_session(session: &Session, config: CompactionConfig) -> CompactionResult {
    if trident_compaction_enabled_from_env() {
        trident_compact_session(session, config, &TridentConfig::default())
    } else {
        compact_session(session, config)
    }
}

#[must_use]
fn parse_auto_compaction_threshold(value: Option<&str>) -> u32 {
    value
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|threshold| *threshold > 0)
        .unwrap_or(DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD)
}

fn build_assistant_message(
    events: Vec<AssistantEvent>,
) -> Result<
    (
        ConversationMessage,
        Option<TokenUsage>,
        Vec<PromptCacheEvent>,
    ),
    RuntimeError,
> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut thinking = String::new();
    let mut signature = String::new();
    let mut prompt_cache_events = Vec::new();
    let mut finished = false;
    let mut usage = None;
    let mut in_thinking = false;

    tracing::debug!(
        "build_assistant_message: processing {} events",
        events.len()
    );
    for event in events {
        match event {
            AssistantEvent::TextDelta(delta) => {
                text.push_str(&delta);
            }
            AssistantEvent::ThinkingDelta(delta) => {
                if !in_thinking {
                    in_thinking = true;
                }
                thinking.push_str(&delta);
            }
            AssistantEvent::SignatureDelta(delta) => {
                signature.push_str(&delta);
            }
            AssistantEvent::ToolUse { id, name, input } => {
                if in_thinking {
                    in_thinking = false;
                }
                flush_text_block(&mut text, &mut blocks);
                blocks.push(ContentBlock::ToolUse { id, name, input });
            }
            AssistantEvent::Usage(value) => usage = Some(value),
            AssistantEvent::PromptCache(event) => prompt_cache_events.push(event),
            AssistantEvent::MessageStop => {
                if in_thinking {
                    in_thinking = false;
                }
                finished = true;
            }
        }
    }

    flush_text_block(&mut text, &mut blocks);
    tracing::debug!(
        "build_assistant_message: after processing - text.len={}, blocks.len={}, finished={}",
        text.len(),
        blocks.len(),
        finished
    );

    if !finished {
        return Err(RuntimeError::new(
            "assistant stream ended without a message stop event",
        ));
    }
    if blocks.is_empty() {
        return Err(RuntimeError::new("assistant stream produced no content"));
    }

    let thinking_opt = if thinking.is_empty() {
        None
    } else {
        Some(thinking)
    };
    let thinking_signature_opt = if signature.is_empty() {
        None
    } else {
        Some(signature)
    };
    Ok((
        ConversationMessage {
            role: MessageRole::Assistant,
            blocks,
            thinking: thinking_opt,
            thinking_signature: thinking_signature_opt,
            usage,
        },
        usage,
        prompt_cache_events,
    ))
}

fn replay_assistant_events_to_reporter(
    events: &[AssistantEvent],
    reporter: &mut (dyn RuntimeEventReporter + Send),
) {
    let mut in_thinking = false;
    for event in events {
        match event {
            AssistantEvent::TextDelta(delta) => reporter.on_text_delta(delta),
            AssistantEvent::ThinkingDelta(delta) => {
                if !in_thinking {
                    reporter.on_thinking_start();
                    in_thinking = true;
                }
                reporter.on_thinking_delta(delta);
            }
            AssistantEvent::SignatureDelta(_)
            | AssistantEvent::Usage(_)
            | AssistantEvent::PromptCache(_) => {}
            AssistantEvent::ToolUse { .. } | AssistantEvent::MessageStop => {
                if in_thinking {
                    reporter.on_thinking_end();
                    in_thinking = false;
                }
            }
        }
    }
    if in_thinking {
        reporter.on_thinking_end();
    }
}

fn flush_text_block(text: &mut String, blocks: &mut Vec<ContentBlock>) {
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text: std::mem::take(text),
        });
    }
}

fn format_hook_message(result: &HookRunResult, fallback: &str) -> String {
    if result.messages().is_empty() {
        fallback.to_string()
    } else {
        result.messages().join("\n")
    }
}

fn merge_hook_feedback(messages: &[String], output: String, is_error: bool) -> String {
    if messages.is_empty() {
        return output;
    }

    let mut sections = Vec::new();
    if !output.trim().is_empty() {
        sections.push(output);
    }
    let label = if is_error {
        "Hook feedback (error)"
    } else {
        "Hook feedback"
    };
    sections.push(format!("{label}:\n{}", messages.join("\n")));
    sections.join("\n\n")
}

type ToolHandler = Box<dyn FnMut(&str) -> Result<String, ToolError>>;

/// Simple in-memory tool executor for tests and lightweight integrations.
#[derive(Default)]
pub struct StaticToolExecutor {
    handlers: BTreeMap<String, ToolHandler>,
}

impl StaticToolExecutor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn register(
        mut self,
        tool_name: impl Into<String>,
        handler: impl FnMut(&str) -> Result<String, ToolError> + 'static,
    ) -> Self {
        self.handlers.insert(tool_name.into(), Box::new(handler));
        self
    }
}

impl ToolExecutor for StaticToolExecutor {
    fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
        self.handlers
            .get_mut(tool_name)
            .ok_or_else(|| ToolError::new(format!("unknown tool: {tool_name}")))?(input)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_assistant_message, compile_model_visible_request_with_budget, message_tool_result,
        parse_auto_compaction_threshold, should_auto_compact, stable_context_hash, ApiClient,
        ApiRequest, AssistantEvent, CompactionHook, ConversationRuntime, DeferredApprovalDecision,
        DeferredToolResult, PreparedCompaction, PromptCacheEvent, ResumableTurnOutcome,
        RuntimeError, StaticToolExecutor, ToolExecutionOutcome, ToolExecutionRequest, ToolExecutor,
        DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD,
    };
    use crate::compact::{CompactionConfig, CompactionResult};
    use crate::config::{RuntimeFeatureConfig, RuntimeHookConfig};
    use crate::execution_kernel::{
        AgentExecutionKernel, RuntimeApprovalDecision, RuntimeApprovalRequest,
        RuntimeApprovalResolution, RuntimeContextManifestInput, RuntimeInteractionRequest,
        RuntimeManifestLineage, RuntimeToolContract, RuntimeToolIntent, RuntimeToolOutcome,
        RuntimeToolProjection, RuntimeTurnStart, RuntimeTurnTerminalStatus,
    };
    use crate::permissions::{
        PermissionMode, PermissionPolicy, PermissionPromptDecision, PermissionPrompter,
        PermissionRequest,
    };
    use crate::prompt::{ProjectContext, SystemPromptBuilder};
    use crate::session::{
        ContentBlock, ConversationMessage, MessageRole, Session, SessionTurnStatus,
    };
    use crate::usage::TokenUsage;
    use crate::ToolError;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use telemetry::{MemoryTelemetrySink, SessionTracer, TelemetryEvent};

    /// Keep compaction fixtures large enough that the framed continuation is
    /// meaningfully smaller than the archived window. Production compaction
    /// must still fail closed for a replacement that grows the context.
    fn compaction_fixture_text(label: &str) -> String {
        format!("{label}: {}", "important context ".repeat(128))
    }

    #[test]
    fn context_compiler_changes_the_actual_provider_request() {
        let (system, messages) = compile_model_visible_request_with_budget(
            vec!["stable instructions".into()],
            vec![
                ConversationMessage::user_text("old request"),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "old answer".into(),
                }]),
                ConversationMessage::user_text("current request"),
            ],
            16,
        )
        .expect("request should fit after dropping old context");
        assert_eq!(system, vec!["stable instructions"]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            ConversationMessage::user_text("current request")
        );
    }

    #[test]
    fn context_compiler_never_emits_half_a_tool_transaction() {
        let assistant_tool = ConversationMessage::assistant(vec![ContentBlock::ToolUse {
            id: "tool-1".into(),
            name: "large_tool".into(),
            input: "large input ".repeat(200),
        }]);
        let result =
            ConversationMessage::tool_result("tool-1", "large_tool", "short result", false);
        let (_, messages) = compile_model_visible_request_with_budget(
            vec!["system".into()],
            vec![
                ConversationMessage::user_text("old request"),
                assistant_tool,
                result,
                ConversationMessage::user_text("current request"),
            ],
            24,
        )
        .expect("request selection");
        assert!(messages
            .iter()
            .all(|message| message.blocks.iter().all(|block| !matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            ))));
        assert!(messages
            .iter()
            .any(|message| message == &ConversationMessage::user_text("current request")));
    }

    #[tokio::test]
    async fn context_compiler_budget_controls_the_request_observed_by_provider() {
        struct CapturingApi {
            request: Arc<Mutex<Option<ApiRequest>>>,
        }
        #[::async_trait::async_trait]
        impl ApiClient for CapturingApi {
            fn context_window_tokens(&self) -> Option<u64> {
                Some(96)
            }
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                *self.request.lock().unwrap() = Some(request);
                Ok(vec![
                    AssistantEvent::TextDelta("done".into()),
                    AssistantEvent::MessageStop,
                ])
            }
        }
        let captured = Arc::new(Mutex::new(None));
        let mut session = Session::new();
        session
            .messages
            .push(ConversationMessage::user_text("old context ".repeat(120)));
        session
            .messages
            .push(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "old answer ".repeat(120),
            }]));
        let mut runtime = ConversationRuntime::new(
            session,
            CapturingApi {
                request: captured.clone(),
            },
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["stable system".into()],
        );
        runtime.run_turn("current request", None, ()).await.unwrap();
        let request = captured.lock().unwrap().clone().unwrap();
        assert_eq!(request.system_prompt, vec!["stable system"]);
        assert_eq!(
            request.messages,
            vec![ConversationMessage::user_text("current request")]
        );
    }

    struct ScriptedApiClient {
        call_count: usize,
    }

    #[::async_trait::async_trait]
    impl ApiClient for ScriptedApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => {
                    assert!(request
                        .messages
                        .iter()
                        .any(|message| message.role == MessageRole::User));
                    Ok(vec![
                        AssistantEvent::TextDelta("Let me calculate that.".to_string()),
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "add".to_string(),
                            input: "2,2".to_string(),
                        },
                        AssistantEvent::Usage(TokenUsage {
                            input_tokens: 20,
                            output_tokens: 6,
                            cache_creation_input_tokens: 1,
                            cache_read_input_tokens: 2,
                        }),
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    let last_message = request
                        .messages
                        .last()
                        .expect("tool result should be present");
                    assert_eq!(last_message.role, MessageRole::Tool);
                    Ok(vec![
                        AssistantEvent::TextDelta("The answer is 4.".to_string()),
                        AssistantEvent::Usage(TokenUsage {
                            input_tokens: 24,
                            output_tokens: 4,
                            cache_creation_input_tokens: 1,
                            cache_read_input_tokens: 3,
                        }),
                        AssistantEvent::PromptCache(PromptCacheEvent {
                            unexpected: true,
                            reason:
                                "cache read tokens dropped while prompt fingerprint remained stable"
                                    .to_string(),
                            previous_cache_read_input_tokens: 6_000,
                            current_cache_read_input_tokens: 1_000,
                            token_drop: 5_000,
                        }),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra API call"),
            }
        }
    }

    struct PromptAllowOnce;

    impl PermissionPrompter for PromptAllowOnce {
        fn decide(&mut self, request: &PermissionRequest) -> PermissionPromptDecision {
            assert_eq!(request.tool_name, "add");
            PermissionPromptDecision::Allow
        }
    }

    struct PromptDefer;

    impl PermissionPrompter for PromptDefer {
        fn decide(&mut self, _request: &PermissionRequest) -> PermissionPromptDecision {
            PermissionPromptDecision::Defer
        }
    }

    struct ApprovalApiClient {
        call_count: usize,
    }

    #[::async_trait::async_trait]
    impl ApiClient for ApprovalApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "approval-tool-1".to_string(),
                        name: "write_external".to_string(),
                        input: r#"{"value":"durable"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ]),
                2 => {
                    assert!(matches!(
                        request.messages.last().and_then(|message| message.blocks.first()),
                        Some(ContentBlock::ToolResult { tool_use_id, .. })
                            if tool_use_id == "approval-tool-1"
                    ));
                    Ok(vec![
                        AssistantEvent::TextDelta("approval handled".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("approval flow made an extra provider call"),
            }
        }
    }

    struct CountingApprovalExecutor {
        executions: Arc<AtomicUsize>,
    }

    impl ToolExecutor for CountingApprovalExecutor {
        fn execute(&mut self, tool_name: &str, _input: &str) -> Result<String, ToolError> {
            assert_eq!(tool_name, "write_external");
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok("external write completed".to_string())
        }
    }

    #[derive(Default)]
    struct ApprovalKernelState {
        approval_requests: Vec<RuntimeApprovalRequest>,
        resolutions: Vec<RuntimeApprovalResolution>,
        authorized: Vec<RuntimeToolIntent>,
        started: BTreeSet<String>,
        finished: Vec<RuntimeToolOutcome>,
        visible_messages: Vec<(String, ConversationMessage)>,
        visible_attempt_ids: Vec<String>,
        reject_visible_message: bool,
        terminal_checkpoints: Vec<(RuntimeTurnTerminalStatus, SessionTurnStatus)>,
    }

    struct ApprovalTestKernel {
        state: Mutex<ApprovalKernelState>,
    }

    impl ApprovalTestKernel {
        fn new() -> Self {
            Self {
                state: Mutex::new(ApprovalKernelState::default()),
            }
        }
    }

    #[::async_trait::async_trait]
    impl AgentExecutionKernel for ApprovalTestKernel {
        async fn recover(&self) -> Result<(), RuntimeError> {
            Ok(())
        }

        async fn start_turn(&self, _input: RuntimeTurnStart) -> Result<(), RuntimeError> {
            Ok(())
        }

        async fn current_turn_revision(&self, _turn_id: &str) -> Result<u64, RuntimeError> {
            Ok(0)
        }

        async fn record_context_manifest(
            &self,
            input: RuntimeContextManifestInput,
        ) -> Result<RuntimeManifestLineage, RuntimeError> {
            Ok(RuntimeManifestLineage {
                context_manifest_id: format!("context:{}:{}", input.turn_id, input.iteration),
                prompt_manifest_id: input
                    .prompt_manifest
                    .as_ref()
                    .map(|_| format!("prompt:{}:{}", input.turn_id, input.iteration)),
                context_manifest_hash: stable_context_hash(&format!(
                    "{}:{}",
                    input.turn_id, input.iteration
                )),
                prompt_manifest_hash: input
                    .prompt_manifest
                    .as_ref()
                    .map(|manifest| stable_context_hash(&format!("{manifest:?}"))),
            })
        }

        async fn record_assistant_message(
            &self,
            _turn_id: &str,
            _iteration: usize,
            _message: &ConversationMessage,
        ) -> Result<(), RuntimeError> {
            Ok(())
        }

        async fn record_visible_message(
            &self,
            message_id: &str,
            message: &ConversationMessage,
        ) -> Result<(), RuntimeError> {
            let mut state = self.state.lock().unwrap();
            state.visible_attempt_ids.push(message_id.to_string());
            if state.reject_visible_message {
                return Err(RuntimeError::new("durable visible message write failed"));
            }
            state
                .visible_messages
                .push((message_id.to_string(), message.clone()));
            Ok(())
        }

        async fn authorize_tool(&self, intent: &RuntimeToolIntent) -> Result<(), RuntimeError> {
            self.state.lock().unwrap().authorized.push(intent.clone());
            Ok(())
        }

        async fn start_tool(&self, intent: &RuntimeToolIntent) -> Result<(), RuntimeError> {
            let mut state = self.state.lock().unwrap();
            if !state.started.insert(intent.invocation_id.clone()) {
                return Err(RuntimeError::new("duplicate durable tool dispatch"));
            }
            Ok(())
        }

        async fn request_approval(
            &self,
            request: &RuntimeApprovalRequest,
        ) -> Result<(), RuntimeError> {
            let mut state = self.state.lock().unwrap();
            if !state
                .approval_requests
                .iter()
                .any(|existing| existing.invocation_id == request.invocation_id)
            {
                state.approval_requests.push(request.clone());
            }
            Ok(())
        }

        async fn resolve_approval(
            &self,
            resolution: &RuntimeApprovalResolution,
        ) -> Result<RuntimeApprovalDecision, RuntimeError> {
            let mut state = self.state.lock().unwrap();
            if state
                .resolutions
                .iter()
                .any(|existing| existing.invocation_id == resolution.invocation_id)
            {
                return Err(RuntimeError::new("approval was already resolved"));
            }
            state.resolutions.push(resolution.clone());
            Ok(resolution.decision)
        }

        async fn request_interaction(
            &self,
            request: &RuntimeInteractionRequest,
        ) -> Result<agent_protocol::DurableInteraction, RuntimeError> {
            Ok(agent_protocol::DurableInteraction {
                interaction_id: request.interaction_id.clone(),
                kind: request.kind,
                state: agent_protocol::InteractionState::Pending,
                scope: agent_protocol::InteractionScope {
                    tenant_id: "tenant".into(),
                    user_id: request.owner_user_id.clone(),
                    session_id: "session".into(),
                    turn_id: request.turn_id.clone(),
                    invocation_id: request.invocation_id.clone(),
                },
                owner_user_id: request.owner_user_id.clone(),
                allowed_responder_ids: request.allowed_responder_ids.clone(),
                capability_requirement: request.capability_requirement.clone(),
                request_schema_hash: request.request_schema_hash.clone(),
                choice_schema_hash: request.choice_schema_hash.clone(),
                display_projection: request.display_projection.clone(),
                response_projection: None,
                encrypted_secret_ref: None,
                idempotency_key: request.idempotency_key.clone(),
                expected_turn_revision: request.expected_turn_revision,
                expires_at: request.expires_at,
                created_event_id: None,
                response_event_id: None,
                consumed_event_id: None,
            })
        }

        async fn respond_interaction(
            &self,
            _resolution: &crate::RuntimeInteractionResolution,
        ) -> Result<agent_protocol::DurableInteraction, RuntimeError> {
            Err(RuntimeError::new("test interaction was not created"))
        }

        async fn consume_interaction(
            &self,
            _interaction_id: &str,
            _turn_id: &str,
            _idempotency_key: &str,
        ) -> Result<agent_protocol::DurableInteraction, RuntimeError> {
            Err(RuntimeError::new("test interaction was not answered"))
        }

        async fn finish_tool(
            &self,
            outcome: RuntimeToolOutcome,
        ) -> Result<RuntimeToolProjection, RuntimeError> {
            let projection = RuntimeToolProjection::inline(outcome.output.clone());
            self.state.lock().unwrap().finished.push(outcome);
            Ok(projection)
        }

        async fn finish_turn_with_checkpoint(
            &self,
            turn_id: &str,
            status: RuntimeTurnTerminalStatus,
            _detail: Option<&str>,
            session: &Session,
        ) -> Result<(), RuntimeError> {
            let session_status = session
                .turns
                .iter()
                .rev()
                .find(|turn| turn.turn_id == turn_id)
                .map(|turn| turn.status)
                .ok_or_else(|| RuntimeError::new("terminal checkpoint turn missing"))?;
            self.state
                .lock()
                .unwrap()
                .terminal_checkpoints
                .push((status, session_status));
            Ok(())
        }
    }

    #[tokio::test]
    async fn rolled_back_outer_cancellation_still_commits_a_cancelled_checkpoint() {
        let kernel = Arc::new(ApprovalTestKernel::new());
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            ApprovalApiClient { call_count: 0 },
            CountingApprovalExecutor {
                executions: Arc::new(AtomicUsize::new(0)),
            },
            PermissionPolicy::new(PermissionMode::Allow),
            vec!["system".to_string()],
        )
        .with_execution_kernel(kernel.clone());
        let turn_id = runtime.session_mut().begin_turn("cancel me").unwrap();
        runtime.session_mut().push_user_text("cancel me").unwrap();
        assert!(runtime.rollback_latest_turn_started_at(0).unwrap());

        runtime
            .finish_latest_kernel_turn(
                RuntimeTurnTerminalStatus::Cancelled,
                Some("outer coordinator cancelled"),
            )
            .await
            .unwrap();

        assert_eq!(runtime.session().turns[0].turn_id, turn_id);
        assert_eq!(
            runtime.session().turns[0].status,
            SessionTurnStatus::Cancelled
        );
        assert_eq!(
            kernel.state.lock().unwrap().terminal_checkpoints,
            vec![(
                RuntimeTurnTerminalStatus::Cancelled,
                SessionTurnStatus::Cancelled
            )]
        );
    }

    #[tokio::test]
    async fn external_visible_message_is_recorded_before_memory_and_retries_idempotently() {
        struct NoopApi;

        #[::async_trait::async_trait]
        impl ApiClient for NoopApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                unreachable!("visible-message append must not call the provider")
            }
        }

        let kernel = Arc::new(ApprovalTestKernel::new());
        kernel.state.lock().unwrap().reject_visible_message = true;
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            NoopApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::ReadOnly),
            Vec::new(),
        )
        .with_execution_kernel(kernel.clone());
        let user = ConversationMessage::user_text("background request");
        let error = runtime
            .append_visible_message(user.clone())
            .await
            .expect_err("a failed ledger write must block the in-memory projection");
        assert!(error
            .to_string()
            .contains("durable visible message write failed"));
        assert!(runtime.session().messages.is_empty());

        kernel.state.lock().unwrap().reject_visible_message = false;
        runtime.append_visible_message(user.clone()).await.unwrap();
        let assistant = ConversationMessage::assistant(vec![ContentBlock::Text {
            text: "background result".into(),
        }]);
        runtime
            .append_visible_message(assistant.clone())
            .await
            .unwrap();
        assert_eq!(runtime.session().messages, vec![user, assistant]);
        let state = kernel.state.lock().unwrap();
        assert_eq!(state.visible_messages.len(), 2);
        assert_eq!(state.visible_attempt_ids[0], state.visible_attempt_ids[1]);
        assert_ne!(state.visible_attempt_ids[1], state.visible_attempt_ids[2]);
    }

    fn approval_runtime(
        executions: Arc<AtomicUsize>,
        kernel: Arc<ApprovalTestKernel>,
    ) -> ConversationRuntime<ApprovalApiClient, CountingApprovalExecutor> {
        ConversationRuntime::new(
            Session::new(),
            ApprovalApiClient { call_count: 0 },
            CountingApprovalExecutor { executions },
            PermissionPolicy::new(PermissionMode::Prompt)
                .with_tool_requirement("write_external", PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_execution_kernel(kernel)
    }

    #[tokio::test]
    async fn durable_approval_suspends_then_dispatches_the_tool_exactly_once() {
        let executions = Arc::new(AtomicUsize::new(0));
        let kernel = Arc::new(ApprovalTestKernel::new());
        let mut runtime = approval_runtime(executions.clone(), kernel.clone());
        let mut prompter = PromptDefer;
        assert!(matches!(
            runtime.permission_policy.authorize(
                "write_external",
                r#"{"value":"durable"}"#,
                Some(&mut prompter),
            ),
            crate::permissions::PermissionOutcome::AwaitingApproval { .. }
        ));

        let outcome = runtime
            .run_turn_resumable("perform the external write", Some(&mut prompter), ())
            .await
            .expect("approval request should suspend the turn");
        let ResumableTurnOutcome::Suspended(suspended) = outcome else {
            panic!("approval request must suspend the same turn");
        };
        assert_eq!(suspended.deferred_tools.len(), 1);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert_eq!(kernel.state.lock().unwrap().approval_requests.len(), 1);

        let outcome = runtime
            .resume_turn_with_approval_decisions(
                vec![DeferredApprovalDecision {
                    tool_use_id: "approval-tool-1".to_string(),
                    decision: RuntimeApprovalDecision::Approved,
                    reason: Some("user confirmed".to_string()),
                }],
                None,
                (),
            )
            .await
            .expect("approved turn should resume");
        assert!(matches!(outcome, ResumableTurnOutcome::Completed(_)));
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        {
            let state = kernel.state.lock().unwrap();
            assert_eq!(state.started.len(), 1);
            assert_eq!(state.finished.len(), 1);
        }

        assert!(runtime
            .resume_turn_with_approval_decisions(
                vec![DeferredApprovalDecision {
                    tool_use_id: "approval-tool-1".to_string(),
                    decision: RuntimeApprovalDecision::Approved,
                    reason: None,
                }],
                None,
                (),
            )
            .await
            .is_err());
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn denied_expired_and_cancelled_approvals_never_dispatch_the_tool() {
        for decision in [
            RuntimeApprovalDecision::Denied,
            RuntimeApprovalDecision::Expired,
            RuntimeApprovalDecision::Cancelled,
        ] {
            let executions = Arc::new(AtomicUsize::new(0));
            let kernel = Arc::new(ApprovalTestKernel::new());
            let mut runtime = approval_runtime(executions.clone(), kernel.clone());
            let mut prompter = PromptDefer;
            assert!(matches!(
                runtime
                    .run_turn_resumable("perform the external write", Some(&mut prompter), ())
                    .await
                    .unwrap(),
                ResumableTurnOutcome::Suspended(_)
            ));

            let outcome = runtime
                .resume_turn_with_approval_decisions(
                    vec![DeferredApprovalDecision {
                        tool_use_id: "approval-tool-1".to_string(),
                        decision,
                        reason: None,
                    }],
                    None,
                    (),
                )
                .await
                .expect("a non-approval decision should resume with a denied tool result");
            assert!(matches!(outcome, ResumableTurnOutcome::Completed(_)));
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            let state = kernel.state.lock().unwrap();
            assert!(state.started.is_empty());
            assert_eq!(state.finished.len(), 1);
        }
    }

    #[tokio::test]
    async fn approval_does_not_bypass_a_policy_rule_added_while_suspended() {
        let executions = Arc::new(AtomicUsize::new(0));
        let kernel = Arc::new(ApprovalTestKernel::new());
        let mut runtime = approval_runtime(executions.clone(), kernel.clone());
        let mut prompter = PromptDefer;
        assert!(matches!(
            runtime
                .run_turn_resumable("perform the external write", Some(&mut prompter), ())
                .await
                .unwrap(),
            ResumableTurnOutcome::Suspended(_)
        ));

        runtime.permission_policy = PermissionPolicy::new(PermissionMode::Prompt)
            .with_tool_requirement("write_external", PermissionMode::DangerFullAccess)
            .with_permission_rules(&crate::config::RuntimePermissionRuleConfig::new(
                Vec::new(),
                vec!["write_external(*)".to_string()],
                Vec::new(),
            ));
        let outcome = runtime
            .resume_turn_with_approval_decisions(
                vec![DeferredApprovalDecision {
                    tool_use_id: "approval-tool-1".to_string(),
                    decision: RuntimeApprovalDecision::Approved,
                    reason: None,
                }],
                None,
                (),
            )
            .await
            .expect("policy denial should be returned to the model");
        assert!(matches!(outcome, ResumableTurnOutcome::Completed(_)));
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        let state = kernel.state.lock().unwrap();
        assert!(state.started.is_empty());
        assert_eq!(state.finished.len(), 1);
        assert!(!state.authorized[0].authorized);
    }

    #[tokio::test]
    async fn runs_user_to_tool_to_result_loop_end_to_end_and_tracks_usage() {
        let api_client = ScriptedApiClient { call_count: 0 };
        let tool_executor = StaticToolExecutor::new().register("add", |input| {
            let total = input
                .split(',')
                .map(|part| part.parse::<i32>().expect("input must be valid integer"))
                .sum::<i32>();
            Ok(total.to_string())
        });
        let permission_policy = PermissionPolicy::new(PermissionMode::WorkspaceWrite);
        let system_prompt = SystemPromptBuilder::new()
            .with_project_context(ProjectContext {
                cwd: PathBuf::from("/tmp/project"),
                current_date: "2026-03-31".to_string(),
                git_status: None,
                git_diff: None,
                git_context: None,
                instruction_files: Vec::new(),
            })
            .with_os("linux", "6.8")
            .build();
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            api_client,
            tool_executor,
            permission_policy,
            system_prompt,
        );

        let summary = runtime
            .run_turn("what is 2 + 2?", Some(&mut PromptAllowOnce), ())
            .await
            .expect("conversation loop should succeed");

        assert_eq!(summary.iterations, 2);
        assert_eq!(summary.assistant_messages.len(), 2);
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(summary.prompt_cache_events.len(), 1);
        assert_eq!(runtime.session().messages.len(), 4);
        assert_eq!(summary.usage.output_tokens, 10);
        assert_eq!(summary.auto_compaction, None);
        assert!(matches!(
            runtime.session().messages[1].blocks[1],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            runtime.session().messages[2].blocks[0],
            ContentBlock::ToolResult {
                is_error: false,
                ..
            }
        ));
    }

    struct DeferredApiClient {
        call_count: usize,
    }

    #[::async_trait::async_trait]
    impl ApiClient for DeferredApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "research-1".to_string(),
                        name: "deep_research_start".to_string(),
                        input: r#"{"question":"why"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ]),
                2 => {
                    assert!(matches!(
                        request.messages.last().and_then(|message| message.blocks.first()),
                        Some(ContentBlock::ToolResult { tool_use_id, output, .. })
                            if tool_use_id == "research-1" && output == "evidence ready"
                    ));
                    Ok(vec![
                        AssistantEvent::TextDelta("Verified answer".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra deferred API call"),
            }
        }
    }

    struct DeferredExecutor;

    impl ToolExecutor for DeferredExecutor {
        fn execute(&mut self, tool_name: &str, _input: &str) -> Result<String, ToolError> {
            Err(ToolError::new(format!(
                "unexpected immediate tool: {tool_name}"
            )))
        }

        fn tool_contract(&self, tool_name: &str) -> Option<RuntimeToolContract> {
            (tool_name == "deep_research_start").then(|| {
                let mut contract = RuntimeToolContract::test_read_only(tool_name);
                contract.supports_deferred = true;
                contract
            })
        }

        fn execute_outcome(&mut self, tool_name: &str, _input: &str) -> ToolExecutionOutcome {
            assert_eq!(tool_name, "deep_research_start");
            ToolExecutionOutcome::deferred(r#"{"engine":"pm"}"#)
        }
    }

    #[tokio::test]
    async fn deferred_tool_suspends_and_resumes_the_same_runtime_turn() {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            DeferredApiClient { call_count: 0 },
            DeferredExecutor,
            PermissionPolicy::new(PermissionMode::Allow),
            vec!["system".to_string()],
        );

        let suspended = runtime
            .run_turn_resumable("research this", None, ())
            .await
            .expect("deferred turn should suspend");
        let ResumableTurnOutcome::Suspended(suspended) = suspended else {
            panic!("turn should be suspended");
        };
        assert_eq!(suspended.deferred_tools.len(), 1);
        assert_eq!(suspended.deferred_tools[0].tool_use_id, "research-1");
        assert_eq!(runtime.session().turns.len(), 1);

        let completed = runtime
            .resume_turn_with_tool_results(
                vec![DeferredToolResult {
                    tool_use_id: "research-1".to_string(),
                    output: "evidence ready".to_string(),
                    is_error: false,
                }],
                None,
                (),
            )
            .await
            .expect("deferred turn should resume");
        let ResumableTurnOutcome::Completed(summary) = completed else {
            panic!("resumed turn should complete");
        };
        assert_eq!(summary.assistant_messages.len(), 1);
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(runtime.session().turns.len(), 1);
        assert_eq!(
            runtime.session().turns[0].status,
            crate::session::SessionTurnStatus::Completed
        );
    }

    #[tokio::test]
    async fn terminal_deferred_tool_finishes_without_another_model_call() {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            DeferredApiClient { call_count: 0 },
            DeferredExecutor,
            PermissionPolicy::new(PermissionMode::Allow),
            vec!["system".to_string()],
        );
        let outcome = runtime
            .run_turn_resumable("research this", None, ())
            .await
            .expect("deferred turn should suspend");
        assert!(matches!(outcome, ResumableTurnOutcome::Suspended(_)));

        let summary = runtime
            .finalize_suspended_turn_with_tool_results(
                vec![DeferredToolResult {
                    tool_use_id: "research-1".to_string(),
                    output: r#"{"accepted":true}"#.to_string(),
                    is_error: false,
                }],
                "Accepted final answer",
            )
            .await
            .expect("terminal deferred result should commit");

        assert_eq!(summary.iterations, 0);
        assert_eq!(summary.tool_results.len(), 1);
        assert_eq!(summary.assistant_messages.len(), 1);
        assert_eq!(runtime.session().messages.len(), 4);
        assert_eq!(
            runtime.session().turns[0].status,
            crate::session::SessionTurnStatus::Completed
        );
        assert!(matches!(
            runtime.session().messages[2].blocks[0],
            ContentBlock::ToolResult { .. }
        ));
        assert!(matches!(
            runtime.session().messages[3].blocks[0],
            ContentBlock::Text { ref text } if text == "Accepted final answer"
        ));
    }

    #[tokio::test]
    async fn rollback_turn_by_id_only_removes_the_exact_latest_committed_turn() {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            DeferredApiClient { call_count: 0 },
            DeferredExecutor,
            PermissionPolicy::new(PermissionMode::Allow),
            vec!["system".to_string()],
        );
        let outcome = runtime
            .run_turn_resumable("research this", None, ())
            .await
            .expect("deferred turn should suspend");
        let ResumableTurnOutcome::Suspended(suspended) = outcome else {
            panic!("turn should suspend");
        };
        let runtime_turn_id = suspended.turn_id.clone();
        runtime
            .finalize_suspended_turn_with_tool_results(
                vec![DeferredToolResult {
                    tool_use_id: "research-1".to_string(),
                    output: r#"{"accepted":true}"#.to_string(),
                    is_error: false,
                }],
                "Accepted final answer",
            )
            .await
            .expect("terminal deferred result should commit");
        let message_count = runtime.session().messages.len();

        assert!(!runtime
            .rollback_turn_by_id("an-older-or-unrelated-turn")
            .expect("wrong turn id should be a no-op"));
        assert_eq!(runtime.session().messages.len(), message_count);
        assert_eq!(
            runtime.session().turns[0].status,
            crate::session::SessionTurnStatus::Completed
        );

        assert!(runtime
            .rollback_turn_by_id(&runtime_turn_id)
            .expect("exact latest turn should roll back"));
        assert!(runtime.session().messages.is_empty());
        assert_eq!(
            runtime.session().turns[0].status,
            crate::session::SessionTurnStatus::RolledBack
        );
    }

    struct MultiDeferredApiClient {
        call_count: usize,
    }

    #[::async_trait::async_trait]
    impl ApiClient for MultiDeferredApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "deferred-a".to_string(),
                        name: "deep_research_start".to_string(),
                        input: r#"{"question":"market"}"#.to_string(),
                    },
                    AssistantEvent::ToolUse {
                        id: "deferred-b".to_string(),
                        name: "nl2sql_analyze".to_string(),
                        input: r#"{"question":"revenue"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ]),
                2 => {
                    let results = request
                        .messages
                        .iter()
                        .filter_map(message_tool_result)
                        .map(|(id, output, _)| (id.to_string(), output.to_string()))
                        .collect::<Vec<_>>();
                    assert_eq!(
                        results,
                        vec![
                            (
                                "deep_research_start".to_string(),
                                "research ready".to_string()
                            ),
                            ("nl2sql_analyze".to_string(), "sql ready".to_string()),
                        ]
                    );
                    Ok(vec![
                        AssistantEvent::TextDelta("Combined answer".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra multi-deferred API call"),
            }
        }
    }

    struct MultiDeferredExecutor;

    impl ToolExecutor for MultiDeferredExecutor {
        fn execute(&mut self, tool_name: &str, _input: &str) -> Result<String, ToolError> {
            Err(ToolError::new(format!(
                "unexpected immediate tool: {tool_name}"
            )))
        }

        fn tool_contract(&self, tool_name: &str) -> Option<RuntimeToolContract> {
            matches!(tool_name, "deep_research_start" | "nl2sql_analyze").then(|| {
                let mut contract = RuntimeToolContract::test_read_only(tool_name);
                contract.supports_deferred = true;
                contract
            })
        }

        fn execute_outcome(&mut self, tool_name: &str, _input: &str) -> ToolExecutionOutcome {
            assert!(matches!(
                tool_name,
                "deep_research_start" | "nl2sql_analyze"
            ));
            ToolExecutionOutcome::deferred(format!(r#"{{"engine":"{tool_name}"}}"#))
        }
    }

    #[tokio::test]
    async fn multiple_deferred_results_resume_in_original_model_order() {
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            MultiDeferredApiClient { call_count: 0 },
            MultiDeferredExecutor,
            PermissionPolicy::new(PermissionMode::Allow),
            vec!["system".to_string()],
        );
        let suspended = runtime
            .run_turn_resumable("combine research and data", None, ())
            .await
            .expect("multi-deferred turn should suspend");
        let ResumableTurnOutcome::Suspended(suspended) = suspended else {
            panic!("turn should suspend");
        };
        assert_eq!(
            suspended
                .deferred_tools
                .iter()
                .map(|tool| tool.tool_use_id.as_str())
                .collect::<Vec<_>>(),
            vec!["deferred-a", "deferred-b"]
        );

        let completed = runtime
            .resume_turn_with_tool_results(
                vec![
                    DeferredToolResult {
                        tool_use_id: "deferred-b".to_string(),
                        output: "sql ready".to_string(),
                        is_error: false,
                    },
                    DeferredToolResult {
                        tool_use_id: "deferred-a".to_string(),
                        output: "research ready".to_string(),
                        is_error: false,
                    },
                ],
                None,
                (),
            )
            .await
            .expect("multi-deferred turn should resume");
        assert!(matches!(completed, ResumableTurnOutcome::Completed(_)));
    }

    struct MultiToolApiClient {
        call_count: usize,
    }

    #[::async_trait::async_trait]
    impl ApiClient for MultiToolApiClient {
        async fn stream(
            &mut self,
            request: ApiRequest,
        ) -> Result<Vec<AssistantEvent>, RuntimeError> {
            self.call_count += 1;
            match self.call_count {
                1 => {
                    assert!(request
                        .messages
                        .iter()
                        .any(|message| message.role == MessageRole::User));
                    Ok(vec![
                        AssistantEvent::TextDelta("I will read these in parallel.".to_string()),
                        AssistantEvent::ToolUse {
                            id: "tool-a".to_string(),
                            name: "safe_read".to_string(),
                            input: r#"{"path":"a.sql"}"#.to_string(),
                        },
                        AssistantEvent::ToolUse {
                            id: "tool-b".to_string(),
                            name: "safe_search".to_string(),
                            input: r#"{"query":"roi"}"#.to_string(),
                        },
                        AssistantEvent::ToolUse {
                            id: "tool-c".to_string(),
                            name: "safe_memory".to_string(),
                            input: r#"{"query":"previous sql"}"#.to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ])
                }
                2 => {
                    assert_eq!(
                        request
                            .messages
                            .iter()
                            .filter(|message| message.role == MessageRole::Tool)
                            .count(),
                        3
                    );
                    Ok(vec![
                        AssistantEvent::TextDelta("Done.".to_string()),
                        AssistantEvent::MessageStop,
                    ])
                }
                _ => unreachable!("extra API call"),
            }
        }
    }

    struct BatchRecordingToolExecutor {
        batches: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl ToolExecutor for BatchRecordingToolExecutor {
        fn execute(&mut self, tool_name: &str, input: &str) -> Result<String, ToolError> {
            Ok(format!("{tool_name}:{input}"))
        }

        fn tool_contract(&self, tool_name: &str) -> Option<RuntimeToolContract> {
            matches!(tool_name, "safe_read" | "safe_search" | "safe_memory").then(|| {
                let mut contract = RuntimeToolContract::test_read_only(tool_name);
                contract.can_parallel = true;
                contract
            })
        }

        fn supports_parallel_tool_calls(&self, tool_name: &str) -> bool {
            matches!(tool_name, "safe_read" | "safe_search" | "safe_memory")
        }

        fn execute_batch(
            &mut self,
            requests: Vec<ToolExecutionRequest>,
        ) -> Vec<Result<String, ToolError>> {
            self.batches.lock().expect("batch lock").push(
                requests
                    .iter()
                    .map(|request| request.tool_name.clone())
                    .collect(),
            );
            requests
                .into_iter()
                .map(|request| Ok(format!("{}:{}", request.tool_name, request.input)))
                .collect()
        }
    }

    #[tokio::test]
    async fn parallel_safe_tool_calls_are_batched_and_results_keep_model_order() {
        let batches = Arc::new(Mutex::new(Vec::new()));
        let tool_executor = BatchRecordingToolExecutor {
            batches: Arc::clone(&batches),
        };
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            MultiToolApiClient { call_count: 0 },
            tool_executor,
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("compare previous SQL, files and memory", None, ())
            .await
            .expect("parallel tool loop succeeds");

        assert_eq!(
            batches.lock().expect("batch lock").as_slice(),
            &[vec![
                "safe_read".to_string(),
                "safe_search".to_string(),
                "safe_memory".to_string()
            ]]
        );
        let outputs = summary
            .tool_results
            .iter()
            .map(|message| {
                message_tool_result(message)
                    .expect("tool result")
                    .1
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            outputs,
            vec![
                r#"safe_read:{"path":"a.sql"}"#,
                r#"safe_search:{"query":"roi"}"#,
                r#"safe_memory:{"query":"previous sql"}"#,
            ]
        );
    }

    #[tokio::test]
    async fn records_runtime_session_trace_events() {
        let sink = Arc::new(MemoryTelemetrySink::default());
        let tracer = SessionTracer::new("session-runtime", sink.clone());
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            ScriptedApiClient { call_count: 0 },
            StaticToolExecutor::new().register("add", |_input| Ok("4".to_string())),
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec!["system".to_string()],
        )
        .with_session_tracer(tracer);

        runtime
            .run_turn("what is 2 + 2?", Some(&mut PromptAllowOnce), ())
            .await
            .expect("conversation loop should succeed");

        let events = sink.events();
        let trace_names = events
            .iter()
            .filter_map(|event| match event {
                TelemetryEvent::SessionTrace(trace) => Some(trace.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(trace_names.contains(&"turn_started"));
        assert!(trace_names.contains(&"assistant_iteration_completed"));
        assert!(trace_names.contains(&"tool_execution_started"));
        assert!(trace_names.contains(&"tool_execution_finished"));
        assert!(trace_names.contains(&"turn_completed"));
    }

    #[tokio::test]
    async fn records_denied_tool_results_when_prompt_rejects() {
        struct RejectPrompter;
        impl PermissionPrompter for RejectPrompter {
            fn decide(&mut self, _request: &PermissionRequest) -> PermissionPromptDecision {
                PermissionPromptDecision::Deny {
                    reason: "not now".to_string(),
                }
            }
        }

        struct SingleCallApiClient;
        #[::async_trait::async_trait]
        impl ApiClient for SingleCallApiClient {
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool)
                {
                    return Ok(vec![
                        AssistantEvent::TextDelta("I could not use the tool.".to_string()),
                        AssistantEvent::MessageStop,
                    ]);
                }
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "blocked".to_string(),
                        input: "secret".to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            SingleCallApiClient,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::WorkspaceWrite),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("use the tool", Some(&mut RejectPrompter), ())
            .await
            .expect("conversation should continue after denied tool");

        assert_eq!(summary.tool_results.len(), 1);
        assert!(matches!(
            &summary.tool_results[0].blocks[0],
            ContentBlock::ToolResult { is_error: true, output, .. } if output == "not now"
        ));
    }

    #[tokio::test]
    async fn denies_tool_use_when_pre_tool_hook_blocks() {
        struct SingleCallApiClient;
        #[::async_trait::async_trait]
        impl ApiClient for SingleCallApiClient {
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool)
                {
                    return Ok(vec![
                        AssistantEvent::TextDelta("blocked".to_string()),
                        AssistantEvent::MessageStop,
                    ]);
                }
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "blocked".to_string(),
                        input: r#"{"path":"secret.txt"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            SingleCallApiClient,
            StaticToolExecutor::new().register("blocked", |_input| {
                panic!("tool should not execute when hook denies")
            }),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                vec![shell_snippet("printf 'blocked by hook'; exit 2")],
                Vec::new(),
                Vec::new(),
            )),
        );

        let summary = runtime
            .run_turn("use the tool", None, ())
            .await
            .expect("conversation should continue after hook denial");

        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            *is_error,
            "hook denial should produce an error result: {output}"
        );
        assert!(
            output.contains("denied tool") || output.contains("blocked by hook"),
            "unexpected hook denial output: {output:?}"
        );
    }

    #[tokio::test]
    async fn denies_tool_use_when_pre_tool_hook_fails() {
        struct SingleCallApiClient;
        #[::async_trait::async_trait]
        impl ApiClient for SingleCallApiClient {
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                if request
                    .messages
                    .iter()
                    .any(|message| message.role == MessageRole::Tool)
                {
                    return Ok(vec![
                        AssistantEvent::TextDelta("failed".to_string()),
                        AssistantEvent::MessageStop,
                    ]);
                }
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "blocked".to_string(),
                        input: r#"{"path":"secret.txt"}"#.to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        // given
        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            SingleCallApiClient,
            StaticToolExecutor::new().register("blocked", |_input| {
                panic!("tool should not execute when hook fails")
            }),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                vec![shell_snippet("printf 'broken hook'; exit 1")],
                Vec::new(),
                Vec::new(),
            )),
        );

        // when
        let summary = runtime
            .run_turn("use the tool", None, ())
            .await
            .expect("conversation should continue after hook failure");

        // then
        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            *is_error,
            "hook failure should produce an error result: {output}"
        );
        assert!(
            output.contains("exited with status 1") || output.contains("broken hook"),
            "unexpected hook failure output: {output:?}"
        );
    }

    #[tokio::test]
    async fn appends_post_tool_hook_feedback_to_tool_result() {
        struct TwoCallApiClient {
            calls: usize,
        }

        #[::async_trait::async_trait]
        impl ApiClient for TwoCallApiClient {
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                match self.calls {
                    1 => Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "add".to_string(),
                            input: r#"{"lhs":2,"rhs":2}"#.to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]),
                    2 => {
                        assert!(request
                            .messages
                            .iter()
                            .any(|message| message.role == MessageRole::Tool));
                        Ok(vec![
                            AssistantEvent::TextDelta("done".to_string()),
                            AssistantEvent::MessageStop,
                        ])
                    }
                    _ => unreachable!("extra API call"),
                }
            }
        }

        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            TwoCallApiClient { calls: 0 },
            StaticToolExecutor::new().register("add", |_input| Ok("4".to_string())),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                vec![shell_snippet("printf 'pre hook ran'")],
                vec![shell_snippet("printf 'post hook ran'")],
                Vec::new(),
            )),
        );

        let summary = runtime
            .run_turn("use add", None, ())
            .await
            .expect("tool loop succeeds");

        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            !*is_error,
            "post hook should preserve non-error result: {output:?}"
        );
        assert!(
            output.contains('4'),
            "tool output missing value: {output:?}"
        );
        assert!(
            output.contains("pre hook ran"),
            "tool output missing pre hook feedback: {output:?}"
        );
        assert!(
            output.contains("post hook ran"),
            "tool output missing post hook feedback: {output:?}"
        );
    }

    #[tokio::test]
    async fn appends_post_tool_use_failure_hook_feedback_to_tool_result() {
        struct TwoCallApiClient {
            calls: usize,
        }

        #[::async_trait::async_trait]
        impl ApiClient for TwoCallApiClient {
            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                match self.calls {
                    1 => Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "fail".to_string(),
                            input: r#"{"path":"README.md"}"#.to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]),
                    2 => {
                        assert!(request
                            .messages
                            .iter()
                            .any(|message| message.role == MessageRole::Tool));
                        Ok(vec![
                            AssistantEvent::TextDelta("done".to_string()),
                            AssistantEvent::MessageStop,
                        ])
                    }
                    _ => unreachable!("extra API call"),
                }
            }
        }

        // given
        let mut runtime = ConversationRuntime::new_with_features(
            Session::new(),
            TwoCallApiClient { calls: 0 },
            StaticToolExecutor::new()
                .register("fail", |_input| Err(ToolError::new("tool exploded"))),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_hooks(RuntimeHookConfig::new(
                Vec::new(),
                vec![shell_snippet("printf 'post hook should not run'")],
                vec![shell_snippet("printf 'failure hook ran'")],
            )),
        );

        // when
        let summary = runtime
            .run_turn("use fail", None, ())
            .await
            .expect("tool loop succeeds");

        // then
        assert_eq!(summary.tool_results.len(), 1);
        let ContentBlock::ToolResult {
            is_error, output, ..
        } = &summary.tool_results[0].blocks[0]
        else {
            panic!("expected tool result block");
        };
        assert!(
            *is_error,
            "failure hook path should preserve error result: {output:?}"
        );
        assert!(
            output.contains("tool exploded"),
            "tool output missing failure reason: {output:?}"
        );
        assert!(
            output.contains("failure hook ran"),
            "tool output missing failure hook feedback: {output:?}"
        );
        assert!(
            !output.contains("post hook should not run"),
            "normal post hook should not run on tool failure: {output:?}"
        );
    }

    #[tokio::test]
    async fn reconstructs_usage_tracker_from_restored_session() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        session
            .messages
            .push(crate::session::ConversationMessage::assistant_with_usage(
                vec![ContentBlock::Text {
                    text: "earlier".to_string(),
                }],
                Some(TokenUsage {
                    input_tokens: 11,
                    output_tokens: 7,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 1,
                }),
            ));

        let runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        assert_eq!(runtime.usage().turns(), 1);
        assert_eq!(runtime.usage().cumulative_usage().total_tokens(), 21);
    }

    #[tokio::test]
    async fn compacts_session_after_turns() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );
        runtime.run_turn("a", None, ()).await.expect("turn a");
        runtime.run_turn("b", None, ()).await.expect("turn b");
        runtime.run_turn("c", None, ()).await.expect("turn c");

        let result = runtime.compact(CompactionConfig {
            preserve_recent_messages: 2,
            max_estimated_tokens: 1,
        });
        assert!(result.summary.contains("Conversation summary"));
        assert_eq!(
            result.compacted_session.messages[0].role,
            MessageRole::System
        );
        assert_eq!(
            result.compacted_session.session_id,
            runtime.session().session_id
        );
        assert!(result.compacted_session.compaction.is_some());
    }

    #[test]
    fn recoverable_context_view_compacts_old_large_tool_results_without_mutating_session() {
        let large_output = "line\n".repeat(3_000);
        let mut session = Session::new();
        session
            .push_message(ConversationMessage::tool_result(
                "old",
                "read_file",
                large_output.clone(),
                false,
            ))
            .expect("old tool result should append");
        for index in 0..4 {
            session
                .push_message(ConversationMessage::tool_result(
                    format!("recent-{index}"),
                    "read_file",
                    format!("recent exact output {index}"),
                    false,
                ))
                .expect("recent tool result should append");
        }

        let runtime = ConversationRuntime::new_with_features(
            session,
            ScriptedApiClient { call_count: 0 },
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_context_management(
                crate::config::RuntimeContextManagementConfig::recoverable_compaction(),
            ),
        );

        let (request_messages, report) = runtime.request_messages();
        let report = report.expect("large old tool result should be compacted");
        assert_eq!(report.compacted_tool_results, 1);
        assert!(report.request_tool_result_chars < report.original_tool_result_chars);

        let original = message_tool_result(&runtime.session().messages[0])
            .expect("original tool result")
            .1;
        assert_eq!(original, large_output);

        let request_old = message_tool_result(&request_messages[0])
            .expect("request tool result")
            .1;
        assert!(request_old.contains("recoverable tool result compacted"));
        let request_recent = message_tool_result(&request_messages[4])
            .expect("recent request tool result")
            .1;
        assert_eq!(request_recent, "recent exact output 3");
    }

    #[test]
    fn recoverable_context_view_compacts_duplicate_and_budget_excess_tool_results() {
        let mut session = Session::new();
        let medium_output = "medium\n".repeat(400);
        session
            .push_message(ConversationMessage::tool_result(
                "old-1",
                "read_file",
                medium_output.clone(),
                false,
            ))
            .expect("old tool result should append");
        session
            .push_message(ConversationMessage::tool_result(
                "old-2",
                "read_file",
                medium_output,
                false,
            ))
            .expect("duplicate tool result should append");
        session
            .push_message(ConversationMessage::tool_result(
                "old-3",
                "grep_search",
                "search\n".repeat(4_000),
                false,
            ))
            .expect("budget-heavy tool result should append");
        for index in 0..2 {
            session
                .push_message(ConversationMessage::tool_result(
                    format!("recent-{index}"),
                    "read_file",
                    format!("recent exact output {index}"),
                    false,
                ))
                .expect("recent tool result should append");
        }

        let runtime = ConversationRuntime::new_with_features(
            session,
            ScriptedApiClient { call_count: 0 },
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
            &RuntimeFeatureConfig::default().with_context_management(
                crate::config::RuntimeContextManagementConfig::recoverable_compaction(),
            ),
        );

        let (request_messages, report) = runtime.request_messages();
        let report = report.expect("duplicate and budget excess results should be compacted");
        assert_eq!(report.compacted_tool_results, 2);
        assert!(report.request_tool_result_chars < report.original_tool_result_chars);

        let request_old_1 = message_tool_result(&request_messages[0])
            .expect("first old request tool result")
            .1;
        assert!(!request_old_1.contains("recoverable tool result compacted"));

        let request_old_2 = message_tool_result(&request_messages[1])
            .expect("duplicate request tool result")
            .1;
        assert!(request_old_2.contains("reason=duplicate"));

        let request_old_3 = message_tool_result(&request_messages[2])
            .expect("budget request tool result")
            .1;
        assert!(request_old_3.contains("reason=request_budget"));

        let request_recent = message_tool_result(&request_messages[4])
            .expect("recent request tool result")
            .1;
        assert_eq!(request_recent, "recent exact output 1");
    }

    #[tokio::test]
    async fn persists_conversation_turn_messages_to_jsonl_session() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let path = temp_session_path("persisted-turn");
        let session = Session::new().with_persistence_path(path.clone());
        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        runtime
            .run_turn("persist this turn", None, ())
            .await
            .expect("turn should succeed");

        let restored = Session::load_from_path(&path).expect("persisted session should reload");
        fs::remove_file(&path).expect("temp session file should be removable");

        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[0].role, MessageRole::User);
        assert_eq!(restored.messages[1].role, MessageRole::Assistant);
        assert_eq!(restored.session_id, runtime.session().session_id);
    }

    #[tokio::test]
    async fn forks_runtime_session_without_mutating_original() {
        let mut session = Session::new();
        session
            .push_user_text("branch me")
            .expect("message should append");

        let runtime = ConversationRuntime::new(
            session.clone(),
            ScriptedApiClient { call_count: 0 },
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let forked = runtime.fork_session(Some("alt-path".to_string()));

        assert_eq!(forked.messages, session.messages);
        assert_ne!(forked.session_id, session.session_id);
        assert_eq!(
            forked
                .fork
                .as_ref()
                .map(|fork| (fork.parent_session_id.as_str(), fork.branch_name.as_deref())),
            Some((session.session_id.as_str(), Some("alt-path")))
        );
        assert!(runtime.session().fork.is_none());
    }

    fn temp_session_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-conversation-{label}-{nanos}.json"))
    }

    #[cfg(windows)]
    fn shell_snippet(script: &str) -> String {
        script.replace('\'', "\"")
    }

    #[cfg(not(windows))]
    fn shell_snippet(script: &str) -> String {
        script.to_string()
    }

    #[tokio::test]
    async fn auto_compacts_when_cumulative_input_threshold_is_crossed() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        let one = compaction_fixture_text("one");
        let two = compaction_fixture_text("two");
        let three = compaction_fixture_text("three");
        let four = compaction_fixture_text("four");
        session.messages = vec![
            crate::session::ConversationMessage::user_text(one),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: two }]),
            crate::session::ConversationMessage::user_text(three),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: four }]),
        ];

        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000);

        let summary = runtime
            .run_turn("trigger", None, ())
            .await
            .expect("turn should succeed");

        let auto_compaction = summary
            .auto_compaction
            .expect("auto compaction should be reported");
        assert_eq!(auto_compaction.removed_message_count, 2);
        assert_eq!(auto_compaction.archived_messages.len(), 2);
        assert!(matches!(
            &auto_compaction.archived_messages[0].blocks[0],
            ContentBlock::Text { text } if text.starts_with("one:")
        ));
        assert_eq!(runtime.session().messages[0].role, MessageRole::System);

        // Field-complete `session_compacted` payload (Req 1.5): the event carries
        // `summaryTokens` and the `retainedTail` window, not just the removed
        // count / archive.
        assert!(
            auto_compaction.summary_tokens > 0,
            "summary_tokens should be reported for the generated summary"
        );
        // The tail preserved verbatim (`retainedTailTokens` window) is the two
        // most-recent messages, preserved byte-for-byte after the synthesized
        // continuation system message at index 0.
        assert_eq!(auto_compaction.retained_tail.len(), 4);
        assert!(matches!(
            &auto_compaction.retained_tail[0].blocks[0],
            ContentBlock::Text { text } if text.starts_with("three:")
        ));
        assert!(matches!(
            &auto_compaction.retained_tail[1].blocks[0],
            ContentBlock::Text { text } if text.starts_with("four:")
        ));
        assert!(matches!(
            &auto_compaction.retained_tail[2].blocks[0],
            ContentBlock::Text { text } if text == "trigger"
        ));
        assert!(matches!(
            &auto_compaction.retained_tail[3].blocks[0],
            ContentBlock::Text { text } if text == "done"
        ));
        // The event's retained tail must match the live compacted session's tail
        // (everything after the leading continuation system message) exactly.
        assert_eq!(
            auto_compaction.retained_tail,
            runtime.session().messages[1..].to_vec(),
            "retained tail in the event must be preserved verbatim in the session"
        );
    }

    #[tokio::test]
    async fn emits_single_field_complete_session_compacted_trace_event() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        let one = compaction_fixture_text("one");
        let two = compaction_fixture_text("two");
        let three = compaction_fixture_text("three");
        let four = compaction_fixture_text("four");
        session.messages = vec![
            crate::session::ConversationMessage::user_text(one),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: two }]),
            crate::session::ConversationMessage::user_text(three),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: four }]),
        ];

        let sink = Arc::new(MemoryTelemetrySink::default());
        let tracer = SessionTracer::new("session-compacted", sink.clone());
        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000)
        .with_session_tracer(tracer);

        let summary = runtime
            .run_turn("trigger", None, ())
            .await
            .expect("turn should succeed");

        let expected = summary
            .auto_compaction
            .expect("auto compaction should be reported");

        // Exactly one `session_compacted` trace event is emitted per committed
        // compaction (Req 1.5).
        let compacted_events = sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                TelemetryEvent::SessionTrace(trace) if trace.name == "session_compacted" => {
                    Some(trace)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            compacted_events.len(),
            1,
            "exactly one session_compacted event must be emitted"
        );

        // The event is field-complete: removed_messages / summary_tokens /
        // retained_tail (+ retained_tail_tokens), reconciled with the payload.
        let attributes = &compacted_events[0].attributes;
        assert_eq!(
            attributes.get("removed_messages").and_then(Value::as_u64),
            Some(expected.removed_message_count as u64)
        );
        assert_eq!(
            attributes.get("summary_tokens").and_then(Value::as_u64),
            Some(expected.summary_tokens as u64)
        );
        assert_eq!(
            attributes.get("retained_tail").and_then(Value::as_u64),
            Some(expected.retained_tail.len() as u64)
        );
        assert!(
            attributes
                .get("retained_tail_tokens")
                .and_then(Value::as_u64)
                .is_some(),
            "retained_tail_tokens must be present in the event"
        );
    }

    #[tokio::test]
    async fn auto_compaction_persists_replacement_checkpoint() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let path = std::env::temp_dir().join(format!(
            "runtime-auto-compact-checkpoint-{}.jsonl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time should be valid")
                .as_nanos()
        ));
        let mut session = Session::new().with_persistence_path(path.clone());
        let one = compaction_fixture_text("one");
        let two = compaction_fixture_text("two");
        let three = compaction_fixture_text("three");
        let four = compaction_fixture_text("four");
        session.messages = vec![
            crate::session::ConversationMessage::user_text(one),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: two }]),
            crate::session::ConversationMessage::user_text(three),
            crate::session::ConversationMessage::assistant(vec![ContentBlock::Text { text: four }]),
        ];
        session
            .save_to_path(&path)
            .expect("initial persisted session should save");

        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000);

        runtime
            .run_turn("trigger", None, ())
            .await
            .expect("turn should succeed");
        let restored = Session::load_from_path(&path).expect("session should reload");
        std::fs::remove_file(&path).expect("temp session should be removable");

        assert_eq!(restored.messages, runtime.session().messages);
        assert_eq!(
            restored
                .compaction
                .as_ref()
                .expect("compaction")
                .replacement_messages,
            runtime.session().messages
        );
        assert!(restored
            .messages
            .iter()
            .all(|message| !message.blocks.iter().any(
                |block| matches!(block, ContentBlock::Text { text } if text.starts_with("one:"))
            )));
    }

    #[tokio::test]
    async fn skips_auto_compaction_below_threshold() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 99_999,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000);

        let summary = runtime
            .run_turn("trigger", None, ())
            .await
            .expect("turn should succeed");
        assert_eq!(summary.auto_compaction, None);
        assert_eq!(runtime.session().messages.len(), 2);
    }

    #[tokio::test]
    async fn auto_compaction_rejects_a_framed_replacement_that_grows_context() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        struct OversizedSummaryHook;
        #[::async_trait::async_trait]
        impl CompactionHook for OversizedSummaryHook {
            async fn prepare_compaction(
                &self,
                _archived_messages: &[ConversationMessage],
                _default_summary: &str,
                _trigger: &str,
            ) -> Result<PreparedCompaction, RuntimeError> {
                Ok(PreparedCompaction {
                    transaction_id: "oversized".to_string(),
                    replacement_summary: Some("oversized replacement ".repeat(4_096)),
                })
            }

            async fn commit_compaction(
                &self,
                _prepared: &PreparedCompaction,
                _result: &CompactionResult,
                _trigger: &str,
            ) -> Result<(), RuntimeError> {
                Ok(())
            }

            async fn abort_compaction(
                &self,
                _prepared: &PreparedCompaction,
                _reason: &str,
            ) -> Result<(), RuntimeError> {
                Ok(())
            }
        }

        let mut session = Session::new();
        for index in 0..6 {
            session
                .messages
                .push(ConversationMessage::user_text(compaction_fixture_text(
                    &format!("source-{index}"),
                )));
        }
        let original_message_count = session.messages.len();
        let sink = Arc::new(MemoryTelemetrySink::default());
        let tracer = SessionTracer::new("compaction-growth-guard", sink.clone());
        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000)
        .with_session_tracer(tracer)
        .with_compaction_hook(Arc::new(OversizedSummaryHook));

        let summary = runtime
            .run_turn("trigger", None, ())
            .await
            .expect("oversized compaction should degrade without aborting the turn");

        assert!(summary.auto_compaction.is_none());
        assert_eq!(runtime.session().messages.len(), original_message_count + 2);
        assert!(sink.events().into_iter().any(|event| matches!(
            event,
            TelemetryEvent::SessionTrace(trace)
                if trace.name == "in_turn_compaction_failed"
                    && trace
                        .attributes
                        .get("error")
                        .and_then(Value::as_str)
                        .is_some_and(|error| error.contains("framed compaction replacement"))
        )));
    }

    #[tokio::test]
    async fn auto_compaction_uses_tokens_since_previous_compaction() {
        struct SequencedApi {
            calls: usize,
        }
        #[::async_trait::async_trait]
        impl ApiClient for SequencedApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                let input_tokens = if self.calls == 0 { 120_000 } else { 10_000 };
                self.calls += 1;
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        for index in 0..6 {
            session
                .messages
                .push(crate::session::ConversationMessage::user_text(
                    compaction_fixture_text(&format!("old-{index}")),
                ));
        }
        let mut runtime = ConversationRuntime::new(
            session,
            SequencedApi { calls: 0 },
            StaticToolExecutor::new().register("glob_search", |_input| Ok("[]".to_string())),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_auto_compaction_input_tokens_threshold(100_000);

        let first = runtime
            .run_turn("first", None, ())
            .await
            .expect("first turn should succeed");
        assert!(first.auto_compaction.is_some());
        let second = runtime
            .run_turn("second", None, ())
            .await
            .expect("second turn should succeed");
        assert_eq!(second.auto_compaction, None);
    }

    #[tokio::test]
    async fn auto_compaction_threshold_defaults_and_parses_values() {
        assert_eq!(
            parse_auto_compaction_threshold(None),
            DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD
        );
        assert_eq!(parse_auto_compaction_threshold(Some("4321")), 4321);
        assert_eq!(
            parse_auto_compaction_threshold(Some("0")),
            DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD
        );
        assert_eq!(
            parse_auto_compaction_threshold(Some("not-a-number")),
            DEFAULT_AUTO_COMPACTION_INPUT_TOKENS_THRESHOLD
        );
    }

    #[test]
    fn auto_compaction_uses_local_context_estimate_when_provider_usage_is_missing() {
        assert!(should_auto_compact(0, 83_200, 83_200));
        assert!(should_auto_compact(83_200, 0, 83_200));
        assert!(!should_auto_compact(83_199, 83_199, 83_200));
    }

    #[tokio::test]
    async fn compaction_health_probe_blocks_turn_when_tool_executor_is_broken() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                panic!("API should not run when health probe fails");
            }
        }

        let mut session = Session::new();
        session.record_compaction("summarized earlier work", 4);
        session
            .push_user_text("previous message")
            .expect("message should append");

        let tool_executor = StaticToolExecutor::new().register("glob_search", |_input| {
            Err(ToolError::new("transport unavailable"))
        });
        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            tool_executor,
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let error = runtime
            .run_turn("trigger", None, ())
            .await
            .expect_err("health probe failure should abort the turn");
        assert!(
            error
                .to_string()
                .contains("Session health probe failed after compaction"),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("transport unavailable"),
            "expected underlying probe error: {error}"
        );
    }

    #[tokio::test]
    async fn compaction_health_probe_skips_empty_compacted_session() {
        struct SimpleApi;
        #[::async_trait::async_trait]
        impl ApiClient for SimpleApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut session = Session::new();
        session.record_compaction("fresh summary", 2);

        let tool_executor = StaticToolExecutor::new().register("glob_search", |_input| {
            Err(ToolError::new(
                "glob_search should not run for an empty compacted session",
            ))
        });
        let mut runtime = ConversationRuntime::new(
            session,
            SimpleApi,
            tool_executor,
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("trigger", None, ())
            .await
            .expect("empty compacted session should not fail health probe");
        assert_eq!(summary.auto_compaction, None);
        assert_eq!(runtime.session().messages.len(), 2);
    }

    #[tokio::test]
    async fn build_assistant_message_requires_message_stop_event() {
        // given
        let events = vec![AssistantEvent::TextDelta("hello".to_string())];

        // when
        let error = build_assistant_message(events)
            .expect_err("assistant messages should require a stop event");

        // then
        assert!(error
            .to_string()
            .contains("assistant stream ended without a message stop event"));
    }

    #[tokio::test]
    async fn build_assistant_message_requires_content() {
        // given
        let events = vec![AssistantEvent::MessageStop];

        // when
        let error =
            build_assistant_message(events).expect_err("assistant messages should require content");

        // then
        assert!(error
            .to_string()
            .contains("assistant stream produced no content"));
    }

    #[tokio::test]
    async fn static_tool_executor_rejects_unknown_tools() {
        // given
        let mut executor = StaticToolExecutor::new();

        // when
        let error = executor
            .execute("missing", "{}")
            .expect_err("unregistered tools should fail");

        // then
        assert_eq!(error.to_string(), "unknown tool: missing");
    }

    #[tokio::test]
    async fn run_turn_errors_when_max_iterations_is_exceeded() {
        struct LoopingApi;

        #[::async_trait::async_trait]
        impl ApiClient for LoopingApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::ToolUse {
                        id: "tool-1".to_string(),
                        name: "echo".to_string(),
                        input: "payload".to_string(),
                    },
                    AssistantEvent::MessageStop,
                ])
            }
        }

        // given
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            LoopingApi,
            StaticToolExecutor::new().register("echo", |input| Ok(input.to_string())),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        )
        .with_max_iterations(1);

        // when
        let error = runtime
            .run_turn("loop", None, ())
            .await
            .expect_err("conversation loop should stop after the configured limit");

        // then
        assert!(error
            .to_string()
            .contains("conversation loop exceeded the maximum number of iterations"));
    }

    #[tokio::test]
    async fn per_iteration_policy_can_force_same_turn_synthesis_after_tools() {
        struct BudgetAwareApi {
            calls: usize,
            budget_instruction_seen: bool,
        }

        #[::async_trait::async_trait]
        impl ApiClient for BudgetAwareApi {
            fn prepare_model_iteration(
                &mut self,
                _iteration: usize,
                completed_tool_names: &[String],
            ) -> Option<String> {
                completed_tool_names
                    .iter()
                    .any(|name| name == "search")
                    .then(|| "Tool budget reached; synthesize now.".to_string())
            }

            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                if self.calls == 1 {
                    return Ok(vec![
                        AssistantEvent::ToolUse {
                            id: "tool-1".to_string(),
                            name: "search".to_string(),
                            input: "query".to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]);
                }
                self.budget_instruction_seen = request
                    .system_prompt
                    .iter()
                    .any(|section| section.contains("Tool budget reached"));
                Ok(vec![
                    AssistantEvent::TextDelta("best available answer".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            BudgetAwareApi {
                calls: 0,
                budget_instruction_seen: false,
            },
            StaticToolExecutor::new()
                .register("search", |_input| Ok("retrieved evidence".to_string())),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("research", None, ())
            .await
            .expect("budget policy should synthesize instead of failing the turn");

        assert_eq!(summary.iterations, 2);
        assert!(runtime.api_client_mut().budget_instruction_seen);
        assert!(summary
            .assistant_messages
            .iter()
            .flat_map(|message| message.blocks.iter())
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "best available answer")));
    }

    #[tokio::test]
    async fn unavailable_tool_hallucination_is_rejected_before_execution() {
        struct StaleToolApi {
            calls: usize,
            search_blocked: bool,
        }

        #[::async_trait::async_trait]
        impl ApiClient for StaleToolApi {
            fn prepare_model_iteration(
                &mut self,
                _iteration: usize,
                completed_tool_names: &[String],
            ) -> Option<String> {
                if completed_tool_names.iter().any(|name| name == "search") {
                    self.search_blocked = true;
                    return Some("Search budget exhausted; finish now.".to_string());
                }
                None
            }

            fn is_tool_call_allowed(&self, tool_name: &str) -> bool {
                !(self.search_blocked && tool_name == "search")
            }

            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                match self.calls {
                    1 | 2 => Ok(vec![
                        AssistantEvent::ToolUse {
                            id: format!("tool-{}", self.calls),
                            name: "search".to_string(),
                            input: "query".to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]),
                    _ => {
                        let denied = request.messages.iter().any(|message| {
                            message.blocks.iter().any(|block| {
                                matches!(
                                    block,
                                    ContentBlock::ToolResult {
                                        tool_name,
                                        output,
                                        is_error: true,
                                        ..
                                    } if tool_name == "search" && output.contains("unavailable")
                                )
                            })
                        });
                        assert!(denied, "the stale call must be returned as a tool error");
                        Ok(vec![
                            AssistantEvent::TextDelta("bounded final answer".to_string()),
                            AssistantEvent::MessageStop,
                        ])
                    }
                }
            }
        }

        let executions = Arc::new(AtomicUsize::new(0));
        let executions_for_tool = executions.clone();
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            StaleToolApi {
                calls: 0,
                search_blocked: false,
            },
            StaticToolExecutor::new().register("search", move |_input| {
                executions_for_tool.fetch_add(1, Ordering::SeqCst);
                Ok("retrieved evidence".to_string())
            }),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("research", None, ())
            .await
            .expect("a stale tool call should not fail the answer");

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(summary.iterations, 3);
    }

    #[tokio::test]
    async fn repeated_identical_tool_failures_open_a_turn_local_circuit() {
        struct RepeatedFailureApi {
            calls: usize,
            circuit_instruction_seen: bool,
            blocked_tools: BTreeSet<String>,
        }

        #[::async_trait::async_trait]
        impl ApiClient for RepeatedFailureApi {
            fn block_tool_for_turn(&mut self, tool_name: &str) {
                self.blocked_tools.insert(tool_name.to_string());
            }

            fn is_tool_call_allowed(&self, tool_name: &str) -> bool {
                !self.blocked_tools.contains(tool_name)
            }

            async fn stream(
                &mut self,
                request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.calls += 1;
                if self.calls <= 2 {
                    return Ok(vec![
                        AssistantEvent::ToolUse {
                            id: format!("memory-{}", self.calls),
                            name: "memory_list".to_string(),
                            input: "{}".to_string(),
                        },
                        AssistantEvent::MessageStop,
                    ]);
                }
                self.circuit_instruction_seen = request.system_prompt.iter().any(|section| {
                    section.contains("disabled these tools")
                        && section.contains("memory_list")
                        && section.contains("original latest user request")
                });
                Ok(vec![
                    AssistantEvent::TextDelta("answering the original request".to_string()),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        let mut runtime = ConversationRuntime::new(
            Session::new(),
            RepeatedFailureApi {
                calls: 0,
                circuit_instruction_seen: false,
                blocked_tools: BTreeSet::new(),
            },
            StaticToolExecutor::new().register("memory_list", |_input| {
                Err(ToolError::new("near UNION: syntax error"))
            }),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        let summary = runtime
            .run_turn("用 skill 查一下昨天的 ROI", None, ())
            .await
            .expect("repeated tool failure must degrade to synthesis");

        assert_eq!(summary.iterations, 3);
        assert!(runtime.api_client_mut().circuit_instruction_seen);
        assert_eq!(
            summary
                .tool_results
                .iter()
                .filter_map(message_tool_result)
                .filter(|(_, _, is_error)| *is_error)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn run_turn_propagates_api_errors() {
        struct FailingApi;

        #[::async_trait::async_trait]
        impl ApiClient for FailingApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Err(RuntimeError::new("upstream failed"))
            }
        }

        // given
        let mut runtime = ConversationRuntime::new(
            Session::new(),
            FailingApi,
            StaticToolExecutor::new(),
            PermissionPolicy::new(PermissionMode::DangerFullAccess),
            vec!["system".to_string()],
        );

        // when
        let error = runtime
            .run_turn("hello", None, ())
            .await
            .expect_err("API failures should propagate");

        // then
        assert_eq!(error.to_string(), "upstream failed");
    }

    // Feature: codex-parity-gaps, Property 5: 压缩失败降级——记录并以未压缩上下文继续
    //
    // For any injected hook whose `before_compaction` surfaces an error during the
    // "extract → persist" stage, In_Turn_Compaction MUST degrade the failure to
    // "uncompacted" (as if `Ok(None)` had been returned), record the error to the
    // session trace, and let the current turn continue with the *uncompacted*
    // context rather than aborting the turn.
    //
    // Validates: Requirements 1.7
    mod property_compaction_failure_downgrade {
        use super::*;
        use proptest::prelude::*;

        const AUTO_COMPACT_THRESHOLD: u32 = 100_000;

        // An API stub that always reports cumulative input tokens over the
        // Auto_Compact_Threshold and never requests a tool, so the turn runs a
        // single terminal iteration that still evaluates the compaction gate.
        struct OverThresholdApi;

        #[::async_trait::async_trait]
        impl ApiClient for OverThresholdApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        // A hook whose `before_compaction` always fails with the given error,
        // exercising the "extract → persist" failure path (Req 1.7).
        struct FailingCompactionHook {
            error_message: String,
        }

        #[::async_trait::async_trait]
        impl CompactionHook for FailingCompactionHook {
            async fn prepare_compaction(
                &self,
                _archived_messages: &[ConversationMessage],
                _default_summary: &str,
                _trigger: &str,
            ) -> Result<PreparedCompaction, RuntimeError> {
                Err(RuntimeError::new(self.error_message.clone()))
            }

            async fn commit_compaction(
                &self,
                _prepared: &PreparedCompaction,
                _result: &CompactionResult,
                _trigger: &str,
            ) -> Result<(), RuntimeError> {
                Ok(())
            }

            async fn abort_compaction(
                &self,
                _prepared: &PreparedCompaction,
                _reason: &str,
            ) -> Result<(), RuntimeError> {
                Ok(())
            }
        }

        // Build a session whose message count guarantees the compaction window
        // discovery removes at least one message once the turn adds the user
        // input + assistant response (so the failing hook is actually reached).
        // Every message is plain text (no tool-use / tool-result pairs) so the
        // compaction boundary never has to walk back.
        fn seed_session(contents: &[String]) -> Session {
            let mut session = Session::new();
            session.messages = contents
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let text = compaction_fixture_text(text);
                    if index % 2 == 0 {
                        ConversationMessage::user_text(text)
                    } else {
                        ConversationMessage::assistant(vec![ContentBlock::Text { text }])
                    }
                })
                .collect();
            session
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn compaction_failure_degrades_and_records_without_compacting(
                // >= 5 seed messages keeps the total (seed + user + assistant)
                // above preserve_recent_messages (4) so removal is non-zero and
                // the failing hook is exercised on every case.
                contents in proptest::collection::vec("[a-z ]{1,24}", 5..12),
                error_message in "[A-Za-z0-9 ._-]{1,48}",
                user_input in "[a-z ]{1,24}",
            ) {
                let runtime_env = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime should build");

                runtime_env.block_on(async {
                    let seed_len = contents.len();
                    let session = seed_session(&contents);
                    let first_role = session.messages[0].role;

                    let sink = Arc::new(MemoryTelemetrySink::default());
                    let tracer = SessionTracer::new("session-degrade", sink.clone());
                    let hook: Arc<dyn CompactionHook> = Arc::new(FailingCompactionHook {
                        error_message: error_message.clone(),
                    });

                    let mut runtime = ConversationRuntime::new(
                        session,
                        OverThresholdApi,
                        StaticToolExecutor::new(),
                        PermissionPolicy::new(PermissionMode::DangerFullAccess),
                        vec!["system".to_string()],
                    )
                    .with_auto_compaction_input_tokens_threshold(AUTO_COMPACT_THRESHOLD)
                    .with_session_tracer(tracer)
                    .with_compaction_hook(hook);

                    // (1) The turn continues (is not aborted) despite the hook error.
                    let summary = runtime
                        .run_turn(user_input, None, ())
                        .await
                        .expect("compaction failure must degrade, not abort the turn");

                    // (2) The context was NOT compacted: no compaction event is reported.
                    prop_assert!(
                        summary.auto_compaction.is_none(),
                        "a failed compaction must not report an auto_compaction event"
                    );

                    // (3) The session is left uncompacted: no synthesized System
                    // continuation message is inserted at index 0 (the original
                    // leading role is preserved), and the message count equals the
                    // seed history plus the user input and assistant response only.
                    let messages = &runtime.session().messages;
                    prop_assert_eq!(
                        messages[0].role,
                        first_role,
                        "the leading message must remain uncompacted (no System continuation)"
                    );
                    prop_assert_eq!(
                        messages.len(),
                        seed_len + 2,
                        "uncompacted session must keep every message (seed + user + assistant)"
                    );

                    // (4) The failure is recorded to the session trace exactly once
                    // as an `in_turn_compaction_failed` event carrying the error and
                    // the degrade marker (Req 1.7 observability/auditability).
                    let failure_events = sink
                        .events()
                        .into_iter()
                        .filter_map(|event| match event {
                            TelemetryEvent::SessionTrace(trace)
                                if trace.name == "in_turn_compaction_failed" =>
                            {
                                Some(trace)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    prop_assert_eq!(
                        failure_events.len(),
                        1,
                        "exactly one in_turn_compaction_failed event must be recorded"
                    );

                    let attributes = &failure_events[0].attributes;
                    prop_assert_eq!(
                        attributes.get("error").and_then(Value::as_str),
                        Some(error_message.as_str()),
                        "the trace event must carry the hook error message"
                    );
                    prop_assert_eq!(
                        attributes.get("degraded").and_then(Value::as_str),
                        Some("continued_with_uncompacted_context"),
                        "the trace event must mark the degraded continuation"
                    );

                    Ok(())
                })?;
            }
        }
    }

    // Property 3 (design.md): 达阈压缩发出字段完整事件且保留尾部.
    //
    // For any session that crosses the Auto_Compact_Threshold, executing
    // In_Turn_Compaction MUST satisfy all of: the removed-message count is
    // strictly greater than zero, the retained tail (the `retainedTailTokens`
    // window) is preserved verbatim, and exactly one field-complete
    // `session_compacted` trace event is emitted (carrying `removed_messages`,
    // `summary_tokens`, and `retained_tail`).
    //
    // Validates: Requirements 1.5
    mod property_compaction_event_and_tail_retention {
        use super::*;
        use proptest::prelude::*;

        const AUTO_COMPACT_THRESHOLD: u32 = 100_000;

        // An API stub that always reports cumulative input tokens over the
        // Auto_Compact_Threshold and never requests a tool, so the turn runs a
        // single terminal iteration that crosses the compaction gate exactly
        // once.
        struct OverThresholdApi;

        #[::async_trait::async_trait]
        impl ApiClient for OverThresholdApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                Ok(vec![
                    AssistantEvent::TextDelta("done".to_string()),
                    AssistantEvent::Usage(TokenUsage {
                        input_tokens: 120_000,
                        output_tokens: 4,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    }),
                    AssistantEvent::MessageStop,
                ])
            }
        }

        // Build a seed session out of plain-text messages (no tool-use /
        // tool-result pairs) so the compaction boundary never has to walk back
        // and every message above the preserved tail is discardable.
        fn seed_session(contents: &[String]) -> Session {
            let mut session = Session::new();
            session.messages = contents
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let text = compaction_fixture_text(text);
                    if index % 2 == 0 {
                        ConversationMessage::user_text(text)
                    } else {
                        ConversationMessage::assistant(vec![ContentBlock::Text { text }])
                    }
                })
                .collect();
            session
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            // Feature: codex-parity-gaps, Property 3: 达阈压缩发出字段完整事件且保留尾部
            #[test]
            fn over_threshold_compaction_removes_preserves_tail_and_emits_one_field_complete_event(
                // >= 6 seed messages keeps the total (seed + user + assistant)
                // well above preserve_recent_messages (4), so the compaction
                // window discovery always removes at least one message.
                contents in proptest::collection::vec("[a-z ]{1,24}", 6..16),
                user_input in "[a-z ]{1,24}",
            ) {
                let runtime_env = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime should build");

                runtime_env.block_on(async {
                    let session = seed_session(&contents);

                    let sink = Arc::new(MemoryTelemetrySink::default());
                    let tracer = SessionTracer::new("session-compacted-prop", sink.clone());

                    let mut runtime = ConversationRuntime::new(
                        session,
                        OverThresholdApi,
                        StaticToolExecutor::new(),
                        PermissionPolicy::new(PermissionMode::DangerFullAccess),
                        vec!["system".to_string()],
                    )
                    .with_auto_compaction_input_tokens_threshold(AUTO_COMPACT_THRESHOLD)
                    .with_session_tracer(tracer);

                    let summary = runtime
                        .run_turn(user_input, None, ())
                        .await
                        .expect("over-threshold turn should succeed");

                    // The over-threshold turn must report a committed compaction.
                    let event = summary
                        .auto_compaction
                        .expect("over-threshold turn must report a compaction");

                    // (1) The removed-message count is strictly positive.
                    prop_assert!(
                        event.removed_message_count > 0,
                        "compaction must remove at least one message"
                    );

                    // (2) The retained tail is preserved verbatim: it equals the
                    // live compacted session's messages after the synthesized
                    // continuation system message at index 0 (byte-for-byte).
                    prop_assert_eq!(
                        event.retained_tail.clone(),
                        runtime.session().messages[1..].to_vec(),
                        "retained tail must be preserved verbatim in the session"
                    );

                    // (3) Exactly one `session_compacted` trace event is emitted
                    // for the single committed compaction.
                    let compacted_events = sink
                        .events()
                        .into_iter()
                        .filter_map(|telemetry_event| match telemetry_event {
                            TelemetryEvent::SessionTrace(trace)
                                if trace.name == "session_compacted" =>
                            {
                                Some(trace)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    prop_assert_eq!(
                        compacted_events.len(),
                        1,
                        "exactly one session_compacted event must be emitted"
                    );

                    // (4) That event is field-complete: removed_messages /
                    // summary_tokens / retained_tail (+ retained_tail_tokens),
                    // reconciled with the reported payload.
                    let attributes = &compacted_events[0].attributes;
                    prop_assert_eq!(
                        attributes.get("removed_messages").and_then(Value::as_u64),
                        Some(event.removed_message_count as u64),
                        "removed_messages must match the payload"
                    );
                    prop_assert_eq!(
                        attributes.get("summary_tokens").and_then(Value::as_u64),
                        Some(event.summary_tokens as u64),
                        "summary_tokens must match the payload"
                    );
                    prop_assert_eq!(
                        attributes.get("retained_tail").and_then(Value::as_u64),
                        Some(event.retained_tail.len() as u64),
                        "retained_tail count must match the payload"
                    );
                    prop_assert!(
                        attributes
                            .get("retained_tail_tokens")
                            .and_then(Value::as_u64)
                            .is_some(),
                        "retained_tail_tokens must be present in the event"
                    );

                    Ok(())
                })?;
            }
        }
    }

    // Feature: codex-parity-gaps, Property 4: 单 turn 内可多次压缩且每次后继续
    //
    // For any long-horizon turn whose consecutive reasoning-loop iterations keep
    // cumulative input tokens over the Auto_Compact_Threshold, the runtime MUST
    // be able to compact more than once in that same turn, and each compaction
    // must resume the turn rather than terminating it.
    //
    // Validates: Requirements 1.6
    mod property_multiple_compactions_per_turn {
        use super::*;
        use proptest::prelude::*;

        const AUTO_COMPACT_THRESHOLD: u32 = 100_000;

        struct MultiStepOverThresholdApi {
            input_tokens: u32,
            call_count: usize,
        }

        #[::async_trait::async_trait]
        impl ApiClient for MultiStepOverThresholdApi {
            async fn stream(
                &mut self,
                _request: ApiRequest,
            ) -> Result<Vec<AssistantEvent>, RuntimeError> {
                self.call_count = self.call_count.saturating_add(1);
                let usage = AssistantEvent::Usage(TokenUsage {
                    input_tokens: self.input_tokens,
                    output_tokens: 4,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                });
                if self.call_count <= 2 {
                    let mut events = (0..8)
                        .map(|tool_index| AssistantEvent::ToolUse {
                            id: format!("tool-{}-{tool_index}", self.call_count),
                            name: "echo".to_string(),
                            input: format!("payload-{}-{tool_index}", self.call_count),
                        })
                        .collect::<Vec<_>>();
                    events.extend([usage, AssistantEvent::MessageStop]);
                    Ok(events)
                } else {
                    Ok(vec![
                        AssistantEvent::TextDelta("done".to_string()),
                        usage,
                        AssistantEvent::MessageStop,
                    ])
                }
            }
        }

        fn seed_session(contents: &[String]) -> Session {
            let mut session = Session::new();
            session.messages = contents
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    // Keep the retained tail materially larger than the merged
                    // summary so the second compaction also passes the growth
                    // guard instead of being correctly rejected as lossy.
                    let text = format!("{text}: {}", "important context ".repeat(256));
                    if index % 2 == 0 {
                        ConversationMessage::user_text(text)
                    } else {
                        ConversationMessage::assistant(vec![ContentBlock::Text { text }])
                    }
                })
                .collect();
            session
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn repeated_over_threshold_iterations_compact_more_than_once_and_continue(
                // Keep a sufficiently large source window after each compaction
                // so the property can exercise two distinct in-turn passes.
                contents in proptest::collection::vec("[a-z ]{1,24}", 12..24),
                extra in 0u32..=100_000,
                user_input in "[a-z ]{1,24}",
            ) {
                let runtime_env = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime should build");

                runtime_env.block_on(async {
                    let sink = Arc::new(MemoryTelemetrySink::default());
                    let tracer = SessionTracer::new("session-multi-compact-prop", sink.clone());
                    let mut runtime = ConversationRuntime::new(
                        seed_session(&contents),
                        MultiStepOverThresholdApi {
                            input_tokens: AUTO_COMPACT_THRESHOLD.saturating_add(extra),
                            call_count: 0,
                        },
                        StaticToolExecutor::new().register("echo", |input| Ok(input.to_string())),
                        PermissionPolicy::new(PermissionMode::DangerFullAccess),
                        vec!["system".to_string()],
                    )
                    .with_auto_compaction_input_tokens_threshold(AUTO_COMPACT_THRESHOLD)
                    .with_session_tracer(tracer);

                    let summary = runtime
                        .run_turn(user_input, None, ())
                        .await
                        .expect("each in-turn compaction must continue the turn");
                    prop_assert_eq!(
                        summary.iterations,
                        3,
                        "the tool loop must continue through two tool iterations and finish naturally"
                    );
                    prop_assert_eq!(
                        summary.tool_results.len(),
                        16,
                        "tool calls after compaction must still execute"
                    );

                    let compacted_events = sink
                        .events()
                        .into_iter()
                        .filter(|event| matches!(
                            event,
                            TelemetryEvent::SessionTrace(trace)
                                if trace.name == "session_compacted"
                        ))
                        .count();
                    prop_assert!(
                        compacted_events >= 2,
                        "expected at least two in-turn compactions in one turn, got {compacted_events}"
                    );

                    Ok(())
                })?;
            }
        }
    }
}
