//! Durable execution-kernel boundary for the conversation runtime.
//!
//! The runtime owns ordering, while storage adapters own durability, fencing,
//! capability persistence, artifact retention and projection.  Keeping this as
//! a trait avoids coupling the generic runtime to SQLx or the web server.

use crate::permissions::PermissionRequest;
use crate::session::{ConversationMessage, Session};
use crate::RuntimeError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTurnStart {
    pub turn_id: String,
    pub user_input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContextManifestInput {
    pub turn_id: String,
    pub iteration: usize,
    pub budget_stage: RuntimeModelBudgetStage,
    pub system_sections: Vec<String>,
    pub messages: Vec<ConversationMessage>,
    pub estimated_tokens: usize,
    /// Exact hard input limit used by the Context Compiler for the provider
    /// request represented by this manifest.
    pub max_input_tokens: u64,
    pub model_version: Option<String>,
    pub active_tools: Vec<String>,
    /// Exact packet whose selected blocks produced `system_sections` and
    /// `messages`. This is captured before provider dispatch so replay never
    /// has to reconstruct selection from a shadow manifest.
    pub context_packet: semantic_core::ContextPacket,
    /// Evaluated prompt variant which actually contributed the stable/domain
    /// sections in this provider request.
    pub prompt_manifest: Option<agent_protocol::PromptManifest>,
    /// Version of the semantic snapshot used to compile this request. The
    /// snapshot body stays in the semantic-state store; the manifest carries
    /// the immutable reference so replay can load the exact state.
    pub semantic_snapshot_version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContextSupplementRequest {
    pub turn_id: String,
    pub iteration: usize,
    pub objective: String,
    pub domain: String,
    pub max_input_tokens: u64,
    pub model_version: Option<String>,
    pub active_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeContextSupplement {
    pub envelope: semantic_core::ContextEnvelope,
    pub blocks: Vec<semantic_core::ContextBlock>,
    pub semantic_snapshot_version: Option<u64>,
}

/// Budget class for one provider request. General exploration is isolated from
/// protected close-out stages so tool loops cannot starve verification or the
/// final user-visible answer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeModelBudgetStage {
    #[default]
    General,
    FinalSynthesis,
    DomainVerifier,
    UserVisibleError,
}

impl RuntimeModelBudgetStage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::FinalSynthesis => "final_synthesis",
            Self::DomainVerifier => "domain_verifier",
            Self::UserVisibleError => "user_visible_error",
        }
    }

    #[must_use]
    pub fn is_protected(self) -> bool {
        !matches!(self, Self::General)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeToolIntent {
    pub turn_id: String,
    pub invocation_id: String,
    pub tool_name: String,
    pub input: String,
    pub iteration: usize,
    pub authorized: bool,
    pub denial_reason: Option<String>,
    pub idempotency_key: String,
    pub contract: RuntimeToolContract,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeToolSideEffectClass {
    None,
    LocalRead,
    ExternalRead,
    LocalWrite,
    ExternalWrite,
    Irreversible,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeToolRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeToolRetryPolicy {
    Never,
    SafeTransportOnly,
    Idempotent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeToolCancellationContract {
    Cooperative,
    Immediate,
    Unsupported,
}

/// Runtime-enforced tool lifecycle contract. Tool descriptions remain model
/// metadata; this contract is server authority and is hashed into durable
/// intent before any executor can observe the request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToolContract {
    pub tool_name: String,
    pub contract_version: String,
    pub input_schema_version: String,
    pub output_schema_version: String,
    pub side_effect_class: RuntimeToolSideEffectClass,
    pub risk_level: RuntimeToolRiskLevel,
    pub required_capability: String,
    pub tenant_policy: String,
    pub secret_scope: String,
    pub idempotency_strategy: String,
    pub retry_policy: RuntimeToolRetryPolicy,
    pub timeout_ms: u64,
    pub deadline_ms: u64,
    pub cancellation: RuntimeToolCancellationContract,
    pub network_policy: String,
    pub filesystem_policy: String,
    pub datasource_policy: String,
    pub artifact_policy: String,
    pub evidence_policy: String,
    pub compensation: String,
    pub can_parallel: bool,
    pub supports_deferred: bool,
    pub continue_after_parent_cancel: bool,
}

impl RuntimeToolContract {
    #[must_use]
    pub fn test_read_only(tool_name: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            contract_version: "tool-contract-v1".into(),
            input_schema_version: "v1".into(),
            output_schema_version: "v1".into(),
            side_effect_class: RuntimeToolSideEffectClass::LocalRead,
            risk_level: RuntimeToolRiskLevel::Low,
            required_capability: "execute".into(),
            tenant_policy: "current_tenant".into(),
            secret_scope: "none".into(),
            idempotency_strategy: "read_only".into(),
            retry_policy: RuntimeToolRetryPolicy::SafeTransportOnly,
            timeout_ms: 30_000,
            deadline_ms: 60_000,
            cancellation: RuntimeToolCancellationContract::Cooperative,
            network_policy: "deny".into(),
            filesystem_policy: "read_only".into(),
            datasource_policy: "deny".into(),
            artifact_policy: "spill_if_oversized".into(),
            evidence_policy: "eligible_with_provenance".into(),
            compensation: "none_required".into(),
            can_parallel: false,
            supports_deferred: false,
            continue_after_parent_cancel: false,
        }
    }

    pub fn validate(&self, tool_name: &str) -> Result<(), RuntimeError> {
        if self.tool_name != tool_name
            || self.contract_version.trim().is_empty()
            || self.input_schema_version.trim().is_empty()
            || self.output_schema_version.trim().is_empty()
            || self.required_capability.trim().is_empty()
            || self.idempotency_strategy.trim().is_empty()
            || self.timeout_ms == 0
            || self.deadline_ms < self.timeout_ms
        {
            return Err(RuntimeError::new(format!(
                "tool `{tool_name}` has an incomplete or mismatched runtime contract"
            )));
        }
        if matches!(
            self.side_effect_class,
            RuntimeToolSideEffectClass::ExternalWrite | RuntimeToolSideEffectClass::Irreversible
        ) && self.idempotency_strategy == "none"
        {
            return Err(RuntimeError::new(format!(
                "tool `{tool_name}` cannot perform high-risk side effects without idempotency"
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn content_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("tool contract is serializable");
        hex::encode(Sha256::digest(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeApprovalRequest {
    pub turn_id: String,
    pub invocation_id: String,
    pub tool_name: String,
    pub input: String,
    pub iteration: usize,
    pub request: PermissionRequest,
    pub contract: RuntimeToolContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeApprovalDecision {
    Approved,
    Denied,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeApprovalResolution {
    pub turn_id: String,
    pub invocation_id: String,
    pub decision: RuntimeApprovalDecision,
    pub reason: Option<String>,
}

impl RuntimeToolIntent {
    #[must_use]
    pub fn new(
        turn_id: &str,
        invocation_id: &str,
        tool_name: &str,
        input: &str,
        iteration: usize,
        authorized: bool,
        denial_reason: Option<String>,
    ) -> Self {
        Self::new_with_contract(
            turn_id,
            invocation_id,
            tool_name,
            input,
            iteration,
            authorized,
            denial_reason,
            RuntimeToolContract::test_read_only(tool_name),
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_contract(
        turn_id: &str,
        invocation_id: &str,
        tool_name: &str,
        input: &str,
        iteration: usize,
        authorized: bool,
        denial_reason: Option<String>,
        contract: RuntimeToolContract,
    ) -> Self {
        let input_hash = hex::encode(Sha256::digest(input.as_bytes()));
        let contract_hash = contract.content_hash();
        Self {
            turn_id: turn_id.to_string(),
            invocation_id: invocation_id.to_string(),
            tool_name: tool_name.to_string(),
            input: input.to_string(),
            iteration,
            authorized,
            denial_reason,
            idempotency_key: format!("tool:{turn_id}:{invocation_id}:{input_hash}:{contract_hash}"),
            contract,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeToolOutcomeKind {
    Completed,
    Failed,
    Cancelled,
    Expired,
    OutcomeUnknown,
    Deferred,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeToolOutcome {
    pub turn_id: String,
    pub invocation_id: String,
    pub tool_name: String,
    pub input: String,
    pub output: String,
    pub iteration: usize,
    pub outcome: RuntimeToolOutcomeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeToolProjection {
    pub model_output: String,
    pub artifact_id: Option<String>,
    pub content_hash: String,
    pub omitted_bytes: u64,
}

/// Logical payload class used by the spill-plane reducers. The tool protocol
/// still transports UTF-8 today, but keeping the class explicit prevents logs,
/// result tables and structured JSON from silently falling back to one generic
/// character truncation policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeArtifactKind {
    Text,
    Log,
    SearchResults,
    Table,
    Json,
    Binary,
}

impl RuntimeArtifactKind {
    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Text | Self::Log | Self::SearchResults => "text/plain; charset=utf-8",
            Self::Table | Self::Json => "application/json",
            Self::Binary => "application/octet-stream",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeArtifactPreview {
    pub kind: RuntimeArtifactKind,
    pub text: String,
    pub source_bytes: u64,
    pub retained_bytes: u64,
    pub omitted_bytes: u64,
    pub total_rows: Option<u64>,
    pub omitted_rows: Option<u64>,
    pub truncated: bool,
}

fn count_json_rows(value: &serde_json::Value) -> Option<u64> {
    if let Some(rows) = value.as_array() {
        return u64::try_from(rows.len()).ok();
    }
    for key in ["rows", "results", "data", "items"] {
        if let Some(rows) = value.get(key).and_then(serde_json::Value::as_array) {
            return u64::try_from(rows.len()).ok();
        }
    }
    None
}

fn infer_artifact_kind(tool_name: &str, output: &str) -> (RuntimeArtifactKind, Option<u64>) {
    if output.contains('\0') {
        return (RuntimeArtifactKind::Binary, None);
    }
    let lower = tool_name.to_ascii_lowercase();
    let parsed = serde_json::from_str::<serde_json::Value>(output).ok();
    let rows = parsed.as_ref().and_then(count_json_rows);
    if lower.contains("search") {
        return (RuntimeArtifactKind::SearchResults, rows);
    }
    if lower.contains("query")
        || lower.contains("sql")
        || lower.contains("datasource")
        || rows.is_some()
    {
        return (RuntimeArtifactKind::Table, rows);
    }
    if parsed.is_some() {
        return (RuntimeArtifactKind::Json, rows);
    }
    if lower.contains("shell")
        || lower.contains("exec")
        || lower.contains("command")
        || output.lines().count() > 80
    {
        return (RuntimeArtifactKind::Log, None);
    }
    (RuntimeArtifactKind::Text, None)
}

fn utf8_prefix_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &text[..boundary]
}

fn utf8_suffix_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = text.len().saturating_sub(max_bytes);
    while boundary < text.len() && !text.is_char_boundary(boundary) {
        boundary += 1;
    }
    &text[boundary..]
}

/// Build a typed, UTF-8-safe preview while retaining an exact byte accounting
/// of the protected source payload. The caller persists the full payload before
/// exposing this preview as evidence.
#[must_use]
pub fn reduce_runtime_artifact(
    tool_name: &str,
    output: &str,
    max_bytes: usize,
) -> RuntimeArtifactPreview {
    let (kind, total_rows) = infer_artifact_kind(tool_name, output);
    let source_bytes = output.len();
    let max_bytes = max_bytes.max(512);
    if source_bytes <= max_bytes {
        return RuntimeArtifactPreview {
            kind,
            text: output.to_string(),
            source_bytes: u64::try_from(source_bytes).unwrap_or(u64::MAX),
            retained_bytes: u64::try_from(source_bytes).unwrap_or(u64::MAX),
            omitted_bytes: 0,
            total_rows,
            omitted_rows: Some(0).filter(|_| total_rows.is_some()),
            truncated: false,
        };
    }

    let (head_budget, tail_budget) = match kind {
        RuntimeArtifactKind::Log => (max_bytes / 3, max_bytes.saturating_mul(2) / 3),
        RuntimeArtifactKind::SearchResults | RuntimeArtifactKind::Table => {
            (max_bytes.saturating_mul(3) / 4, max_bytes / 4)
        }
        RuntimeArtifactKind::Binary => (max_bytes.min(256), 0),
        RuntimeArtifactKind::Text | RuntimeArtifactKind::Json => (max_bytes / 2, max_bytes / 2),
    };
    let head = utf8_prefix_bytes(output, head_budget);
    let tail = if tail_budget == 0 {
        ""
    } else {
        utf8_suffix_bytes(output, tail_budget)
    };
    let retained_bytes = head.len().saturating_add(tail.len()).min(source_bytes);
    let omitted_bytes = source_bytes.saturating_sub(retained_bytes);
    let omitted_rows = total_rows.map(|rows| {
        let retained_ratio = retained_bytes as f64 / source_bytes.max(1) as f64;
        rows.saturating_sub((rows as f64 * retained_ratio).ceil() as u64)
    });
    let label = serde_json::to_string(&kind)
        .unwrap_or_else(|_| "\"text\"".to_string())
        .trim_matches('"')
        .to_string();
    let separator = format!(
        "\n\n[artifact preview: kind={label}; omitted_bytes={omitted_bytes}{}]\n\n",
        omitted_rows
            .map(|rows| format!("; omitted_rows={rows}"))
            .unwrap_or_default()
    );
    RuntimeArtifactPreview {
        kind,
        text: format!("{head}{separator}{tail}"),
        source_bytes: u64::try_from(source_bytes).unwrap_or(u64::MAX),
        retained_bytes: u64::try_from(retained_bytes).unwrap_or(u64::MAX),
        omitted_bytes: u64::try_from(omitted_bytes).unwrap_or(u64::MAX),
        total_rows,
        omitted_rows,
        truncated: true,
    }
}

impl RuntimeToolProjection {
    #[must_use]
    pub fn inline(output: String) -> Self {
        Self {
            content_hash: hex::encode(Sha256::digest(output.as_bytes())),
            model_output: output,
            artifact_id: None,
            omitted_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTurnTerminalStatus {
    Completed,
    Failed,
    Cancelled,
    Suspended,
}

#[::async_trait::async_trait]
pub trait AgentExecutionKernel: Send + Sync {
    /// Reconcile open durable items after process restart.  Implementations
    /// append synthetic closers and never rewrite committed history.
    async fn recover(&self) -> Result<(), RuntimeError>;

    async fn start_turn(&self, input: RuntimeTurnStart) -> Result<(), RuntimeError>;

    /// Load the governed semantic state that must be considered by the real
    /// provider request. Durable adapters may retrieve/rank data here, but the
    /// generic runtime remains responsible for final token-budget selection.
    async fn load_context_supplement(
        &self,
        _input: RuntimeContextSupplementRequest,
    ) -> Result<RuntimeContextSupplement, RuntimeError> {
        Ok(RuntimeContextSupplement::default())
    }

    /// Persist the exact model-visible context manifest before the provider is
    /// called.  Payload projections may redact secrets, but hashes always refer
    /// to the original runtime view.
    async fn record_context_manifest(
        &self,
        input: RuntimeContextManifestInput,
    ) -> Result<(), RuntimeError>;

    async fn record_assistant_message(
        &self,
        turn_id: &str,
        iteration: usize,
        message: &ConversationMessage,
    ) -> Result<(), RuntimeError>;

    /// Persist a user-visible message produced outside the native model turn
    /// loop (for example a background PM, attribution, or adversarial task).
    /// The message must reach the ledger before the in-memory Session changes.
    async fn record_visible_message(
        &self,
        message_id: &str,
        message: &ConversationMessage,
    ) -> Result<(), RuntimeError>;

    /// Atomically append a canonical, field-complete runtime checkpoint and
    /// its query projection. Durable adapters encrypt the exact payload;
    /// redacted event projections are never used as the recovery source.
    async fn checkpoint_session(
        &self,
        _reason: &str,
        _session: &Session,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Commit durable intent, capability consumption and budget reservation
    /// before a tool executor is allowed to observe the request.
    async fn authorize_tool(&self, intent: &RuntimeToolIntent) -> Result<(), RuntimeError>;

    /// Atomically transition an authorized intent to started immediately
    /// before dispatch. A second transition must fail, preventing duplicate
    /// side effects after retry or process recovery.
    async fn start_tool(&self, intent: &RuntimeToolIntent) -> Result<(), RuntimeError>;

    /// Persist an external approval request before the turn is exposed as
    /// suspended. Implementations must make this idempotent by invocation id.
    async fn request_approval(&self, request: &RuntimeApprovalRequest) -> Result<(), RuntimeError>;

    /// Atomically consume one pending approval decision. Duplicate or stale
    /// resolutions must fail closed and may never grant a second execution.
    async fn resolve_approval(
        &self,
        resolution: &RuntimeApprovalResolution,
    ) -> Result<RuntimeApprovalDecision, RuntimeError>;

    /// Atomically consume a previously persisted user answer. Durable stores
    /// must fence this transition so restart/retry cannot inject the answer or
    /// resume the suspended turn twice.
    async fn consume_user_question(
        &self,
        _turn_id: &str,
        _invocation_id: &str,
        answer: &str,
    ) -> Result<String, RuntimeError> {
        Ok(answer.to_string())
    }

    /// Persist the complete result as a typed artifact when needed, settle the
    /// budget and return the bounded model-visible projection.
    async fn finish_tool(
        &self,
        outcome: RuntimeToolOutcome,
    ) -> Result<RuntimeToolProjection, RuntimeError>;

    async fn finish_turn(
        &self,
        turn_id: &str,
        status: RuntimeTurnTerminalStatus,
        detail: Option<&str>,
    ) -> Result<(), RuntimeError>;

    /// Atomically commit the terminal turn transition and the exact recovery
    /// checkpoint representing that transition. Durable implementations must
    /// use one storage transaction: exposing a terminal turn with an older
    /// checkpoint makes the next provider request unrecoverable.
    async fn finish_turn_with_checkpoint(
        &self,
        turn_id: &str,
        status: RuntimeTurnTerminalStatus,
        detail: Option<&str>,
        session: &Session,
    ) -> Result<(), RuntimeError>;
}

#[cfg(test)]
mod artifact_reducer_tests {
    use super::*;

    #[test]
    fn text_preview_is_utf8_safe_and_accounts_exact_source_bytes() {
        let output = "数".repeat(8_000);
        let preview = reduce_runtime_artifact("read_file", &output, 4_096);
        assert_eq!(preview.kind, RuntimeArtifactKind::Text);
        assert!(preview.truncated);
        assert_eq!(preview.source_bytes as usize, output.len());
        assert_eq!(
            preview.retained_bytes + preview.omitted_bytes,
            preview.source_bytes
        );
        assert!(preview.text.is_char_boundary(preview.text.len()));
    }

    #[test]
    fn table_and_log_use_distinct_typed_policies() {
        let table = serde_json::json!({
            "rows": (0..1_000).map(|id| serde_json::json!({"id":id})).collect::<Vec<_>>()
        })
        .to_string();
        let table_preview = reduce_runtime_artifact("nl2sql_query", &table, 2_048);
        assert_eq!(table_preview.kind, RuntimeArtifactKind::Table);
        assert_eq!(table_preview.total_rows, Some(1_000));
        assert!(table_preview.omitted_rows.is_some_and(|rows| rows > 0));

        let log = (0..500)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let log_preview = reduce_runtime_artifact("shell", &log, 2_048);
        assert_eq!(log_preview.kind, RuntimeArtifactKind::Log);
        assert!(log_preview.text.contains("line 499"));
    }
}
