//! Agent session manager — manages per-user `ConversationRuntime` instances.
//!
//! ## Key responsibilities
//!
//! 1. **Concurrency control** — each user has at most `max_concurrent` active sessions.
//! 2. **Workspace isolation** — every session is bound to a specific user's workspace.
//! 3. **Lifecycle management** — create, run turns, and destroy sessions.
//! 4. **Token usage recording** — every turn's usage is written to `token_usage` table.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};
use tokio::time::{sleep, Duration, Instant};

use crate::config_registry::{
    ContextArchiveParams, SessionCompactionParams, TenantConfigRegistry, UserRuntimeConfig,
};
use crate::error::{GatewayError, Result};
use crate::events::{AgentEvent, TurnResult};
use crate::gitlab::GitlabProjectManager;
use crate::runtime_builder::{
    compact_session_with_provider_native, run_streaming_turn_streaming,
    run_streaming_turn_with_options, summarize_session_for_compaction, BatchedTurnResult,
    BuiltRuntime, GatewayApiClient, GatewayToolExecutor, ProviderNativeCompactionBridge,
    RuntimeBuilder, StreamingResumableTurnResult, StreamingTurnOptions, StreamingTurnResult,
};
use runtime::{
    ContentBlock as RuntimeContentBlock, ConversationMessage, McpServerSessionManager, MessageRole,
    TokenUsage as RuntimeTokenUsage,
};
use sha2::Digest;

/// Maximum default concurrent sessions per user.
#[expect(dead_code)]
const DEFAULT_MAX_CONCURRENT: usize = 3;
const INTERNAL_TRANSIENT_SESSION_SWEEP_AFTER_SECS: i64 = 30 * 60;
const CONTEXT_ARCHIVE_MAX_CHARS: usize = 2_000_000;

struct TurnAdmission {
    accepting: AtomicBool,
    active: AtomicUsize,
    idle: Notify,
}

impl TurnAdmission {
    fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            idle: Notify::new(),
        }
    }

    fn admit(self: &Arc<Self>) -> Result<ActiveTurnGuard> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(GatewayError::ShuttingDown);
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if !self.accepting.load(Ordering::Acquire) {
            if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.idle.notify_waiters();
            }
            return Err(GatewayError::ShuttingDown);
        }
        Ok(ActiveTurnGuard {
            admission: Arc::clone(self),
        })
    }

    async fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.active.load(Ordering::Acquire) == 0 {
                return true;
            }
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active.load(Ordering::Acquire) == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.active.load(Ordering::Acquire) == 0;
            }
        }
    }
}

struct ActiveTurnGuard {
    admission: Arc<TurnAdmission>,
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        if self.admission.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.admission.idle.notify_waiters();
        }
    }
}

/// Rebuild the user-visible runtime session from the durable execution ledger.
/// JSONL is intentionally not consulted here: it is an export/compatibility
/// archive, while the ledger is the ordered recovery authority for new turns.
fn rebuild_session_from_ledger_rows(
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
    rows: &[(i64, String, String, String, Option<String>)],
) -> std::result::Result<Option<runtime::Session>, String> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut session = runtime::Session::new();
    session.session_id = session_id.to_string();
    session.tenant_id = Some(tenant_id.to_string());
    session.user_id = Some(user_id.to_string());
    // A session checkpoint contains the complete validated projection up to
    // its sequence. Recovery can therefore start at the newest checkpoint and
    // replay only the tail, avoiding an O(total-ledger-size) scan on every
    // history request. Full-ledger callers still start at sequence one.
    let mut expected_sequence = rows
        .first()
        .and_then(|row| u64::try_from(row.0).ok())
        .unwrap_or(1);
    for (stored_sequence, _event_type, raw, stored_hash, recovery_ciphertext) in rows {
        let sequence = u64::try_from(*stored_sequence)
            .map_err(|_| "execution ledger contains a negative sequence".to_string())?;
        if sequence != expected_sequence {
            return Err(format!(
                "execution ledger sequence gap: expected {expected_sequence}, found {sequence}"
            ));
        }
        let typed_envelope = serde_json::from_str::<agent_protocol::AgentEventEnvelope>(raw)
            .map_err(|error| format!("invalid execution-ledger envelope: {error}"))?;
        if typed_envelope.sequence != sequence {
            return Err(format!(
                "execution ledger sequence mismatch: row={sequence}, envelope={}",
                typed_envelope.sequence
            ));
        }
        if typed_envelope.schema_version != 1 {
            return Err(format!(
                "execution ledger contains unsupported required schema version {}",
                typed_envelope.schema_version
            ));
        }
        if typed_envelope.payload_hash != *stored_hash {
            return Err(format!(
                "execution ledger payload hash mismatch at sequence {sequence}"
            ));
        }
        typed_envelope.verify_hash().map_err(|error| {
            format!("execution ledger envelope failed hash verification: {error}")
        })?;
        if typed_envelope.thread_id != session_id {
            return Err(format!(
                "execution ledger envelope belongs to thread {}, expected {session_id}",
                typed_envelope.thread_id
            ));
        }
        let expected_event_type = match &typed_envelope.event {
            agent_protocol::AgentEventV1::Domain(domain) => {
                format!("{}.{}", domain.domain, domain.kind)
            }
            _ => "agent.event".to_string(),
        };
        if expected_event_type != *_event_type {
            return Err(format!(
                "execution ledger event type mismatch at sequence {sequence}: row={_event_type}, envelope={expected_event_type}"
            ));
        }
        expected_sequence = expected_sequence.saturating_add(1);
        let agent_protocol::AgentEventV1::Domain(domain) = &typed_envelope.event else {
            continue;
        };
        let protected_payload = &domain.payload;
        if domain.payload.get("_recoveryPayloadHash").is_some() && recovery_ciphertext.is_none() {
            return Err(
                "execution ledger is missing the encrypted payload required for exact recovery"
                    .to_string(),
            );
        }
        let recovery_payload = recovery_ciphertext
            .as_deref()
            .map(|ciphertext| {
                crate::crypto::decrypt_scoped(
                    ciphertext,
                    &crate::crypto::scoped_aad(
                        "ledger.raw_payload",
                        tenant_id,
                        &typed_envelope.event_id,
                    ),
                )
                .map_err(|error| format!("cannot decrypt runtime recovery payload: {error}"))
            })
            .transpose()?
            .map(|value| {
                let expected_hash = protected_payload
                    .get("_recoveryPayloadHash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        "encrypted recovery payload is not bound to the ledger envelope".to_string()
                    })?;
                let actual_hash = format!("{:x}", sha2::Sha256::digest(value.as_bytes()));
                if actual_hash != expected_hash {
                    return Err(
                        "decrypted recovery payload hash does not match ledger envelope"
                            .to_string(),
                    );
                }
                serde_json::from_str::<Value>(&value)
                    .map_err(|error| format!("invalid decrypted recovery payload: {error}"))
            })
            .transpose()?;
        let payload = recovery_payload.as_ref().unwrap_or(protected_payload);
        let kind = domain.kind.as_str();
        let turn_id = typed_envelope
            .turn_id
            .as_deref()
            .unwrap_or("recovered-turn");
        let known_runtime_kind = matches!(
            kind,
            "turn_started"
                | "assistant_message"
                | "visible_message"
                | "tool_outcome"
                | "tool_outcome_unknown"
                | "turn_terminal"
                | "context_manifest_committed"
                | "tool_intent_authorized"
                | "tool_started"
                | "session_checkpoint"
        );
        if domain.domain == "runtime"
            && !known_runtime_kind
            && domain
                .payload
                .get("_requiredForRecovery")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(format!(
                "execution ledger contains unknown required runtime event {kind}"
            ));
        }
        if kind == "turn_started" && !session.turns.iter().any(|turn| turn.turn_id == turn_id) {
            let input = payload
                .get("userInput")
                .and_then(Value::as_str)
                .unwrap_or_default();
            session.restore_turn(
                turn_id,
                input,
                session.messages.len(),
                None,
                runtime::SessionTurnStatus::Running,
            );
        }
        if kind == "session_checkpoint" {
            let checkpoint = payload
                .get("session")
                .ok_or_else(|| "session_checkpoint is missing session state".to_string())?;
            let state_hash = payload
                .get("stateHash")
                .and_then(Value::as_str)
                .ok_or_else(|| "session_checkpoint is missing stateHash".to_string())?;
            let actual_hash = format!(
                "{:x}",
                sha2::Sha256::digest(serde_json::to_vec(checkpoint).unwrap_or_default())
            );
            if actual_hash != state_hash {
                return Err("session_checkpoint state hash mismatch".to_string());
            }
            let checkpoint = runtime::Session::from_recovery_json(checkpoint)
                .map_err(|error| format!("invalid runtime session checkpoint: {error}"))?;
            if checkpoint.session_id != session_id
                || checkpoint.tenant_id.as_deref() != Some(tenant_id)
                || checkpoint.user_id.as_deref() != Some(user_id)
            {
                return Err("runtime session checkpoint scope mismatch".to_string());
            }
            session = checkpoint;
            continue;
        }
        let result = match kind {
            "turn_started" => payload
                .get("userInput")
                .and_then(Value::as_str)
                .map(runtime::ConversationMessage::user_text),
            "assistant_message" => payload
                .get("message")
                .and_then(|value| value.get("message"))
                .and_then(|value| {
                    serde_json::from_value::<runtime::ConversationMessage>(value.clone()).ok()
                }),
            "visible_message" => payload.get("message").and_then(|value| {
                serde_json::from_value::<runtime::ConversationMessage>(value.clone()).ok()
            }),
            "tool_outcome" => {
                let invocation_id = payload
                    .get("invocationId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "tool_outcome is missing invocationId".to_string())?;
                let tool_name = payload
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let output = payload
                    .get("modelOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("[tool result recovered from execution ledger]");
                Some(runtime::ConversationMessage::tool_result(
                    invocation_id.to_string(),
                    tool_name.to_string(),
                    output.to_string(),
                    payload
                        .get("outcome")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value != "completed"),
                ))
            }
            _ => None,
        };
        if let Some(message) = result {
            if session.push_message(message).is_err() {
                return Err("execution ledger produced an invalid message sequence".to_string());
            }
        }
        if kind == "turn_terminal" {
            let status = match payload.get("status").and_then(Value::as_str) {
                Some("completed") => runtime::SessionTurnStatus::Completed,
                Some("suspended") => runtime::SessionTurnStatus::Suspended,
                Some("rolled_back") => runtime::SessionTurnStatus::RolledBack,
                _ => runtime::SessionTurnStatus::Failed,
            };
            if let Some(turn) = session
                .turns
                .iter_mut()
                .find(|turn| turn.turn_id == turn_id)
            {
                turn.status = status;
                turn.end_message_count = Some(session.messages.len());
            }
        }
    }
    Ok(Some(session))
}

async fn load_durable_ledger_session(
    db: &sqlx::SqlitePool,
    session_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<Option<runtime::Session>> {
    crate::behavior_trace("PROTO-002");
    let rows = sqlx::query(
        "SELECT sequence, event_type, payload_json, payload_hash, raw_payload_ciphertext
         FROM agent_event_ledger
         WHERE tenant_id = ? AND thread_id = ? AND durable = 1
           AND sequence >= COALESCE(
             (SELECT MAX(sequence) FROM agent_event_ledger
              WHERE tenant_id = ? AND thread_id = ? AND durable = 1
                AND event_type = 'runtime.session_checkpoint'),
             1
           )
         ORDER BY sequence ASC",
    )
    .bind(tenant_id)
    .bind(session_id)
    .bind(tenant_id)
    .bind(session_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|row| {
        Ok((
            row.try_get::<i64, _>(0)?,
            row.try_get::<String, _>(1)?,
            row.try_get::<String, _>(2)?,
            row.try_get::<String, _>(3)?,
            row.try_get::<Option<String>, _>(4)?,
        ))
    })
    .collect::<std::result::Result<Vec<_>, sqlx::Error>>()?;
    rebuild_session_from_ledger_rows(session_id, tenant_id, user_id, &rows)
        .map_err(GatewayError::RuntimeBuild)
}

fn token_usage_delta(after: RuntimeTokenUsage, before: RuntimeTokenUsage) -> RuntimeTokenUsage {
    RuntimeTokenUsage {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        cache_creation_input_tokens: after
            .cache_creation_input_tokens
            .saturating_sub(before.cache_creation_input_tokens),
        cache_read_input_tokens: after
            .cache_read_input_tokens
            .saturating_sub(before.cache_read_input_tokens),
    }
}

fn priced_usage_cost(
    usage: RuntimeTokenUsage,
    model: &str,
    custom_input_price: Option<f64>,
    custom_output_price: Option<f64>,
) -> f64 {
    runtime::pricing_with_provenance(model, custom_input_price, custom_output_price)
        .0
        .map_or(0.0, |pricing| {
            usage
                .estimate_cost_usd_with_pricing(pricing)
                .total_cost_usd()
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InternalReasoningBudget {
    Fast,
    #[default]
    Standard,
    Deep,
}

#[derive(Debug, Clone, Default)]
pub struct AgentTurnOptions {
    pub blocked_tools: Vec<String>,
    /// Hide the entire tool registry for internal model-only turns.
    pub disable_tools: bool,
    /// Disable provider-side hidden thinking for bounded model-only turns.
    /// This is applied only when the provider exposes an explicit toggle.
    pub disable_provider_thinking: bool,
    pub enable_deferred_tools: bool,
    pub system_instructions: Vec<String>,
    pub reasoning_budget: InternalReasoningBudget,
    pub prefer_native_web_search: bool,
    pub suppress_native_web_search: bool,
    pub stream_timeout_secs: Option<u64>,
    pub disable_stream_timeout: bool,
    /// Stop exposing Web/search/fetch tools after this many completed Web tool
    /// results, while leaving completion and non-Web tools available.
    pub web_tool_result_budget: Option<usize>,
    /// Isolates exploration from final synthesis, domain verification and
    /// user-visible recovery reserves in the durable execution kernel.
    pub model_budget_stage: runtime::RuntimeModelBudgetStage,
}

fn context_window_recovery_options(options: &StreamingTurnOptions) -> StreamingTurnOptions {
    let mut recovery = options.clone();
    recovery.disable_tools = true;
    recovery.disable_provider_thinking = true;
    recovery.enable_deferred_tools = false;
    recovery.reasoning_budget = InternalReasoningBudget::Fast;
    recovery.prefer_native_web_search = false;
    recovery.suppress_native_web_search = true;
    recovery.disable_stream_timeout = false;
    recovery.stream_timeout_secs = Some(recovery.stream_timeout_secs.unwrap_or(90).min(90));
    recovery.web_tool_result_budget = None;
    recovery.model_budget_stage = runtime::RuntimeModelBudgetStage::UserVisibleError;
    recovery.system_instructions.push(
        "The previous tool-enabled attempt exceeded the model context window. Do not call tools. Give the user a concise, self-contained final response based on their request and any work already completed in the workspace. Never expose internal provider, context-window, retry, or product-branding errors."
            .to_string(),
    );
    recovery
}

#[derive(Debug, Clone)]
pub struct AgentSuspendedTurn {
    pub session_id: String,
    pub runtime_turn_id: String,
    pub deferred_tools: Vec<runtime::DeferredToolUse>,
    pub partial_text: String,
    pub tool_calls: Vec<crate::events::ToolCallRecord>,
    pub usage: crate::events::TokenUsageRecord,
    pub iterations: usize,
}

#[derive(Debug, Clone)]
pub enum AgentTurnRunOutcome {
    Completed(TurnResult),
    Suspended(AgentSuspendedTurn),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchToolCandidate {
    pub qualified_name: String,
    pub server_name: String,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSearchToolExecution {
    pub qualified_name: String,
    pub server_name: String,
    pub arguments: Value,
    pub output: Value,
}

fn runtime_scenario_from_source(source: &str) -> &str {
    if source.starts_with("pm_internal") || source.eq_ignore_ascii_case("pm") {
        "pm"
    } else if source.eq_ignore_ascii_case("materials") || source.eq_ignore_ascii_case("material") {
        "materials"
    } else if source.eq_ignore_ascii_case("chat")
        || source.eq_ignore_ascii_case("super_assistant")
        || source.eq_ignore_ascii_case("super-assistant")
    {
        "chat"
    } else if source.eq_ignore_ascii_case("nl2sql") {
        "nl2sql"
    } else {
        "rd"
    }
}

/// Resolve the Unified_Memory `app` bucket for a session identified by its
/// `source` (and optional explicit `scenario`), matching exactly the value the
/// compaction hook (`runtime_builder::memory_app_for_scenario`) persisted key
/// info under.
///
/// [`canonical_runtime_scenario`] already normalizes to the canonical scenario
/// labels (`chat` / `rd` / `pm` / `materials` / `nl2sql`), which are identical
/// to the memory `app` bucket names, so this is an identity mapping over the
/// canonical form. Exposed so the web layer's Zero_Loss measurement reads and
/// probes the *same* bucket the hook wrote to, with no re-derivation drift.
#[must_use]
pub fn memory_app_for_session_source(source: &str, scenario: Option<&str>) -> String {
    canonical_runtime_scenario(source, scenario).to_string()
}

fn is_internal_transient_source(source: &str) -> bool {
    source.starts_with("pm_internal")
        || source.starts_with("rd_internal")
        || source.starts_with("bot_internal")
        || source.starts_with("agent_team_internal")
}

fn is_hidden_runtime_source(source: &str) -> bool {
    is_internal_transient_source(source) || source.starts_with("rd_thread")
}

fn is_rd_candidate_source(source: &str) -> bool {
    source.starts_with("rd_internal_cand") || source.starts_with("rd_internal_candidate")
}

fn is_stale_internal_transient_session(session: &AgentSession, now: DateTime<Utc>) -> bool {
    is_internal_transient_source(&session.source)
        && matches!(session.state, SessionState::Idle)
        && now
            .signed_duration_since(session.last_activity)
            .num_seconds()
            >= INTERNAL_TRANSIENT_SESSION_SWEEP_AFTER_SECS
}

fn canonical_runtime_scenario<'a>(source: &'a str, scenario: Option<&'a str>) -> &'a str {
    match scenario.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.eq_ignore_ascii_case("chat") => "chat",
        Some(s) if s.eq_ignore_ascii_case("rd") || s.eq_ignore_ascii_case("agent") => "rd",
        Some(s) if s.eq_ignore_ascii_case("pm") => "pm",
        Some(s) if s.eq_ignore_ascii_case("materials") || s.eq_ignore_ascii_case("material") => {
            "materials"
        }
        Some(s) if s.eq_ignore_ascii_case("nl2sql") => "nl2sql",
        _ => runtime_scenario_from_source(source),
    }
}

fn mcp_server_name_from_qualified_tool(name: &str) -> String {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(server, _)| server.to_string())
        .unwrap_or_default()
}

fn is_search_like_mcp_tool(tool: &runtime::mcp::McpTool) -> bool {
    let mut haystack = tool.name.to_ascii_lowercase();
    if let Some(description) = &tool.description {
        haystack.push(' ');
        haystack.push_str(&description.to_ascii_lowercase());
    }
    if let Some(schema) = &tool.input_schema {
        haystack.push(' ');
        haystack.push_str(&schema.to_string().to_ascii_lowercase());
    }
    [
        "search", "browser", "browse", "fetch", "web", "url", "crawl",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn mcp_search_tool_arguments(
    tool: &McpSearchToolCandidate,
    query: &str,
    max_results: usize,
) -> Option<Value> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let schema = tool.input_schema.as_ref();
    let properties = schema
        .and_then(|value| value.get("properties"))
        .and_then(Value::as_object);

    let mut object = serde_json::Map::new();
    if let Some(properties) = properties {
        let query_key = [
            "query", "q", "keyword", "keywords", "term", "text", "input", "prompt",
        ]
        .iter()
        .find(|key| properties.contains_key(**key))
        .copied();
        let url_key = ["url", "uri", "href", "link"]
            .iter()
            .find(|key| properties.contains_key(**key))
            .copied();
        if let Some(key) = query_key {
            object.insert(key.to_string(), Value::String(query.to_string()));
        } else if let Some(key) = url_key {
            if query.starts_with("http://") || query.starts_with("https://") {
                object.insert(key.to_string(), Value::String(query.to_string()));
            } else {
                return None;
            }
        } else {
            object.insert("query".to_string(), Value::String(query.to_string()));
        }

        for key in ["limit", "maxResults", "max_results", "count", "num_results"] {
            if properties.contains_key(key) {
                object.insert(
                    key.to_string(),
                    Value::Number(serde_json::Number::from(max_results.max(1) as u64)),
                );
                break;
            }
        }
    } else {
        object.insert("query".to_string(), Value::String(query.to_string()));
        object.insert(
            "limit".to_string(),
            Value::Number(serde_json::Number::from(max_results.max(1) as u64)),
        );
    }

    Some(Value::Object(object))
}

fn apply_model_override(config: &mut UserRuntimeConfig, model: Option<&str>) {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return;
    };
    config.model = model.to_string();
    if config
        .api_keys
        .iter()
        .any(|key| key.model.as_deref() == Some(model))
    {
        config
            .api_keys
            .retain(|key| key.model.as_deref() == Some(model));
    }
}

fn reload_model_override<'a>(
    requested_model: Option<&'a str>,
    current_model: &'a str,
    model_pinned: bool,
) -> Option<&'a str> {
    requested_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| model_pinned.then_some(current_model))
}

fn build_default_session_name(locale: Option<&str>) -> String {
    let now = chrono::Local::now();
    let stamp = now.format("%m-%d %H:%M");
    let lang = locale.unwrap_or_default().trim().to_ascii_lowercase();
    if lang.starts_with("zh") {
        format!("会话 {stamp}")
    } else {
        format!("Session {stamp}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Running,
    Paused,
    Completed,
    Failed,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl From<&str> for SessionState {
    fn from(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Idle,
        }
    }
}

/// A live agent session bound to a user's workspace.
pub struct AgentSession {
    pub session_id: String,
    pub name: String,
    pub user_id: String,
    pub tenant_id: String,
    pub source: String,
    pub workspace: PathBuf,
    pub state: SessionState,
    pub model: String,
    /// True only when the user explicitly selected a model for this session.
    /// Unpinned sessions follow the highest-priority API key after config changes.
    pub model_pinned: bool,
    pub provider: String,
    /// Custom input price from the API key (None = use default).
    pub custom_input_price: Option<f64>,
    /// Custom output price from the API key (None = use default).
    pub custom_output_price: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub built: BuiltRuntime,
    /// Session configuration metadata (MCP servers, Skills, permission mode).
    pub session_metadata: crate::events::SessionMetadata,
    /// Incremented by the session manager whenever the per-tenant MCP or Skills
    /// configuration changes. Before each turn, if this counter differs from
    /// `AgentSessionManager::mcp_config_version[tenant_id]`, the runtime is
    /// rebuilt with fresh MCP tools and the latest skill instructions.
    /// This enables hot-reload: operators can add/remove/toggle MCP servers
    /// or Skills from the `WebUI`, and existing sessions pick up the changes
    /// without requiring a new session to be created.
    pub config_version: u64,
    /// Tracks the `api_keys_version` from the `tenants` table at the time the
    /// session was created or last hot-reloaded. Sessions compare this against
    /// the current DB value on each turn — if they differ, the runtime is rebuilt
    /// with the fresh (possibly updated/deleted) API key set. This ensures
    /// existing sessions immediately pick up key changes made from the `WebUI`.
    pub api_keys_version: u64,
    /// Set to `true` by `run_turn` when `hot_reload_session` was called before
    /// this turn. Cleared after the turn result is returned so the frontend can
    /// react to it exactly once.
    pub hot_reloaded_this_turn: bool,
    /// Whether this session is pinned to the top of the session list.
    pub is_pinned: bool,
    /// Whether this session is bookmarked (starred) by the user.
    pub is_bookmarked: bool,
}

/// A handle returned to callers when a session is created.
/// The handle can be used to interact with the session.
/// Dropping the handle does NOT destroy the session — use `AgentSessionManager::destroy_session`.
#[derive(Clone)]
pub struct SessionHandle {
    pub session_id: String,
    pub name: String,
    pub user_id: String,
    pub tenant_id: String,
    pub workspace: PathBuf,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub session_metadata: crate::events::SessionMetadata,
    pub is_pinned: bool,
    pub is_bookmarked: bool,
    pub source: String,
}

/// The global session manager.
pub struct AgentSessionManager {
    /// Root data directory for tenant/user pairs.
    data_dir: PathBuf,
    /// Per-user active sessions.
    sessions: Arc<RwLock<HashMap<String, AgentSession>>>,
    /// Per-session run locks — acquired for the duration of a turn to prevent concurrent
    /// turns on the same session. Each session gets its own `Mutex<()>` so sessions are
    /// independently non-blocking while others run. Sessions not in this map are treated
    /// as having the lock available (they will be inserted on first use).
    session_locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
    /// Config registry (provides DB access).
    config_registry: Arc<TenantConfigRegistry>,
    /// Gitlab project manager.
    gitlab_manager: Arc<GitlabProjectManager>,
    /// Per-tenant MCP server managers — each tenant gets its own isolated manager.
    /// This prevents cross-tenant MCP state leakage during hot-reload.
    /// Key: `tenant_id`, Value: `Arc<RwLock<McpServerSessionManager>>`
    mcp_managers: Arc<RwLock<HashMap<String, Arc<RwLock<McpServerSessionManager>>>>>,
    /// Per-tenant config version counter.
    /// Incremented whenever `reload_mcp_servers` rebuilds a tenant's MCP manager.
    /// Existing sessions compare their `config_version` against this counter at
    /// the start of each turn; mismatch triggers a hot-reload of the runtime.
    mcp_config_version: Arc<RwLock<HashMap<String, u64>>>,
    /// Per-session cooperative cancel handles for the currently running turn.
    ///
    /// The session lock still prevents concurrent turns. This map exists so UI
    /// actions such as "Stop" can signal the in-flight async turn to drop its
    /// provider request, restore runtime state, and return control to the user.
    running_turn_cancels: Arc<RwLock<HashMap<String, oneshot::Sender<()>>>>,
    /// Process-lifecycle gate. Shutdown closes admission before observing the
    /// active count, then waits for every admitted turn to reach a durable
    /// terminal state (or cooperatively cancels it after the grace period).
    turn_admission: Arc<TurnAdmission>,
    /// Optional factory that builds a per-session "extract → persist → compact"
    /// hook (e.g. the web-server Super_Assistant `RuntimeCompactionHook`) applied
    /// to every runtime this manager builds. Set once at startup; `None` keeps
    /// the runtime's default heuristic compaction (Req 4.1 / 4.3 / 4.9).
    compaction_hook_factory: Option<crate::runtime_builder::CompactionHookFactory>,
}

#[derive(Debug, Clone)]
pub struct SessionContextStatus {
    pub session_id: String,
    pub model: String,
    pub provider: String,
    pub message_count: usize,
    pub estimated_tokens: usize,
    pub token_estimator: String,
    pub context_window: usize,
    pub effective_context_limit: usize,
    pub auto_compact_token_limit: usize,
    pub context_usage_percent: f64,
    pub tokens_until_compaction: isize,
    pub compaction_count: u32,
    pub last_compaction_summary: Option<String>,
    pub last_compaction_removed_messages: usize,
    pub unknown_context_window: bool,
    pub state: SessionState,
}

#[derive(Debug, Clone)]
pub struct ManualCompactionResult {
    pub session_id: String,
    pub trigger: String,
    pub strategy: String,
    pub provider_compaction_endpoint_supported: bool,
    pub provider_compaction_endpoint_called: bool,
    pub provider_compaction_output_applied: bool,
    pub provider_compaction_fallback_reason: Option<String>,
    pub summary: String,
    pub summary_tokens: usize,
    pub removed_message_count: usize,
    pub retained_tail_tokens: usize,
    pub message_count_after: usize,
    pub archive_entries: Vec<CompactionArchiveEntry>,
}

#[derive(Debug, Clone)]
pub struct CompactionArchiveEntry {
    pub role: String,
    pub ordinal: usize,
    pub content: String,
    pub content_kind: String,
}

fn session_owner_matches(
    actual_tenant: &str,
    actual_user: &str,
    expected_tenant: Option<&str>,
    expected_user: Option<&str>,
) -> bool {
    expected_tenant.is_none_or(|tenant| tenant == actual_tenant)
        && expected_user.is_none_or(|user| user == actual_user)
}

impl AgentSessionManager {
    #[must_use]
    pub fn new(
        data_dir: PathBuf,
        config_registry: Arc<TenantConfigRegistry>,
        gitlab_manager: Arc<GitlabProjectManager>,
        _config_home: PathBuf,
    ) -> Self {
        Self {
            data_dir,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_locks: Arc::new(RwLock::new(HashMap::new())),
            config_registry,
            gitlab_manager,
            // Per-tenant managers: lazily initialized on first session creation per tenant.
            mcp_managers: Arc::new(RwLock::new(HashMap::new())),
            // Config version starts at 0; incremented on each hot-reload.
            mcp_config_version: Arc::new(RwLock::new(HashMap::new())),
            running_turn_cancels: Arc::new(RwLock::new(HashMap::new())),
            turn_admission: Arc::new(TurnAdmission::new()),
            compaction_hook_factory: None,
        }
    }

    /// Inject the per-session compaction-hook factory applied to every runtime
    /// this manager builds. Higher layers (the web-server Super_Assistant
    /// orchestration) call this once at startup so the runtime's real
    /// auto-compaction trigger runs the "extract → persist → compact" closure
    /// before compaction commits (Req 4.1 / 4.3 / 4.9).
    #[must_use]
    pub fn with_compaction_hook_factory(
        mut self,
        factory: crate::runtime_builder::CompactionHookFactory,
    ) -> Self {
        self.compaction_hook_factory = Some(factory);
        self
    }

    /// Attach the manager's compaction-hook factory to a freshly created
    /// [`RuntimeBuilder`] when one is configured (Req 4.1 / 4.3 / 4.9). A no-op
    /// when no factory was injected, preserving the default heuristic compaction.
    fn attach_compaction_hook_factory(&self, builder: RuntimeBuilder) -> RuntimeBuilder {
        match self.compaction_hook_factory.clone() {
            Some(factory) => builder.with_compaction_hook_factory(factory),
            None => builder,
        }
    }

    /// Compute the workspace root for a user.
    fn user_workspace_root(&self, tenant_id: &str, user_id: &str) -> PathBuf {
        self.data_dir
            .join(tenant_id)
            .join(user_id)
            .join("workspace")
    }

    /// Create a new agent session for a user.
    ///
    /// Returns a `SessionHandle` that can be used to interact with the session.
    ///
    /// `scenario` is the usage context (e.g. `"agent"`, `"chat"`, `"nl2sql"`) used to filter
    /// API keys. If None, all chat-model keys are used (backward-compatible default).
    ///
    /// # Errors
    ///
    /// - `ConcurrencyLimit` if the user already has `max_concurrent` active sessions.
    #[allow(clippy::too_many_lines)]
    pub async fn create_session(
        &self,
        user_id: &str,
        tenant_id: &str,
        project_id: Option<&str>,
        model: Option<&str>,
        source: &str,
        scenario: Option<&str>,
        locale: Option<&str>,
        allowed_tools_override: Option<Vec<String>>,
    ) -> Result<SessionHandle> {
        // Proactively sweep leaked idle internal transient sessions for this user.
        // Do not delete running internal sessions: concurrent PM/RD background tasks
        // use these sessions as their active runtime and must not kill each other.
        let now = Utc::now();
        let internal_session_ids = {
            let sessions = self.sessions.read().await;
            sessions
                .values()
                .filter(|s| s.user_id == user_id && s.tenant_id == tenant_id)
                .filter(|s| is_stale_internal_transient_session(s, now))
                .map(|s| s.session_id.clone())
                .collect::<Vec<_>>()
        };
        for sid in internal_session_ids {
            let _ = self.destroy_session(&sid).await;
        }

        // Reconcile stale in-memory sessions with DB active records.
        // This prevents rare race leftovers (in-memory exists while DB already completed)
        // from permanently occupying user workspace quota.
        if let Ok(active_records) = self
            .config_registry
            .list_agent_sessions(tenant_id, user_id, None)
            .await
        {
            let active_ids: HashSet<String> = active_records
                .into_iter()
                .map(|record| record.session_id)
                .collect();
            let stale_ids = {
                let sessions = self.sessions.read().await;
                sessions
                    .values()
                    .filter(|s| s.user_id == user_id && s.tenant_id == tenant_id)
                    .filter(|s| !is_hidden_runtime_source(&s.source))
                    .filter(|s| !active_ids.contains(&s.session_id))
                    .map(|s| s.session_id.clone())
                    .collect::<Vec<_>>()
            };
            for sid in stale_ids {
                let _ = self.destroy_session(&sid).await;
            }
        }

        // 1. Load user quota for concurrency check
        let quota = self
            .config_registry
            .get_user_quota(tenant_id, user_id)
            .await
            .map_err(GatewayError::Database)?;

        // 2. Count open sessions for this user.
        // Do NOT gate session creation by currently running turns, otherwise
        // one long task can block creating/switching to another session.
        // Running-turn concurrency is controlled separately at turn execution time.
        let open_session_count = {
            let sessions = self.sessions.read().await;
            sessions
                .values()
                .filter(|s| s.user_id == user_id && s.tenant_id == tenant_id)
                .filter(|s| !is_hidden_runtime_source(&s.source))
                .count()
        };

        if open_session_count >= quota.max_workspaces {
            return Err(GatewayError::ConcurrencyLimit {
                max: quota.max_workspaces,
                current: open_session_count,
            });
        }

        // 3. Build workspace path. When a project is bound, the runtime's cwd
        // must be the repository root, matching the CLI/Cursor/Claude Code
        // mental model. The per-user workspace remains the parent sandbox.
        let user_workspace = self.user_workspace_root(tenant_id, user_id);
        tokio::fs::create_dir_all(&user_workspace)
            .await
            .map_err(GatewayError::Io)?;
        let workspace = if let Some(pid) = project_id {
            self.gitlab_manager
                .sync_project(tenant_id, user_id, pid)
                .await?
        } else {
            user_workspace
        };

        // Create runtime metadata directory inside the effective workspace.
        tokio::fs::create_dir_all(workspace.join(".aos"))
            .await
            .map_err(GatewayError::Io)?;

        // 4. Load user runtime config, scoped to the requested scenario.
        let runtime_scenario = canonical_runtime_scenario(source, scenario);
        let mut config = self
            .config_registry
            .load_user_config(tenant_id, user_id, Some(runtime_scenario))
            .await
            .map_err(GatewayError::Database)?;

        let model_pinned = model.is_some_and(|value| !value.trim().is_empty());
        // Allow model override. If the selected model exists in api_keys, keep only
        // matching keys so UI model selection cannot be silently overridden by the
        // first priority key's own model.
        apply_model_override(&mut config, model);
        if let Some(allowed_tools) = allowed_tools_override {
            config.allowed_tools = Some(allowed_tools);
        }

        // 5. Initialize the MCP manager with servers from the DB config.
        // This populates the manager's session list so `discover_all_tools()` finds them.
        self.init_mcp_manager_from_config(tenant_id, &config)
            .await?;

        // 6. Build the runtime (extract pricing before consuming config)
        let first_key = config.api_keys.first();
        let custom_input_price = first_key.and_then(|k| k.input_price_per_million);
        let custom_output_price = first_key.and_then(|k| k.output_price_per_million);
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let builder = self.attach_compaction_hook_factory(RuntimeBuilder::new(
            config,
            workspace.clone(),
            tenant_mcp,
        ));
        let built = builder.build().await?;

        let session_id = built.session_id.clone();
        let model = built.model.clone();
        let now = Utc::now();
        let session_metadata = built.session_metadata.clone();

        // 7. Generate a human-readable session name (done early so it can be written to DB)
        let name = build_default_session_name(locale);

        // 8. Persist session record to DB
        self.config_registry
            .persist_session_record(
                tenant_id,
                user_id,
                &session_id,
                &workspace,
                source,
                &name,
                &model,
                model_pinned,
                &built.provider,
            )
            .await
            .map_err(GatewayError::Database)?;

        // 9. Store the session
        let session = AgentSession {
            session_id: session_id.clone(),
            name: name.clone(),
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            source: source.to_string(),
            workspace: workspace.clone(),
            state: SessionState::Idle,
            model: built.model.clone(),
            model_pinned,
            provider: built.provider.clone(),
            custom_input_price,
            custom_output_price,
            created_at: now,
            last_activity: now,
            built,
            session_metadata: session_metadata.clone(),
            config_version: 0,
            api_keys_version: self.config_registry.get_api_keys_version(tenant_id).await,
            hot_reloaded_this_turn: false,
            is_pinned: false,
            is_bookmarked: false,
        };

        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session);

        tracing::info!(
            user_id,
            tenant_id,
            session_id = %session_id,
            workspace = %workspace.display(),
            "agent session created"
        );

        Ok(SessionHandle {
            session_id,
            name,
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            workspace,
            model,
            created_at: now,
            session_metadata,
            is_pinned: false,
            is_bookmarked: false,
            source: source.to_string(),
        })
    }

    /// Create a hidden internal runtime session bound to an already prepared workspace.
    ///
    /// Code Studio uses this for candidate worktrees: the model may use normal
    /// workspace-write tools inside the isolated worktree, while the primary
    /// repository still receives only an explicit, user-approved diff.
    #[allow(clippy::too_many_lines)]
    pub async fn create_internal_session_in_workspace(
        &self,
        user_id: &str,
        tenant_id: &str,
        workspace: PathBuf,
        model: Option<&str>,
        source: &str,
        scenario: Option<&str>,
        allowed_tools_override: Option<Vec<String>>,
    ) -> Result<SessionHandle> {
        if !is_hidden_runtime_source(source) {
            return Err(GatewayError::Validation(
                "explicit workspace sessions must use a hidden internal source".to_string(),
            ));
        }
        tokio::fs::create_dir_all(workspace.join(".aos"))
            .await
            .map_err(GatewayError::Io)?;

        let runtime_scenario = canonical_runtime_scenario(source, scenario);
        let mut config = self
            .config_registry
            .load_user_config(tenant_id, user_id, Some(runtime_scenario))
            .await
            .map_err(GatewayError::Database)?;

        let model_pinned = model.is_some_and(|value| !value.trim().is_empty());
        apply_model_override(&mut config, model);
        if let Some(allowed_tools) = allowed_tools_override {
            config.allowed_tools = Some(allowed_tools);
        }
        if is_rd_candidate_source(source) {
            config.blocked_tools.retain(|tool| {
                !matches!(tool.as_str(), "write_file" | "edit_file" | "NotebookEdit")
            });
        }

        self.init_mcp_manager_from_config(tenant_id, &config)
            .await?;

        let first_key = config.api_keys.first();
        let custom_input_price = first_key.and_then(|k| k.input_price_per_million);
        let custom_output_price = first_key.and_then(|k| k.output_price_per_million);
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let builder = self.attach_compaction_hook_factory(RuntimeBuilder::new(
            config,
            workspace.clone(),
            tenant_mcp,
        ));
        let built = builder.build().await?;

        let session_id = built.session_id.clone();
        let model = built.model.clone();
        let now = Utc::now();
        let session_metadata = built.session_metadata.clone();
        let name = build_default_session_name(None);

        self.config_registry
            .persist_session_record(
                tenant_id,
                user_id,
                &session_id,
                &workspace,
                source,
                &name,
                &model,
                model_pinned,
                &built.provider,
            )
            .await
            .map_err(GatewayError::Database)?;

        let session = AgentSession {
            session_id: session_id.clone(),
            name: name.clone(),
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            source: source.to_string(),
            workspace: workspace.clone(),
            state: SessionState::Idle,
            model: built.model.clone(),
            model_pinned,
            provider: built.provider.clone(),
            custom_input_price,
            custom_output_price,
            created_at: now,
            last_activity: now,
            built,
            session_metadata: session_metadata.clone(),
            config_version: 0,
            api_keys_version: self.config_registry.get_api_keys_version(tenant_id).await,
            hot_reloaded_this_turn: false,
            is_pinned: false,
            is_bookmarked: false,
        };

        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session);

        tracing::info!(
            user_id,
            tenant_id,
            session_id = %session_id,
            workspace = %workspace.display(),
            source,
            "internal agent session created in explicit workspace"
        );

        Ok(SessionHandle {
            session_id,
            name,
            user_id: user_id.to_string(),
            tenant_id: tenant_id.to_string(),
            workspace,
            model,
            created_at: now,
            session_metadata,
            is_pinned: false,
            is_bookmarked: false,
            source: source.to_string(),
        })
    }

    /// Copy only the immutable, completed-turn prefix of a parent into an idle
    /// child runtime. The child keeps its own session identity, kernel, budget,
    /// permissions and future turns; in-progress parent state is never shared.
    pub async fn install_completed_turn_prefix(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<()> {
        let mut prefix = self
            .get_session_messages(parent_session_id, Some(tenant_id), Some(user_id))
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(parent_session_id.to_string()))?;
        let completed_end = prefix
            .turns
            .iter()
            .rev()
            .find(|turn| {
                matches!(
                    turn.status,
                    runtime::SessionTurnStatus::Completed
                        | runtime::SessionTurnStatus::Failed
                        | runtime::SessionTurnStatus::Cancelled
                )
            })
            .and_then(|turn| turn.end_message_count)
            .unwrap_or(0)
            .min(prefix.messages.len());
        prefix.messages.truncate(completed_end);
        prefix.turns.clear();
        prefix.prompt_history.clear();
        prefix.session_id = child_session_id.to_string();
        prefix.tenant_id = Some(tenant_id.to_string());
        prefix.user_id = Some(user_id.to_string());

        let mut sessions = self.sessions.write().await;
        let child = sessions
            .get_mut(child_session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(child_session_id.to_string()))?;
        if child.tenant_id != tenant_id || child.user_id != user_id {
            return Err(GatewayError::Unauthorized);
        }
        if !matches!(child.state, SessionState::Idle) {
            return Err(GatewayError::SessionBusy);
        }
        let mut runtime = child
            .built
            .take_runtime_arc()
            .ok_or(GatewayError::SessionBusy)?;
        let Some(runtime_mut) = Arc::get_mut(&mut runtime) else {
            child.built.return_runtime(runtime);
            return Err(GatewayError::RuntimeExecution(
                "child runtime is unexpectedly shared while installing fork prefix".into(),
            ));
        };
        runtime_mut.restore_session(prefix);
        child.built.return_runtime(runtime);
        Ok(())
    }

    /// Run a turn in an existing session.
    ///
    /// Returns the turn result.
    ///
    /// ## Hot-reload
    ///
    /// Before executing the turn, this method compares the session's
    /// `config_version` against the current global counter for the tenant,
    /// and also checks if the tenant's `api_keys_version` has changed.
    /// If either has changed, the runtime is rebuilt with fresh MCP tool
    /// definitions, skill instructions, and API keys, and the session's
    /// version fields are updated.
    #[allow(clippy::too_many_lines)]
    pub async fn run_turn(&self, session_id: &str, user_input: String) -> Result<TurnResult> {
        self.run_turn_with_options(session_id, user_input, AgentTurnOptions::default())
            .await
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run_turn_with_options(
        &self,
        session_id: &str,
        user_input: String,
        options: AgentTurnOptions,
    ) -> Result<TurnResult> {
        let _active_turn = self.turn_admission.admit()?;
        tracing::debug!(
            "session_manager::run_turn: starting for session {}",
            session_id
        );

        // Non-blocking per-session lock — prevents concurrent turns on the same session.
        // Returns 409 CONFLICT if a turn is already in-flight.
        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(l) = lock {
            l
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };

        // Check config version and API keys version before acquiring the write lock.
        // Both MCP/Skills changes and API key mutations should trigger a hot-reload.
        let needs_hot_reload = {
            let tenant_id = {
                let sessions = self.sessions.read().await;
                sessions.get(session_id).map(|s| s.tenant_id.clone())
            };

            let (session_config_version, session_keys_version) = {
                let sessions = self.sessions.read().await;
                sessions
                    .get(session_id)
                    .map_or((0, 0), |s| (s.config_version, s.api_keys_version))
            };

            let current_config_version = {
                let version_guard = self.mcp_config_version.read().await;
                version_guard
                    .get(tenant_id.as_deref().unwrap_or(""))
                    .copied()
                    .unwrap_or(0)
            };

            let current_keys_version = match tenant_id {
                Some(ref tid) => self.config_registry.get_api_keys_version(tid).await,
                None => 0,
            };

            session_config_version != current_config_version
                || session_keys_version != current_keys_version
        };

        if needs_hot_reload {
            tracing::debug!(
                "session_manager::run_turn: config or API keys version mismatch, hot-reloading runtime for session {}",
                session_id
            );
            // Mark hot_reloaded before rebuilding so the flag is visible when we build TurnResult.
            {
                let mut sessions = self.sessions.write().await;
                if let Some(s) = sessions.get_mut(session_id) {
                    s.hot_reloaded_this_turn = true;
                }
            }
            self.hot_reload_session(session_id).await?;
        }

        // Extract the session from the map so long-running non-streaming turns
        // do not block unrelated operations (list/history/create/other sessions).
        let mut sessions = self.sessions.write().await;
        let mut session = sessions
            .remove(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        drop(sessions);

        tracing::info!(
            "session_manager::run_turn: found session, state: {:?}",
            session.state
        );

        // Clone scalar fields before moving session back into the map.
        let model = session.model.clone();
        let provider = session.provider.clone();
        let tenant_id = session.tenant_id.clone();
        let user_id = session.user_id.clone();
        let custom_input_price = session.custom_input_price;
        let custom_output_price = session.custom_output_price;
        let session_metadata = session.session_metadata.clone();
        let hot_reloaded_this_turn = session.hot_reloaded_this_turn;
        let session_id_owned = session.session_id.clone();
        let hook_scenario = runtime_scenario_from_source(&session.source).to_string();
        let supports_native_compaction = session.built.supports_native_compaction;
        let native_compaction_bridge = session.built.native_compaction_bridge.clone();
        let context_window_tokens = session.built.context_window_tokens;

        tracing::info!(
            "session_manager::run_turn: calling run_streaming_turn with model {}",
            model
        );

        // Take runtime out of BuiltRuntime. If missing, hot-reload and retry once.
        let mut runtime_arc = if let Some(runtime_arc) = session.built.take_runtime_arc() {
            runtime_arc
        } else {
            tracing::warn!(
                session_id = %session_id_owned,
                "run_turn: runtime missing (likely previous turn timeout/cancel), attempting hot-reload recovery"
            );
            session.state = SessionState::Idle;
            session.last_activity = Utc::now();
            {
                let mut sessions = self.sessions.write().await;
                sessions.insert(session_id_owned.clone(), session);
            }
            self.hot_reload_session(&session_id_owned).await?;
            let mut sessions = self.sessions.write().await;
            session = sessions
                .remove(&session_id_owned)
                .ok_or_else(|| GatewayError::SessionNotFound(session_id_owned.clone()))?;
            session.built.take_runtime_arc().ok_or_else(|| {
                GatewayError::RuntimeExecution(
                    "session runtime unavailable after hot-reload recovery".to_string(),
                )
            })?
        };

        // Keep a "running" session shell in the map while runtime executes out-of-map.
        let mut session_running = session;
        session_running.state = SessionState::Running;
        session_running.last_activity = Utc::now();
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id_owned.clone(), session_running);
        }

        let runtime = Arc::get_mut(&mut runtime_arc).ok_or_else(|| {
            GatewayError::RuntimeExecution("runtime Arc is not uniquely owned".to_string())
        })?;
        let pre_turn_compaction = compact_runtime_for_continuity(
            runtime,
            &session_id_owned,
            &provider,
            &model,
            context_window_tokens,
            "auto",
            supports_native_compaction,
            native_compaction_bridge.as_ref(),
        )
        .await?;

        // Execute without holding sessions map lock.
        let user_input_for_retry = user_input.clone();
        let mut context_retry_compaction = None;
        let streaming_options = StreamingTurnOptions {
            blocked_tools: options.blocked_tools.clone(),
            disable_tools: options.disable_tools,
            disable_provider_thinking: options.disable_provider_thinking,
            enable_deferred_tools: options.enable_deferred_tools,
            system_instructions: options.system_instructions.clone(),
            reasoning_budget: options.reasoning_budget,
            prefer_native_web_search: options.prefer_native_web_search,
            suppress_native_web_search: options.suppress_native_web_search,
            stream_timeout_secs: options.stream_timeout_secs,
            disable_stream_timeout: options.disable_stream_timeout,
            web_tool_result_budget: options.web_tool_result_budget,
            model_budget_stage: options.model_budget_stage,
        };
        let turn_usage_before = runtime.usage().cumulative_usage();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        {
            let mut cancels = self.running_turn_cancels.write().await;
            if let Some(stale_cancel) = cancels.insert(session_id_owned.clone(), cancel_tx) {
                let _ = stale_cancel.send(());
            }
        }
        let mut run_result = run_streaming_turn_with_options(
            runtime,
            user_input,
            streaming_options.clone(),
            Some(cancel_rx),
        )
        .await;
        if let Err(error) = run_result.as_ref() {
            if error.is_context_window_exceeded() {
                context_retry_compaction = compact_runtime_for_continuity(
                    runtime,
                    &session_id_owned,
                    &provider,
                    &model,
                    context_window_tokens,
                    "context_window_exceeded",
                    supports_native_compaction,
                    native_compaction_bridge.as_ref(),
                )
                .await?;
                let used_toolless_recovery = context_retry_compaction.is_none();
                let retry_options = if used_toolless_recovery {
                    context_window_recovery_options(&streaming_options)
                } else {
                    streaming_options.clone()
                };
                let (retry_cancel_tx, retry_cancel_rx) = oneshot::channel();
                {
                    let mut cancels = self.running_turn_cancels.write().await;
                    if let Some(stale_cancel) =
                        cancels.insert(session_id_owned.clone(), retry_cancel_tx)
                    {
                        let _ = stale_cancel.send(());
                    }
                }
                run_result = run_streaming_turn_with_options(
                    runtime,
                    user_input_for_retry.clone(),
                    retry_options,
                    Some(retry_cancel_rx),
                )
                .await;

                // A normal retry after historical compaction can still overflow
                // inside one unusually tool-heavy turn. Converge once without
                // tools rather than surfacing an internal provider error.
                if !used_toolless_recovery
                    && run_result
                        .as_ref()
                        .is_err_and(GatewayError::is_context_window_exceeded)
                {
                    let (recovery_cancel_tx, recovery_cancel_rx) = oneshot::channel();
                    {
                        let mut cancels = self.running_turn_cancels.write().await;
                        if let Some(stale_cancel) =
                            cancels.insert(session_id_owned.clone(), recovery_cancel_tx)
                        {
                            let _ = stale_cancel.send(());
                        }
                    }
                    run_result = run_streaming_turn_with_options(
                        runtime,
                        user_input_for_retry,
                        context_window_recovery_options(&streaming_options),
                        Some(recovery_cancel_rx),
                    )
                    .await;
                }
            }
        }
        {
            let mut cancels = self.running_turn_cancels.write().await;
            cancels.remove(&session_id_owned);
        }
        let turn_usage = token_usage_delta(runtime.usage().cumulative_usage(), turn_usage_before);

        // Always return runtime and restore session state.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(mut s) = sessions.remove(&session_id_owned) {
                s.built.return_runtime(runtime_arc);
                s.state = SessionState::Idle;
                s.last_activity = Utc::now();
                sessions.insert(session_id_owned.clone(), s);
            }
        }

        let BatchedTurnResult {
            events,
            tool_records,
            turn_summary,
        } = match run_result {
            Ok(ok) => ok,
            Err(err) => return Err(err),
        };

        tracing::debug!(
            "session_manager::run_turn: run_streaming_turn returned with {} events",
            events.len()
        );
        self.config_registry
            .record_hook_progress_events(&events, Some(&hook_scenario))
            .await;

        // Extract final text
        let text: String = events
            .iter()
            .filter_map(|e| {
                if let AgentEvent::TextDelta { text, .. } = e {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        // Extract thinking/reasoning content
        let thinking: Option<String> = {
            let parts: Vec<String> = events
                .iter()
                .filter_map(|e| {
                    if let AgentEvent::ThinkingDelta { text, .. } = e {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        };

        let (input_tokens, output_tokens, cache_creation, cache_read) = (
            turn_usage.input_tokens,
            turn_usage.output_tokens,
            turn_usage.cache_creation_input_tokens,
            turn_usage.cache_read_input_tokens,
        );

        let usage = RuntimeTokenUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        };

        let cost = priced_usage_cost(usage, &model, custom_input_price, custom_output_price);

        // Record to DB
        self.config_registry
            .record_token_usage(crate::TokenUsageParams {
                tenant_id: tenant_id.clone(),
                user_id: user_id.clone(),
                session_id: Some(session_id_owned.clone()),
                model: model.clone(),
                input_tokens: i64::from(input_tokens),
                output_tokens: i64::from(output_tokens),
                cache_creation_tokens: i64::from(cache_creation),
                cache_read_tokens: i64::from(cache_read),
                api_key_id: None,
                provider,
                custom_input_price,
                custom_output_price,
            })
            .await
            .ok(); // non-fatal

        let iterations = events
            .iter()
            .find_map(|e| {
                if let AgentEvent::Iteration { count } = e {
                    Some(*count)
                } else {
                    None
                }
            })
            .unwrap_or(1);

        let manual_compaction = context_retry_compaction
            .as_ref()
            .or(pre_turn_compaction.as_ref());
        let compacted = manual_compaction
            .map(|compaction| crate::events::CompactionRecord {
                summary: compaction.summary.clone(),
                removed_messages: compaction.removed_message_count,
                count: 1,
                trigger: Some(compaction.trigger.clone()),
                strategy: Some(compaction.strategy.clone()),
                summary_tokens: Some(compaction.summary_tokens),
                retained_tail_tokens: Some(compaction.retained_tail_tokens),
                provider_compaction_endpoint_supported: Some(
                    compaction.provider_compaction_endpoint_supported,
                ),
                provider_compaction_endpoint_called: Some(
                    compaction.provider_compaction_endpoint_called,
                ),
                provider_compaction_output_applied: Some(
                    compaction.provider_compaction_output_applied,
                ),
                provider_compaction_fallback_reason: compaction
                    .provider_compaction_fallback_reason
                    .clone(),
            })
            .or_else(|| {
                events.iter().find_map(|e| {
                    if let AgentEvent::SessionCompacted {
                        summary,
                        removed_messages,
                    } = e
                    {
                        Some(crate::events::CompactionRecord {
                            summary: summary.clone(),
                            removed_messages: *removed_messages,
                            count: 1,
                            trigger: Some("runtime_auto".to_string()),
                            strategy: None,
                            summary_tokens: None,
                            retained_tail_tokens: None,
                            provider_compaction_endpoint_supported: None,
                            provider_compaction_endpoint_called: None,
                            provider_compaction_output_applied: None,
                            provider_compaction_fallback_reason: None,
                        })
                    } else {
                        None
                    }
                })
            });
        let auto_archive_entries = turn_summary
            .as_ref()
            .and_then(|summary| summary.auto_compaction.as_ref())
            .map(|auto| build_compaction_archive_entries(&auto.archived_messages));
        let archive_entries = manual_compaction
            .map(|manual| manual.archive_entries.as_slice())
            .or_else(|| auto_archive_entries.as_deref());
        if let Some(compaction) = compacted.as_ref() {
            record_turn_compaction(
                self.config_registry.as_ref(),
                &tenant_id,
                &user_id,
                &session_id_owned,
                compaction,
                archive_entries,
            )
            .await;
        }

        Ok(TurnResult {
            session_id: session_id_owned,
            text,
            thinking,
            tool_calls: tool_records,
            usage: crate::events::TokenUsageRecord {
                input_tokens,
                output_tokens,
                cache_creation_tokens: cache_creation,
                cache_read_tokens: cache_read,
                total_tokens: input_tokens + output_tokens + cache_creation + cache_read,
                estimated_cost_usd: cost,
                model,
            },
            compacted,
            iterations,
            metadata: Some(session_metadata),
            hot_reloaded: hot_reloaded_this_turn,
        })
    }

    /// Per-session locking
    ///
    /// Each session has its own `Mutex<()>`, acquired here for the duration of the turn.
    /// This means multiple sessions for the same user can run turns concurrently —
    /// only turns on the **same** session are serialized. Other sessions and
    /// operations (create, delete, rename, list, history) are completely unaffected.
    #[allow(clippy::too_many_lines)]
    pub async fn run_turn_streaming(
        &self,
        session_id: &str,
        user_input: String,
        sender: mpsc::Sender<AgentEvent>,
    ) -> Result<TurnResult> {
        match self
            .run_turn_streaming_with_options(
                session_id,
                user_input,
                sender,
                AgentTurnOptions::default(),
            )
            .await?
        {
            AgentTurnRunOutcome::Completed(turn) => Ok(turn),
            AgentTurnRunOutcome::Suspended(turn) => Err(GatewayError::RuntimeExecution(format!(
                "runtime turn {} suspended outside the unified parent orchestrator",
                turn.runtime_turn_id
            ))),
        }
    }

    /// Streaming turn with per-turn policy overlays.
    ///
    /// Used by AI Chat to make search an explicit user choice without rebuilding
    /// or permanently mutating the session runtime. Search-off turns hide
    /// WebSearch/WebFetch for this request only; search-on turns can inject
    /// citation/grounding instructions.
    pub async fn run_turn_streaming_with_options(
        &self,
        session_id: &str,
        user_input: String,
        sender: mpsc::Sender<AgentEvent>,
        options: AgentTurnOptions,
    ) -> Result<AgentTurnRunOutcome> {
        self.run_or_resume_turn_streaming_with_options(
            session_id, user_input, sender, options, None, None,
        )
        .await
    }

    pub async fn resume_turn_streaming_with_options(
        &self,
        session_id: &str,
        deferred_results: Vec<runtime::DeferredToolResult>,
        sender: mpsc::Sender<AgentEvent>,
        options: AgentTurnOptions,
    ) -> Result<AgentTurnRunOutcome> {
        self.run_or_resume_turn_streaming_with_options(
            session_id,
            String::new(),
            sender,
            options,
            Some(deferred_results),
            None,
        )
        .await
    }

    pub async fn resume_turn_streaming_with_approval_decisions(
        &self,
        session_id: &str,
        approval_decisions: Vec<runtime::DeferredApprovalDecision>,
        sender: mpsc::Sender<AgentEvent>,
        options: AgentTurnOptions,
    ) -> Result<AgentTurnRunOutcome> {
        self.run_or_resume_turn_streaming_with_options(
            session_id,
            String::new(),
            sender,
            options,
            None,
            Some(approval_decisions),
        )
        .await
    }

    /// Commit a server-accepted terminal deferred tool result without issuing
    /// another model request. The authenticated owner is checked again before
    /// the suspended runtime checkpoint is modified.
    pub async fn finalize_suspended_turn_with_tool_results(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        results: Vec<runtime::DeferredToolResult>,
        final_text: String,
    ) -> Result<runtime::TurnSummary> {
        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(lock) = lock {
            lock
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Unauthorized);
        }
        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let result = match Arc::get_mut(&mut runtime_arc) {
            Some(runtime) => runtime
                .finalize_suspended_turn_with_tool_results(results, final_text)
                .await
                .map_err(GatewayError::Runtime),
            None => Err(GatewayError::RuntimeExecution(
                "runtime Arc is not uniquely owned".to_string(),
            )),
        };
        session.built.return_runtime(runtime_arc);
        if result.is_ok() {
            session.state = SessionState::Idle;
            session.last_activity = Utc::now();
        }
        result
    }

    /// Load a suspended runtime turn reconstructed from the durable Agent
    /// Ledger for a parent coordinator after process restart.
    pub async fn suspended_turn_snapshot(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<AgentSuspendedTurn>> {
        self.get_owned_session(session_id, tenant_id, user_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        let sessions = self.sessions.read().await;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Unauthorized);
        }
        let runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let snapshot = runtime_arc.suspended_turn_snapshot();
        session.built.return_runtime(runtime_arc);
        Ok(snapshot.map(|suspended| {
            let partial_text = suspended
                .partial_summary
                .assistant_messages
                .iter()
                .flat_map(|message| message.blocks.iter())
                .filter_map(|block| match block {
                    runtime::ContentBlock::Text { text } => Some(text.as_str()),
                    runtime::ContentBlock::ToolUse { .. }
                    | runtime::ContentBlock::ToolResult { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            AgentSuspendedTurn {
                session_id: session_id.to_string(),
                runtime_turn_id: suspended.turn_id,
                deferred_tools: suspended.deferred_tools,
                partial_text,
                tool_calls: Vec::new(),
                usage: crate::events::TokenUsageRecord::from_runtime(
                    suspended.partial_summary.usage,
                    &session.model,
                ),
                iterations: suspended.partial_summary.iterations,
            }
        }))
    }

    /// Remove an interrupted, non-suspended runtime turn before retrying the
    /// same durable parent request after restart.
    pub async fn rollback_interrupted_turn(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<bool> {
        self.get_owned_session(session_id, tenant_id, user_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Unauthorized);
        }
        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let result = Arc::get_mut(&mut runtime_arc)
            .ok_or_else(|| {
                GatewayError::RuntimeExecution("runtime Arc is not uniquely owned".to_string())
            })
            .and_then(|runtime| {
                runtime
                    .rollback_interrupted_turn()
                    .map_err(GatewayError::Runtime)
            });
        session.built.return_runtime(runtime_arc);
        result
    }

    /// Roll back the exact latest runtime turn associated with a durable parent
    /// request. Completed turns are eligible because the process may have
    /// crashed after model completion but before the parent committed its final
    /// answer.
    pub async fn rollback_turn_by_id(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        runtime_turn_id: &str,
    ) -> Result<bool> {
        self.get_owned_session(session_id, tenant_id, user_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Unauthorized);
        }
        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let result = Arc::get_mut(&mut runtime_arc)
            .ok_or_else(|| {
                GatewayError::RuntimeExecution("runtime Arc is not uniquely owned".to_string())
            })
            .and_then(|runtime| {
                runtime
                    .rollback_turn_by_id(runtime_turn_id)
                    .map_err(GatewayError::Runtime)
            });
        session.built.return_runtime(runtime_arc);
        result
    }

    /// Wait for an in-flight turn to release the runtime, then roll back the
    /// exact turn id. This is used by durable parent cancellation so its
    /// replacement visible history does not duplicate the original user turn.
    pub async fn rollback_turn_by_id_when_idle(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        runtime_turn_id: &str,
        max_wait: Duration,
    ) -> Result<bool> {
        let deadline = Instant::now() + max_wait;
        loop {
            match self.get_session_state(session_id).await {
                Some(SessionState::Running) if Instant::now() < deadline => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    sleep(remaining.min(Duration::from_millis(100))).await;
                }
                Some(SessionState::Running) => return Err(GatewayError::SessionBusy),
                Some(_) => {
                    return self
                        .rollback_turn_by_id(session_id, tenant_id, user_id, runtime_turn_id)
                        .await;
                }
                None => return Err(GatewayError::SessionNotFound(session_id.to_string())),
            }
        }
    }

    async fn run_or_resume_turn_streaming_with_options(
        &self,
        session_id: &str,
        user_input: String,
        sender: mpsc::Sender<AgentEvent>,
        mut options: AgentTurnOptions,
        deferred_results: Option<Vec<runtime::DeferredToolResult>>,
        approval_decisions: Option<Vec<runtime::DeferredApprovalDecision>>,
    ) -> Result<AgentTurnRunOutcome> {
        let _active_turn = self.turn_admission.admit()?;
        // Acquire the per-session lock — non-blocking. If the session already has
        // a turn in-flight, try_lock returns None and we return 409 CONFLICT.
        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(l) = lock {
            l
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };

        tracing::debug!(
            "session_manager::run_turn_streaming: starting for session {}",
            session_id
        );

        // A deferred resume is still the same model turn. Keep its runtime/key
        // snapshot stable and avoid one tenant-version query per tool result;
        // fresh user turns still perform the normal hot-reload check.
        let is_resume = deferred_results.is_some() || approval_decisions.is_some();
        let needs_hot_reload = if is_resume {
            false
        } else {
            let tenant_id = {
                let sessions = self.sessions.read().await;
                sessions.get(session_id).map(|s| s.tenant_id.clone())
            };
            let (session_config_version, session_keys_version) = {
                let sessions = self.sessions.read().await;
                sessions
                    .get(session_id)
                    .map_or((0, 0), |s| (s.config_version, s.api_keys_version))
            };
            let current_config_version = {
                let version_guard = self.mcp_config_version.read().await;
                version_guard
                    .get(tenant_id.as_deref().unwrap_or(""))
                    .copied()
                    .unwrap_or(0)
            };
            let current_keys_version = match tenant_id {
                Some(ref tid) => self.config_registry.get_api_keys_version(tid).await,
                None => 0,
            };
            session_config_version != current_config_version
                || session_keys_version != current_keys_version
        };

        if needs_hot_reload {
            tracing::debug!(
                "session_manager::run_turn_streaming: hot-reloading session {}",
                session_id
            );
            {
                let mut sessions = self.sessions.write().await;
                if let Some(s) = sessions.get_mut(session_id) {
                    s.hot_reloaded_this_turn = true;
                }
            }
            self.hot_reload_session(session_id).await?;
        }

        // Extract the session from the map so other requests (create, delete, rename, list)
        // are not blocked while the LLM is streaming. The per-session Mutex (_guard) already
        // prevents concurrent turns on the same session.
        let mut sessions = self.sessions.write().await;
        let mut session = sessions
            .remove(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        drop(sessions); // release the write lock

        tracing::info!(
            "session_manager::run_turn_streaming: found session, state: {:?}",
            session.state
        );

        // Clone all scalar fields from the session while we extract the runtime.
        let model = session.model.clone();
        let session_metadata = session.session_metadata.clone();
        let tenant_id = session.tenant_id.clone();
        let user_id = session.user_id.clone();
        let provider = session.provider.clone();
        let custom_input_price = session.custom_input_price;
        let custom_output_price = session.custom_output_price;
        let hot_reloaded_this_turn = session.hot_reloaded_this_turn;
        let session_id_owned = session.session_id.clone();
        let hook_scenario = runtime_scenario_from_source(&session.source).to_string();
        let supports_native_compaction = session.built.supports_native_compaction;
        let native_compaction_bridge = session.built.native_compaction_bridge.clone();
        let context_window_tokens = session.built.context_window_tokens;

        // Take the runtime out of the BuiltRuntime before moving the session into the map.
        // This uses Arc<RefCell<Option<T>>> inside BuiltRuntime to safely extract the
        // non-Copy runtime without holding any lock on the sessions map.
        // The per-session Mutex (_guard) ensures no concurrent turn on this session.
        // The runtime is returned via `return_runtime` below before any other operation.
        let mut runtime_arc = if let Some(runtime_arc) = session.built.take_runtime_arc() {
            runtime_arc
        } else {
            tracing::warn!(
                session_id = %session_id_owned,
                "run_turn_streaming: runtime missing (likely previous turn timeout/cancel), attempting hot-reload recovery"
            );
            session.state = SessionState::Idle;
            session.last_activity = Utc::now();
            {
                let mut sessions = self.sessions.write().await;
                sessions.insert(session_id_owned.clone(), session);
            }
            self.hot_reload_session(&session_id_owned).await?;
            let mut sessions = self.sessions.write().await;
            session = sessions
                .remove(&session_id_owned)
                .ok_or_else(|| GatewayError::SessionNotFound(session_id_owned.clone()))?;
            session.built.take_runtime_arc().ok_or_else(|| {
                GatewayError::RuntimeExecution(
                    "session runtime unavailable after hot-reload recovery".to_string(),
                )
            })?
        };

        // Create a "resting" session to keep in the map while the turn runs.
        // It has state=Running but the runtime has been extracted.
        let mut session_running = session;
        session_running.state = SessionState::Running;
        session_running.last_activity = Utc::now();

        // Re-insert the session into the map immediately so other requests are unblocked.
        // The per-session Mutex (_guard) still prevents concurrent turns on this session.
        {
            let mut sessions = self.sessions.write().await;
            sessions.insert(session_id_owned.clone(), session_running);
        }

        // Emit session_activated before running the turn.
        let _ = sender
            .try_send(AgentEvent::SessionActivated {
                mcp_servers: session_metadata.mcp_servers.clone(),
                skills: session_metadata.skills.clone(),
                permission_mode: session_metadata.permission_mode.clone(),
                model: session_metadata.model.clone(),
            })
            .ok();

        if hot_reloaded_this_turn {
            let _ = sender
                .try_send(AgentEvent::ConfigHotReload {
                    mcp_servers: session_metadata.mcp_servers.clone(),
                    skills: session_metadata.skills.clone(),
                    permission_mode: session_metadata.permission_mode.clone(),
                    model: session_metadata.model.clone(),
                })
                .ok();
        }

        // Run the streaming turn WITHOUT holding any lock on the sessions map.
        // Arc::get_mut gives us &mut T since we have the only Arc reference.
        let runtime = Arc::get_mut(&mut runtime_arc).ok_or_else(|| {
            GatewayError::RuntimeExecution("runtime Arc is not uniquely owned".to_string())
        })?;
        if is_resume {
            if let Some(turn) = runtime
                .session()
                .turns
                .iter()
                .rev()
                .find(|turn| matches!(turn.status, runtime::SessionTurnStatus::Suspended))
            {
                if let Some(context) = turn.runtime_context.as_ref() {
                    options.reasoning_budget = match context.reasoning_budget.as_deref() {
                        Some("fast") => InternalReasoningBudget::Fast,
                        Some("deep") => InternalReasoningBudget::Deep,
                        _ => InternalReasoningBudget::Standard,
                    };
                    options.blocked_tools = context.blocked_tools.clone();
                    options.disable_tools = context
                        .metadata
                        .get("disable_tools")
                        .is_some_and(|value| value == "true")
                        || !context.tools_enabled;
                    options.disable_provider_thinking = context
                        .metadata
                        .get("disable_provider_thinking")
                        .is_some_and(|value| value == "true");
                    options.enable_deferred_tools = context
                        .metadata
                        .get("deferred_tools_enabled")
                        .is_some_and(|value| value == "true");
                    options.prefer_native_web_search = context.prefer_native_web_search;
                    options.suppress_native_web_search = context.suppress_native_web_search;
                    options.stream_timeout_secs = context.stream_timeout_secs;
                    options.disable_stream_timeout = context.disable_stream_timeout;
                    options.web_tool_result_budget = context
                        .metadata
                        .get("web_tool_result_budget")
                        .and_then(|value| value.parse::<usize>().ok());
                    if let Some(serialized) = context.metadata.get("turn_system_instructions") {
                        if let Ok(instructions) = serde_json::from_str::<Vec<String>>(serialized) {
                            options.system_instructions = instructions;
                        }
                    }
                }
                if let Some(baseline) = turn.context_baseline.as_ref() {
                    let current = runtime.system_prompt_sections();
                    if baseline.system_prompt_sections.starts_with(current) {
                        options.system_instructions =
                            baseline.system_prompt_sections[current.len()..].to_vec();
                    }
                }
            }
        }
        let pre_turn_compaction = if !is_resume {
            compact_runtime_for_continuity(
                runtime,
                &session_id_owned,
                &provider,
                &model,
                context_window_tokens,
                "auto",
                supports_native_compaction,
                native_compaction_bridge.as_ref(),
            )
            .await?
        } else {
            None
        };
        if let Some(compaction) = pre_turn_compaction.as_ref() {
            let _ = sender
                .try_send(AgentEvent::SessionCompacted {
                    summary: compaction.summary.clone(),
                    removed_messages: compaction.removed_message_count,
                })
                .ok();
        }
        let (cancel_tx, cancel_rx) = oneshot::channel();
        {
            let mut cancels = self.running_turn_cancels.write().await;
            if let Some(stale_cancel) = cancels.insert(session_id_owned.clone(), cancel_tx) {
                let _ = stale_cancel.send(());
            }
        }
        let user_input_for_retry = user_input.clone();
        let mut context_retry_compaction = None;
        let turn_usage_before = runtime.usage().cumulative_usage();
        let streaming_options = StreamingTurnOptions {
            blocked_tools: options.blocked_tools.clone(),
            disable_tools: options.disable_tools,
            disable_provider_thinking: options.disable_provider_thinking,
            enable_deferred_tools: options.enable_deferred_tools,
            system_instructions: options.system_instructions.clone(),
            reasoning_budget: options.reasoning_budget,
            prefer_native_web_search: options.prefer_native_web_search,
            suppress_native_web_search: options.suppress_native_web_search,
            stream_timeout_secs: options.stream_timeout_secs,
            disable_stream_timeout: options.disable_stream_timeout,
            web_tool_result_budget: options.web_tool_result_budget,
            model_budget_stage: options.model_budget_stage,
        };
        let mut result = run_streaming_turn_streaming(
            runtime,
            user_input,
            sender.clone(),
            streaming_options.clone(),
            Some(cancel_rx),
            deferred_results.clone(),
            approval_decisions.clone(),
        )
        .await;
        if let Err(error) = result.as_ref() {
            if !is_resume && error.is_context_window_exceeded() {
                context_retry_compaction = compact_runtime_for_continuity(
                    runtime,
                    &session_id_owned,
                    &provider,
                    &model,
                    context_window_tokens,
                    "context_window_exceeded",
                    supports_native_compaction,
                    native_compaction_bridge.as_ref(),
                )
                .await?;
                if let Some(compaction) = context_retry_compaction.as_ref() {
                    let _ = sender
                        .try_send(AgentEvent::SessionCompacted {
                            summary: compaction.summary.clone(),
                            removed_messages: compaction.removed_message_count,
                        })
                        .ok();
                }
                let used_toolless_recovery = context_retry_compaction.is_none();
                let retry_options = if used_toolless_recovery {
                    context_window_recovery_options(&streaming_options)
                } else {
                    streaming_options.clone()
                };
                let (retry_cancel_tx, retry_cancel_rx) = oneshot::channel();
                {
                    let mut cancels = self.running_turn_cancels.write().await;
                    if let Some(stale_cancel) =
                        cancels.insert(session_id_owned.clone(), retry_cancel_tx)
                    {
                        let _ = stale_cancel.send(());
                    }
                }
                result = run_streaming_turn_streaming(
                    runtime,
                    user_input_for_retry.clone(),
                    sender.clone(),
                    retry_options,
                    Some(retry_cancel_rx),
                    None,
                    None,
                )
                .await;

                if !used_toolless_recovery
                    && result
                        .as_ref()
                        .is_err_and(GatewayError::is_context_window_exceeded)
                {
                    let (recovery_cancel_tx, recovery_cancel_rx) = oneshot::channel();
                    {
                        let mut cancels = self.running_turn_cancels.write().await;
                        if let Some(stale_cancel) =
                            cancels.insert(session_id_owned.clone(), recovery_cancel_tx)
                        {
                            let _ = stale_cancel.send(());
                        }
                    }
                    result = run_streaming_turn_streaming(
                        runtime,
                        user_input_for_retry,
                        sender.clone(),
                        context_window_recovery_options(&streaming_options),
                        Some(recovery_cancel_rx),
                        None,
                        None,
                    )
                    .await;
                }
            }
        }
        {
            let mut cancels = self.running_turn_cancels.write().await;
            cancels.remove(&session_id_owned);
        }
        let turn_usage = token_usage_delta(runtime.usage().cumulative_usage(), turn_usage_before);

        // Take the session back out, put the mutated runtime back, update state to Idle.
        let mut sessions = self.sessions.write().await;
        if let Some(mut s) = sessions.remove(&session_id_owned) {
            // Return the runtime to the BuiltRuntime's cell.
            s.built.return_runtime(runtime_arc);
            s.state = SessionState::Idle;
            s.last_activity = Utc::now();
            sessions.insert(session_id_owned.clone(), s);
        }

        match result {
            Ok(streaming_result) => {
                let (turn_summary, tool_records, hook_events, suspended) = match streaming_result {
                    StreamingResumableTurnResult::Completed(StreamingTurnResult {
                        turn_summary,
                        tool_records,
                        hook_events,
                    }) => (turn_summary, tool_records, hook_events, None),
                    StreamingResumableTurnResult::Suspended(result) => (
                        result.suspended.partial_summary.clone(),
                        result.tool_records,
                        result.hook_events,
                        Some(result.suspended),
                    ),
                };
                self.config_registry
                    .record_hook_progress_events(&hook_events, Some(&hook_scenario))
                    .await;

                let text: String = turn_summary
                    .assistant_messages
                    .iter()
                    .flat_map(|m| &m.blocks)
                    .filter_map(|b| {
                        if let RuntimeContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let thinking: Option<String> = turn_summary
                    .assistant_messages
                    .iter()
                    .find_map(|m| m.thinking.clone());

                let (input_tokens, output_tokens, cache_creation, cache_read) = (
                    turn_usage.input_tokens,
                    turn_usage.output_tokens,
                    turn_usage.cache_creation_input_tokens,
                    turn_usage.cache_read_input_tokens,
                );

                let usage = RuntimeTokenUsage {
                    input_tokens,
                    output_tokens,
                    cache_creation_input_tokens: cache_creation,
                    cache_read_input_tokens: cache_read,
                };
                let cost =
                    priced_usage_cost(usage, &model, custom_input_price, custom_output_price);

                let _ = self
                    .config_registry
                    .record_token_usage(crate::TokenUsageParams {
                        tenant_id: tenant_id.clone(),
                        user_id: user_id.clone(),
                        session_id: Some(session_id_owned.clone()),
                        model: model.clone(),
                        input_tokens: i64::from(input_tokens),
                        output_tokens: i64::from(output_tokens),
                        cache_creation_tokens: i64::from(cache_creation),
                        cache_read_tokens: i64::from(cache_read),
                        api_key_id: None,
                        provider,
                        custom_input_price,
                        custom_output_price,
                    })
                    .await
                    .ok();

                let manual_compaction = context_retry_compaction
                    .as_ref()
                    .or(pre_turn_compaction.as_ref());
                let compacted = manual_compaction
                    .map(|compaction| crate::events::CompactionRecord {
                        summary: compaction.summary.clone(),
                        removed_messages: compaction.removed_message_count,
                        count: 1,
                        trigger: Some(compaction.trigger.clone()),
                        strategy: Some(compaction.strategy.clone()),
                        summary_tokens: Some(compaction.summary_tokens),
                        retained_tail_tokens: Some(compaction.retained_tail_tokens),
                        provider_compaction_endpoint_supported: Some(
                            compaction.provider_compaction_endpoint_supported,
                        ),
                        provider_compaction_endpoint_called: Some(
                            compaction.provider_compaction_endpoint_called,
                        ),
                        provider_compaction_output_applied: Some(
                            compaction.provider_compaction_output_applied,
                        ),
                        provider_compaction_fallback_reason: compaction
                            .provider_compaction_fallback_reason
                            .clone(),
                    })
                    .or_else(|| {
                        turn_summary.auto_compaction.as_ref().map(|c| {
                            crate::events::CompactionRecord {
                                summary: format!("Compacted {} messages", c.removed_message_count),
                                removed_messages: c.removed_message_count,
                                count: 1,
                                trigger: Some("runtime_auto".to_string()),
                                strategy: None,
                                summary_tokens: None,
                                retained_tail_tokens: None,
                                provider_compaction_endpoint_supported: None,
                                provider_compaction_endpoint_called: None,
                                provider_compaction_output_applied: None,
                                provider_compaction_fallback_reason: None,
                            }
                        })
                    });
                let auto_archive_entries = turn_summary
                    .auto_compaction
                    .as_ref()
                    .map(|auto| build_compaction_archive_entries(&auto.archived_messages));
                let archive_entries = manual_compaction
                    .map(|manual| manual.archive_entries.as_slice())
                    .or_else(|| auto_archive_entries.as_deref());
                if let Some(compaction) = compacted.as_ref() {
                    record_turn_compaction(
                        self.config_registry.as_ref(),
                        &tenant_id,
                        &user_id,
                        &session_id_owned,
                        compaction,
                        archive_entries,
                    )
                    .await;
                }

                let usage_record = crate::events::TokenUsageRecord {
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens: cache_creation,
                    cache_read_tokens: cache_read,
                    total_tokens: input_tokens + output_tokens + cache_creation + cache_read,
                    estimated_cost_usd: cost,
                    model,
                };
                Ok(match suspended {
                    Some(suspended) => AgentTurnRunOutcome::Suspended(AgentSuspendedTurn {
                        session_id: session_id_owned,
                        runtime_turn_id: suspended.turn_id,
                        deferred_tools: suspended.deferred_tools,
                        partial_text: text,
                        tool_calls: tool_records,
                        usage: usage_record,
                        iterations: turn_summary.iterations,
                    }),
                    None => AgentTurnRunOutcome::Completed(TurnResult {
                        session_id: session_id_owned,
                        text,
                        thinking,
                        tool_calls: tool_records,
                        usage: usage_record,
                        compacted,
                        iterations: turn_summary.iterations,
                        metadata: Some(session_metadata),
                        hot_reloaded: hot_reloaded_this_turn,
                    }),
                })
            }
            Err(e) => {
                let _ = self
                    .config_registry
                    .record_token_usage(crate::TokenUsageParams {
                        tenant_id,
                        user_id,
                        session_id: Some(session_id_owned.clone()),
                        model: model.clone(),
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                        api_key_id: None,
                        provider,
                        custom_input_price,
                        custom_output_price,
                    })
                    .await
                    .ok();
                Err(e)
            }
        }
    }
    /// Rebuild the runtime for an existing session, picking up any changes to
    /// the tenant's MCP servers and Skills configuration that occurred since the
    /// session was created (or since the last hot-reload).
    ///
    /// This is called automatically by `run_turn` when a config version mismatch
    /// is detected, and can also be called manually.
    ///
    /// The session retains its existing conversation history and state — only the
    /// underlying `ConversationRuntime` (MCP tools + skill instructions) is rebuilt.
    pub async fn hot_reload_session(&self, session_id: &str) -> Result<()> {
        self.hot_reload_session_with_options(session_id, None, None)
            .await
    }

    /// Append an internal context note to an existing runtime session without
    /// making an LLM call.
    ///
    /// Code Studio uses this to sync candidate-worktree results back into the
    /// long-lived `rd_thread` session. The candidate runtime edits an isolated
    /// worktree and is destroyed afterwards; this keeps the persistent thread
    /// aware of the proposed diff without changing its repository cwd.
    pub async fn append_internal_context_message(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        text: impl Into<String>,
    ) -> Result<()> {
        let text = text.into();
        if text.trim().is_empty() {
            return Ok(());
        }

        self.get_owned_session(session_id, tenant_id, user_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;

        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(lock) = lock {
            lock
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };

        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Validation(
                "session ownership mismatch".to_string(),
            ));
        }
        if matches!(session.state, SessionState::Running) {
            return Err(GatewayError::SessionBusy);
        }

        let message = ConversationMessage {
            role: MessageRole::System,
            blocks: vec![RuntimeContentBlock::Text { text }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        };
        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let append_result = if let Some(runtime) = Arc::get_mut(&mut runtime_arc) {
            runtime
                .append_visible_message(message)
                .await
                .map_err(GatewayError::Runtime)
        } else {
            Err(GatewayError::RuntimeExecution(
                "runtime Arc is not uniquely owned".to_string(),
            ))
        };
        session.built.return_runtime(runtime_arc);
        append_result?;
        session.last_activity = Utc::now();
        Ok(())
    }

    /// Append a visible user/assistant message to an existing runtime session
    /// without making an LLM call.
    ///
    /// Super Assistant uses its own orchestration endpoint for routing across
    /// capabilities, but the UI still reloads history through the shared agent
    /// session history API. Persisting visible turns into the same runtime
    /// session keeps refresh/session-click recovery identical to normal chat.
    pub async fn append_visible_message(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        role: MessageRole,
        text: impl Into<String>,
    ) -> Result<()> {
        let text = text.into();
        if text.trim().is_empty() {
            return Ok(());
        }
        if !matches!(role, MessageRole::User | MessageRole::Assistant) {
            return Err(GatewayError::Validation(
                "visible message role must be user or assistant".to_string(),
            ));
        }

        self.get_owned_session(session_id, tenant_id, user_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;

        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(lock) = lock {
            lock
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };

        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if session.tenant_id != tenant_id || session.user_id != user_id {
            return Err(GatewayError::Validation(
                "session ownership mismatch".to_string(),
            ));
        }
        if matches!(session.state, SessionState::Running) {
            return Err(GatewayError::SessionBusy);
        }

        let message = match role {
            MessageRole::User => ConversationMessage::user_text(text),
            MessageRole::Assistant => {
                ConversationMessage::assistant(vec![RuntimeContentBlock::Text { text }])
            }
            _ => unreachable!("validated visible message role"),
        };
        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let append_result = if let Some(runtime) = Arc::get_mut(&mut runtime_arc) {
            runtime
                .append_visible_message(message)
                .await
                .map_err(GatewayError::Runtime)
        } else {
            Err(GatewayError::RuntimeExecution(
                "runtime Arc is not uniquely owned".to_string(),
            ))
        };
        session.built.return_runtime(runtime_arc);
        append_result?;
        session.last_activity = Utc::now();
        Ok(())
    }

    /// Append a background task's terminal message after any foreground turn
    /// releases the parent session. Unlike [`append_visible_message`], which is
    /// intentionally fail-fast for interactive requests, this bounded retry
    /// prevents async PM/NL2SQL/adversarial results from being lost merely
    /// because a follow-up turn briefly owns the session lock.
    pub async fn append_visible_message_when_idle(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
        role: MessageRole,
        text: impl Into<String>,
        max_wait: Duration,
    ) -> Result<()> {
        let text = text.into();
        let deadline = Instant::now() + max_wait;
        loop {
            match self
                .append_visible_message(session_id, tenant_id, user_id, role, text.clone())
                .await
            {
                Ok(()) => return Ok(()),
                Err(GatewayError::SessionBusy) if Instant::now() < deadline => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    sleep(remaining.min(Duration::from_millis(250))).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Rebuild a session while explicitly overriding its allowed tool list.
    ///
    /// RD Code Studio uses this when reusing a persistent coding runtime: the
    /// conversation history should continue, but the current Agent Profile's
    /// `allowedTools` must still be enforced after restore/hot-reload.
    pub async fn hot_reload_session_with_allowed_tools(
        &self,
        session_id: &str,
        allowed_tools_override: Option<Vec<String>>,
    ) -> Result<()> {
        self.hot_reload_session_with_options(session_id, None, allowed_tools_override)
            .await
    }

    /// Rebuild a session with optional runtime overrides.
    ///
    /// RD Code Studio can reuse a persistent thread runtime across tasks. When the
    /// user changes the selected model/API key, reuse must still rebuild the
    /// underlying runtime with the currently requested model rather than keeping
    /// the old client alive.
    pub async fn hot_reload_session_with_options(
        &self,
        session_id: &str,
        model_override: Option<&str>,
        allowed_tools_override: Option<Vec<String>>,
    ) -> Result<()> {
        tracing::debug!(
            "session_manager::hot_reload_session: rebuilding runtime for session {}",
            session_id
        );

        let (tenant_id, user_id, workspace, current_model, model_pinned, source) = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
            (
                session.tenant_id.clone(),
                session.user_id.clone(),
                session.workspace.clone(),
                session.model.clone(),
                session.model_pinned,
                session.source.clone(),
            )
        };
        let requested_model = model_override
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let effective_model_pinned = model_pinned || requested_model.is_some();

        // Reload MCP servers from DB (this also increments the config version)
        self.reload_mcp_servers(&tenant_id, &user_id).await?;

        // Update the session's config_version to match the global counter
        let new_version = {
            let guard = self.mcp_config_version.read().await;
            *guard.get(&tenant_id).unwrap_or(&0)
        };

        // Get the current API keys version from the DB (this may have changed if an
        // operator added/updated/deleted a key from the WebUI).
        let new_keys_version = self.config_registry.get_api_keys_version(&tenant_id).await;

        // Rebuild the runtime with fresh MCP tools and the latest API key set.
        // Use the session source as the runtime scenario. In this codebase the source
        // is also the runtime scope tag (e.g. "chat", "agent", "pm"), so this keeps
        // PM sessions on PM-scoped keys and preserves Chat parity after hot reload.
        let new_tenant_mcp = self.get_or_init_mcp_manager(&tenant_id).await;
        let mut config = self
            .config_registry
            .load_user_config(
                &tenant_id,
                &user_id,
                Some(runtime_scenario_from_source(source.as_str())),
            )
            .await
            .map_err(GatewayError::Database)?;

        apply_model_override(
            &mut config,
            reload_model_override(requested_model, &current_model, model_pinned),
        );
        if let Some(allowed_tools) = allowed_tools_override {
            config.allowed_tools = Some(allowed_tools);
        }
        let first_key = config.api_keys.first();
        let custom_input_price = first_key.and_then(|k| k.input_price_per_million);
        let custom_output_price = first_key.and_then(|k| k.output_price_per_million);

        let durable_session = load_durable_ledger_session(
            &self.config_registry.database(),
            session_id,
            &tenant_id,
            &user_id,
        )
        .await?
        .unwrap_or_else(|| {
            let mut session = runtime::Session::new();
            session.session_id = session_id.to_string();
            session.tenant_id = Some(tenant_id.clone());
            session.user_id = Some(user_id.clone());
            session
        });
        let builder = self.attach_compaction_hook_factory(
            RuntimeBuilder::restore(
                config,
                workspace.clone(),
                session_id.to_string(),
                new_tenant_mcp,
            )
            .with_restored_session(durable_session),
        );
        let new_built = builder.build().await?;
        let new_model = new_built.model.clone();
        let new_provider = new_built.provider.clone();

        self.config_registry
            .update_session_runtime_selection(
                session_id,
                &new_model,
                effective_model_pinned,
                &new_provider,
            )
            .await
            .map_err(GatewayError::Database)?;

        let session_path = workspace
            .join(".aos")
            .join("sessions")
            .join(format!("{session_id}.jsonl"));
        tokio::fs::create_dir_all(session_path.parent().unwrap_or(&workspace))
            .await
            .map_err(GatewayError::Io)?;

        // Update the session in-place, preserving all conversation history
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;

        // Replace the runtime while preserving session state
        session.built = new_built;
        session.model = new_model;
        session.model_pinned = effective_model_pinned;
        session.provider = new_provider;
        session.config_version = new_version;
        session.api_keys_version = new_keys_version;
        session.session_metadata = session.built.session_metadata.clone();
        session.custom_input_price = custom_input_price;
        session.custom_output_price = custom_output_price;
        session.last_activity = Utc::now();

        tracing::debug!(
            session_id,
            new_version,
            new_keys_version,
            "session hot-reloaded"
        );

        Ok(())
    }

    /// Destroy a session and clean up resources.
    ///
    /// Attempts to remove the session from both the in-memory map and the DB.
    /// If the session is not in the in-memory map (e.g. it was only persisted
    /// to the DB from a previous server run), we still try to mark it as
    /// `completed` in the DB so that `list_user_sessions` excludes it.
    pub async fn destroy_session(&self, session_id: &str) -> Result<()> {
        let persisted_workspace = self
            .config_registry
            .get_agent_session_record(session_id)
            .await
            .ok()
            .flatten()
            .map(|record| PathBuf::from(record.workspace_path));
        // Capture the workspace path before removing from the map so we can delete the file.
        let session_workspace = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_id).map(|s| s.workspace)
        };

        let session_existed_in_memory = session_workspace.is_some();

        // Clean up the per-session lock entry to avoid unbounded growth.
        {
            let mut locks = self.session_locks.write().await;
            locks.remove(session_id);
        }

        if session_existed_in_memory {
            tracing::info!(session_id, "agent session destroyed (in-memory)");
        }

        // Always try to mark the session as completed in the DB, regardless of
        // whether it was in the in-memory map. This ensures the session is
        // excluded from list_sessions even when it was only persisted in the DB.
        match self.config_registry.finish_session(session_id).await {
            Ok(()) => {
                tracing::info!(session_id, "agent session marked completed in DB");
            }
            Err(e) => {
                tracing::error!(
                    session_id,
                    ?e,
                    "failed to mark agent session as completed in DB"
                );
                return Err(GatewayError::Internal(format!(
                    "failed to delete session from database: {e}"
                )));
            }
        }

        // Physically delete the diagnostic JSONL export asynchronously. The
        // registry workspace is sufficient; recovery never scans this file.
        let file_to_delete: Option<PathBuf> = session_workspace
            .or(persisted_workspace)
            .map(|ws| ws.join(".aos/sessions").join(format!("{session_id}.jsonl")));
        if let Some(path) = file_to_delete {
            let sid = session_id.to_string();
            tokio::spawn(async move {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {
                        tracing::info!(
                            session_id = sid,
                            "session file deleted: {}",
                            path.display()
                        );
                        // Prune the sessions directory if it became empty.
                        if let Some(sessions_dir) = path.parent() {
                            if let Ok(mut entries) = tokio::fs::read_dir(sessions_dir).await {
                                let mut empty = true;
                                #[allow(clippy::never_loop)]
                                while let Ok(Some(_)) = entries.next_entry().await {
                                    empty = false;
                                    break;
                                }
                                if empty {
                                    let _ = tokio::fs::remove_dir(sessions_dir).await;
                                    tracing::debug!(dir = %sessions_dir.display(), "sessions dir empty, removed");
                                    // Also remove the parent .aos dir if now empty.
                                    if let Some(parent) = sessions_dir.parent() {
                                        let _ = tokio::fs::remove_dir(parent).await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::debug!(session_id = sid, "session file already absent");
                    }
                    Err(e) => {
                        tracing::warn!(session_id = sid, ?e, "failed to delete session file");
                    }
                }
            });
        }

        Ok(())
    }

    /// List sessions for a user from the DB, merged with in-memory active sessions.
    /// DB provides name, model, `is_pinned`, source, `created_at`, `updated_at`.
    /// In-memory map provides current state and `last_activity` for active sessions.
    pub async fn list_user_sessions(
        &self,
        tenant_id: &str,
        user_id: &str,
        source_filter: Option<&str>,
    ) -> Vec<SessionInfo> {
        // Fetch from DB (source of truth for persisted data).
        let db_sessions = match self
            .config_registry
            .list_agent_sessions(tenant_id, user_id, source_filter)
            .await
        {
            Ok(sessions) => sessions,
            Err(e) => {
                tracing::warn!(
                    tenant_id,
                    user_id,
                    ?e,
                    "failed to list sessions from DB, falling back to in-memory"
                );
                // Fallback to in-memory only.
                let sessions = self.sessions.read().await;
                let mut result: Vec<SessionInfo> = sessions
                    .values()
                    .filter(|s| s.user_id == user_id && s.tenant_id == tenant_id)
                    .filter(|s| source_filter.is_none_or(|f| s.source == f))
                    .map(|s| SessionInfo {
                        session_id: s.session_id.clone(),
                        name: s.name.clone(),
                        state: s.state,
                        model: s.model.clone(),
                        workspace: s.workspace.clone(),
                        created_at: s.created_at,
                        last_activity: s.last_activity,
                        is_pinned: s.is_pinned,
                        is_bookmarked: s.is_bookmarked,
                        source: s.source.clone(),
                        session_metadata: s.session_metadata.clone(),
                    })
                    .collect();
                result.sort_by(|a, b| {
                    b.is_pinned
                        .cmp(&a.is_pinned)
                        .then_with(|| b.last_activity.cmp(&a.last_activity))
                });
                return result;
            }
        };

        // Get current state from in-memory map for active sessions. Clone the
        // small pieces we need before awaiting metadata fallbacks so we never
        // hold the sessions lock across an await point.
        let sessions = self.sessions.read().await;
        let mut rows = Vec::with_capacity(db_sessions.len());
        for r in db_sessions {
            let in_memory = sessions
                .get(&r.session_id)
                .map(|s| (s.state, s.last_activity, s.session_metadata.clone()));
            rows.push((r, in_memory));
        }
        drop(sessions);

        let mut result = Vec::with_capacity(rows.len());
        let mut metadata_cache: HashMap<String, crate::events::SessionMetadata> = HashMap::new();
        for (r, in_memory) in rows {
            let (state, last_activity, session_metadata) =
                if let Some((state, last_activity, metadata)) = in_memory {
                    (state, last_activity, metadata)
                } else {
                    let state = match r.state.as_str() {
                        "running" => SessionState::Running,
                        "paused" => SessionState::Paused,
                        "completed" => SessionState::Completed,
                        "failed" => SessionState::Failed,
                        _ => SessionState::Idle,
                    };
                    (
                        state,
                        r.updated_at,
                        self.current_session_metadata_for_record(&r, &mut metadata_cache)
                            .await,
                    )
                };

            result.push(SessionInfo {
                session_id: r.session_id,
                name: r.name.unwrap_or_default(),
                state,
                model: r.model,
                workspace: PathBuf::new(), // not needed by list endpoint
                created_at: r.created_at,
                last_activity,
                is_pinned: r.is_pinned,
                is_bookmarked: r.is_bookmarked,
                source: r.source,
                session_metadata,
            });
        }
        result
    }

    async fn current_session_metadata_for_record(
        &self,
        record: &crate::config_registry::AgentSessionRecord,
        cache: &mut HashMap<String, crate::events::SessionMetadata>,
    ) -> crate::events::SessionMetadata {
        let scenario = runtime_scenario_from_source(record.source.as_str()).to_string();
        if let Some(metadata) = cache.get(&scenario) {
            let mut metadata = metadata.clone();
            if metadata.model.is_empty() {
                metadata.model = record.model.clone();
            }
            return metadata;
        }
        match self
            .config_registry
            .load_user_config(&record.tenant_id, &record.user_id, Some(&scenario))
            .await
        {
            Ok(config) => {
                let mut metadata = session_metadata_from_config(&config, &record.model);
                cache.insert(scenario, metadata.clone());
                if metadata.model.is_empty() {
                    metadata.model = record.model.clone();
                }
                metadata
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %record.session_id,
                    tenant_id = %record.tenant_id,
                    user_id = %record.user_id,
                    ?error,
                    "failed to load current session metadata for session list"
                );
                crate::events::SessionMetadata {
                    mcp_servers: Vec::new(),
                    skills: Vec::new(),
                    permission_mode: String::new(),
                    model: record.model.clone(),
                }
            }
        }
    }

    /// Get a session's current state.
    pub async fn get_session_state(&self, session_id: &str) -> Option<SessionState> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|s| s.state)
    }

    /// Request cancellation for the currently running streaming turn.
    ///
    /// Returns `Ok(true)` when a live turn accepted the cancel signal and
    /// `Ok(false)` when the session exists but no cancellable turn is running.
    pub async fn cancel_running_turn(&self, session_id: &str) -> Result<bool> {
        {
            let sessions = self.sessions.read().await;
            if !sessions.contains_key(session_id) {
                return Err(GatewayError::SessionNotFound(session_id.to_string()));
            }
        }

        let cancel = {
            let mut cancels = self.running_turn_cancels.write().await;
            cancels.remove(session_id)
        };
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
            tracing::info!(session_id, "running session turn cancellation requested");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Close turn admission and wait for in-flight work to converge. Once the
    /// graceful budget expires, every registered cooperative cancel handle is
    /// fired and the manager waits once more for the cancellation checkpoints.
    /// A `false` result means the caller must preserve its unclean-shutdown marker.
    pub async fn shutdown_gracefully(
        &self,
        graceful_timeout: Duration,
        cancellation_timeout: Duration,
    ) -> bool {
        self.turn_admission
            .accepting
            .store(false, Ordering::Release);
        if self.turn_admission.wait_until_idle(graceful_timeout).await {
            return true;
        }

        let cancels = {
            let mut running = self.running_turn_cancels.write().await;
            running
                .drain()
                .map(|(_, cancel)| cancel)
                .collect::<Vec<_>>()
        };
        let cancelled_count = cancels.len();
        for cancel in cancels {
            let _ = cancel.send(());
        }
        tracing::warn!(
            active_turns = self.turn_admission.active.load(Ordering::Acquire),
            cancelled_count,
            "agent turn shutdown grace expired; requested cooperative cancellation"
        );
        self.turn_admission
            .wait_until_idle(cancellation_timeout)
            .await
    }

    /// Stop accepting new model turns immediately. This is split from the wait
    /// method so the HTTP acceptor and the agent admission gate close together.
    pub fn begin_shutdown(&self) {
        self.turn_admission
            .accepting
            .store(false, Ordering::Release);
    }

    #[must_use]
    pub fn active_turn_count(&self) -> usize {
        self.turn_admission.active.load(Ordering::Acquire)
    }

    /// Return token-aware context status for a session without mutating it.
    pub async fn context_status(&self, session_id: &str) -> Result<SessionContextStatus> {
        self.get_session(session_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;

        let (model, provider, state, snapshot, context_window_tokens) = {
            let sessions = self.sessions.read().await;
            let session = sessions
                .get(session_id)
                .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
            (
                session.model.clone(),
                session.provider.clone(),
                session.state,
                session
                    .built
                    .try_session_snapshot()
                    .or_else(|| {
                        runtime::Session::load_from_path(&session.built.persistence_path()).ok()
                    })
                    .unwrap_or_default(),
                session.built.context_window_tokens,
            )
        };

        let token_options = token_estimate_options_for_runtime(&provider, &model);
        let estimated_tokens =
            runtime::estimate_session_tokens_with_options(&snapshot, token_options);
        let (context_window, unknown_context_window) =
            resolve_context_window(&model, context_window_tokens);
        let effective_context_limit = context_window.saturating_mul(80) / 100;
        let auto_compact_token_limit = effective_context_limit;
        let context_usage_percent = if effective_context_limit == 0 {
            0.0
        } else {
            (estimated_tokens as f64 / effective_context_limit as f64 * 100.0).min(999.0)
        };
        let tokens_until_compaction = auto_compact_token_limit as isize - estimated_tokens as isize;
        let (compaction_count, last_compaction_summary, last_compaction_removed_messages) =
            snapshot
                .compaction
                .as_ref()
                .map_or((0, None, 0), |compaction| {
                    (
                        compaction.count,
                        Some(compaction.summary.clone()),
                        compaction.removed_message_count,
                    )
                });

        Ok(SessionContextStatus {
            session_id: session_id.to_string(),
            model,
            provider,
            message_count: snapshot.messages.len(),
            estimated_tokens,
            token_estimator: token_options.tokenizer.label().to_string(),
            context_window,
            effective_context_limit,
            auto_compact_token_limit,
            context_usage_percent,
            tokens_until_compaction,
            compaction_count,
            last_compaction_summary,
            last_compaction_removed_messages,
            unknown_context_window,
            state,
        })
    }

    /// Compact a session using the current runtime deterministic/Trident fallback path.
    pub async fn compact_session_manual(&self, session_id: &str) -> Result<ManualCompactionResult> {
        self.get_session(session_id)
            .await
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;

        let lock = {
            let locks = self.session_locks.read().await;
            locks.get(session_id).cloned()
        };
        let lock = if let Some(lock) = lock {
            lock
        } else {
            let mut locks = self.session_locks.write().await;
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            return Err(GatewayError::SessionBusy);
        };

        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| GatewayError::SessionNotFound(session_id.to_string()))?;
        if matches!(session.state, SessionState::Running) {
            return Err(GatewayError::SessionBusy);
        }

        let mut runtime_arc = session.built.take_runtime_arc().ok_or_else(|| {
            GatewayError::RuntimeExecution("session runtime unavailable".to_string())
        })?;
        let supports_native_compaction = session.built.supports_native_compaction;
        let native_compaction_bridge = session.built.native_compaction_bridge.clone();
        let context_window_tokens = session.built.context_window_tokens;
        let result = if let Some(runtime) = Arc::get_mut(&mut runtime_arc) {
            compact_runtime_for_continuity(
                runtime,
                session_id,
                &session.provider,
                &session.model,
                context_window_tokens,
                "manual",
                supports_native_compaction,
                native_compaction_bridge.as_ref(),
            )
            .await?
            .ok_or_else(|| {
                GatewayError::RuntimeExecution(
                    "session does not have enough history to compact".to_string(),
                )
            })
        } else {
            Err(GatewayError::RuntimeExecution(
                "runtime Arc is not uniquely owned".to_string(),
            ))
        };
        session.built.return_runtime(runtime_arc);
        session.last_activity = Utc::now();
        result
    }

    /// Rename a session.
    pub async fn rename_session(&self, session_id: &str, new_name: &str) -> Result<()> {
        // Update in-memory session if it exists.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.name = new_name.to_string();
                tracing::info!(session_id, new_name, "session renamed (in-memory)");
            }
        }
        // Persist name to DB so it survives server restarts and works for DB-only sessions.
        self.config_registry
            .update_session_name(session_id, new_name)
            .await
            .map_err(|_| GatewayError::SessionNotFound(session_id.to_string()))?;
        tracing::info!(session_id, new_name, "session renamed (persisted to DB)");
        Ok(())
    }

    /// Toggle the pinned state of a session.
    pub async fn toggle_pin_session(&self, session_id: &str) -> Result<bool> {
        let new_state: bool;
        // Update in-memory session if it exists.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.is_pinned = !session.is_pinned;
                new_state = session.is_pinned;
                tracing::debug!(
                    session_id,
                    is_pinned = new_state,
                    "session pin toggled (in-memory)"
                );
            } else {
                // Fall back to DB to determine current state.
                let current = self
                    .config_registry
                    .get_session_pin(session_id)
                    .await
                    .map_err(|_| GatewayError::SessionNotFound(session_id.to_string()))?;
                new_state = !current;
            }
        }
        // Persist to DB so pin state survives server restarts and works for DB-only sessions.
        self.config_registry
            .update_session_pin(session_id, new_state)
            .await
            .map_err(|_| GatewayError::SessionNotFound(session_id.to_string()))?;
        tracing::debug!(
            session_id,
            is_pinned = new_state,
            "session pin persisted to DB"
        );
        Ok(new_state)
    }

    /// Toggle the bookmark state of a session.
    pub async fn toggle_bookmark_session(&self, session_id: &str) -> Result<bool> {
        let new_state: bool;
        // Update in-memory session if it exists.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.is_bookmarked = !session.is_bookmarked;
                new_state = session.is_bookmarked;
                tracing::debug!(
                    session_id,
                    is_bookmarked = new_state,
                    "session bookmark toggled (in-memory)"
                );
            } else {
                // Fall back to DB to determine current state.
                let current = self
                    .config_registry
                    .get_session_bookmark(session_id)
                    .await
                    .map_err(|_| GatewayError::SessionNotFound(session_id.to_string()))?;
                new_state = !current;
            }
        }
        // Persist to DB.
        self.config_registry
            .toggle_session_bookmark(session_id)
            .await
            .map_err(|_| GatewayError::SessionNotFound(session_id.to_string()))?;
        tracing::debug!(
            session_id,
            is_bookmarked = new_state,
            "session bookmark persisted to DB"
        );
        Ok(new_state)
    }

    /// Get session handle by ID.
    ///
    /// **Fallback chain**:
    /// 1. In-memory sessions map (active sessions)
    /// 2. Database lookup + lazy restoration (inactive sessions from a previous server run)
    ///
    /// When a session is found in the DB but not in memory, it is lazily restored
    /// from the durable Agent Ledger into a fresh runtime using the user's
    /// current config.
    pub async fn get_session(&self, session_id: &str) -> Option<SessionHandle> {
        self.get_session_scoped(session_id, None, None).await
    }

    /// Resolve and lazily restore a session only when it belongs to the
    /// authenticated tenant and user. Ownership is checked before a DB-backed
    /// runtime is restored, so an opaque session id is never treated as
    /// authorization and cannot be used to load another user's workspace.
    pub async fn get_owned_session(
        &self,
        session_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<SessionHandle> {
        self.get_session_scoped(session_id, Some(tenant_id), Some(user_id))
            .await
    }

    async fn get_session_scoped(
        &self,
        session_id: &str,
        tenant_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Option<SessionHandle> {
        // Fast path: in-memory active session.
        {
            let sessions = self.sessions.read().await;
            if let Some(s) = sessions.get(session_id) {
                if !session_owner_matches(&s.tenant_id, &s.user_id, tenant_id, user_id) {
                    return None;
                }
                return Some(SessionHandle {
                    session_id: s.session_id.clone(),
                    name: s.name.clone(),
                    user_id: s.user_id.clone(),
                    tenant_id: s.tenant_id.clone(),
                    workspace: s.workspace.clone(),
                    model: s.model.clone(),
                    created_at: s.created_at,
                    session_metadata: s.session_metadata.clone(),
                    is_pinned: s.is_pinned,
                    is_bookmarked: s.is_bookmarked,
                    source: s.source.clone(),
                });
            }
        }

        // Slow path: look up in database and lazily restore.
        let Ok(Some(rec)) = self
            .config_registry
            .get_agent_session_record(session_id)
            .await
        else {
            return None;
        };
        if !session_owner_matches(&rec.tenant_id, &rec.user_id, tenant_id, user_id) {
            return None;
        }

        // Restore the session: build a runtime for this session.
        match self.restore_session(&rec).await {
            Ok(session) => {
                let handle = SessionHandle {
                    session_id: session.session_id.clone(),
                    name: session.name.clone(),
                    user_id: session.user_id.clone(),
                    tenant_id: session.tenant_id.clone(),
                    workspace: session.workspace.clone(),
                    model: session.model.clone(),
                    created_at: session.created_at,
                    session_metadata: session.session_metadata.clone(),
                    is_pinned: session.is_pinned,
                    is_bookmarked: session.is_bookmarked,
                    source: session.source.clone(),
                };
                // Store in memory for subsequent fast-path lookups.
                self.sessions
                    .write()
                    .await
                    .insert(session_id.to_string(), session);
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(
                    session_id,
                    ?e,
                    "failed to restore session from DB, returning None"
                );
                None
            }
        }
    }

    /// Restore an inactive session from the database.
    ///
    /// Loads conversation state from the durable Agent Ledger and builds a
    /// fresh runtime with the user's current config.
    async fn restore_session(
        &self,
        rec: &crate::config_registry::AgentSessionRecord,
    ) -> Result<AgentSession> {
        // Load the user's current runtime config (includes API keys, MCP servers, skills).
        let mut config = self
            .config_registry
            .load_user_config(
                &rec.tenant_id,
                &rec.user_id,
                Some(runtime_scenario_from_source(rec.source.as_str())),
            )
            .await
            .map_err(GatewayError::Database)?;
        apply_model_override(&mut config, rec.model_pinned.then_some(rec.model.as_str()));
        let first_key = config.api_keys.first();
        let custom_input_price = first_key.and_then(|key| key.input_price_per_million);
        let custom_output_price = first_key.and_then(|key| key.output_price_per_million);

        let workspace = PathBuf::from(&rec.workspace_path);

        // Get or initialize the MCP manager for this tenant.
        let tenant_mcp = self.get_or_init_mcp_manager(&rec.tenant_id).await;

        // Populate the MCP manager with servers from the config.
        // This is required because:
        // 1. After a server restart, in-memory MCP managers are lost.
        // 2. Even if a manager exists, servers may have been reconfigured since it was built.
        // Without this, restored sessions have no MCP servers at all.
        self.init_mcp_manager_from_config(&rec.tenant_id, &config)
            .await?;

        // The execution ledger is the sole recovery authority. JSONL may be
        // emitted as a diagnostic export, but it is never read to reconstruct
        // a production runtime.
        let durable_session = load_durable_ledger_session(
            &self.config_registry.database(),
            &rec.session_id,
            &rec.tenant_id,
            &rec.user_id,
        )
        .await?
        .unwrap_or_else(|| {
            let mut session = runtime::Session::new();
            session.session_id.clone_from(&rec.session_id);
            session.tenant_id = Some(rec.tenant_id.clone());
            session.user_id = Some(rec.user_id.clone());
            session
        });
        let builder = RuntimeBuilder::restore(
            config,
            workspace.clone(),
            rec.session_id.clone(),
            tenant_mcp,
        )
        .with_restored_session(durable_session);
        let builder = self.attach_compaction_hook_factory(builder);
        let built = builder.build().await?;

        self.config_registry
            .update_session_runtime_selection(
                &rec.session_id,
                &built.model,
                rec.model_pinned,
                &built.provider,
            )
            .await
            .map_err(GatewayError::Database)?;

        let now = Utc::now();
        let session_metadata = built.session_metadata.clone();

        Ok(AgentSession {
            session_id: rec.session_id.clone(),
            name: rec.name.clone().unwrap_or_else(|| {
                format!("Session {}", chrono::Local::now().format("%m-%d %H:%M"))
            }),
            user_id: rec.user_id.clone(),
            tenant_id: rec.tenant_id.clone(),
            source: rec.source.clone(),
            workspace: workspace.clone(),
            state: SessionState::Idle,
            model: built.model.clone(),
            model_pinned: rec.model_pinned,
            provider: built.provider.clone(),
            custom_input_price,
            custom_output_price,
            created_at: rec.created_at,
            last_activity: now,
            built,
            session_metadata,
            config_version: 0,
            api_keys_version: self
                .config_registry
                .get_api_keys_version(&rec.tenant_id)
                .await,
            hot_reloaded_this_turn: false,
            is_pinned: rec.is_pinned,
            is_bookmarked: rec.is_bookmarked,
        })
    }

    /// Get the authoritative session projection from the durable Agent Ledger.
    /// Ownership is validated against the session registry before any ledger
    /// row is read. JSONL is deliberately excluded from this path.
    pub async fn get_session_messages(
        &self,
        session_id: &str,
        tenant_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Option<runtime::Session> {
        let record = self
            .config_registry
            .get_agent_session_record(session_id)
            .await
            .ok()
            .flatten()?;
        if !session_owner_matches(&record.tenant_id, &record.user_id, tenant_id, user_id) {
            return None;
        }
        match load_durable_ledger_session(
            &self.config_registry.database(),
            session_id,
            &record.tenant_id,
            &record.user_id,
        )
        .await
        {
            Ok(Some(session)) => Some(session),
            Ok(None) => {
                if let Some(snapshot) = self
                    .sessions
                    .read()
                    .await
                    .get(session_id)
                    .and_then(|session| session.built.try_session_snapshot())
                {
                    return Some(snapshot);
                }
                let mut session = runtime::Session::new();
                session.session_id = session_id.to_string();
                session.tenant_id = Some(record.tenant_id);
                session.user_id = Some(record.user_id);
                Some(session)
            }
            Err(error) => {
                tracing::error!(
                    session_id,
                    tenant_id = record.tenant_id,
                    error = %error,
                    "durable execution ledger is corrupt; refusing recovery"
                );
                None
            }
        }
    }

    /// Returns a reference to the per-tenant MCP managers map.
    /// Each tenant gets its own isolated manager to prevent cross-tenant state leakage.
    #[must_use]
    pub fn mcp_managers(
        &self,
    ) -> Arc<RwLock<HashMap<String, Arc<RwLock<McpServerSessionManager>>>>> {
        Arc::clone(&self.mcp_managers)
    }

    /// Returns the tenant config registry for resolving API keys with failover support.
    #[must_use]
    pub fn config_registry(&self) -> Arc<TenantConfigRegistry> {
        Arc::clone(&self.config_registry)
    }

    /// Discover currently configured MCP tools that look suitable for public
    /// search/browser/fetch retrieval. This is intentionally narrow so
    /// menu-level search orchestration can reuse MCP without exposing arbitrary
    /// tool execution.
    pub async fn discover_search_like_mcp_tools(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Vec<McpSearchToolCandidate>> {
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let needs_init = {
            let guard = tenant_mcp.read().await;
            guard.is_empty()
        };
        if needs_init {
            let config = self
                .config_registry
                .load_user_config(tenant_id, user_id, Some("chat"))
                .await
                .map_err(GatewayError::Database)?;
            self.init_mcp_manager_from_config(tenant_id, &config)
                .await?;
        }
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let mut guard = tenant_mcp.write().await;
        let (tools, failures) = guard.discover_all_tools().await;
        for (server_name, error) in failures {
            tracing::warn!(
                tenant_id,
                server_name,
                error = %error,
                "MCP search-like discovery skipped an unhealthy server"
            );
        }
        Ok(tools
            .into_iter()
            .filter(is_search_like_mcp_tool)
            .map(|tool| McpSearchToolCandidate {
                server_name: mcp_server_name_from_qualified_tool(&tool.name),
                qualified_name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect())
    }

    /// Execute the first search/browser/fetch-like MCP tool that accepts a
    /// generic query. Returns `Ok(None)` when MCP is configured but no suitable
    /// tool is available or every candidate rejects the inferred arguments.
    pub async fn execute_search_like_mcp_tool(
        &self,
        tenant_id: &str,
        user_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<Option<McpSearchToolExecution>> {
        let candidates = self
            .discover_search_like_mcp_tools(tenant_id, user_id)
            .await?;
        if candidates.is_empty() {
            return Ok(None);
        }
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let mut guard = tenant_mcp.write().await;
        for candidate in candidates {
            let Some(arguments) = mcp_search_tool_arguments(&candidate, query, max_results) else {
                continue;
            };
            match guard
                .call_tool(&candidate.qualified_name, Some(arguments.clone()))
                .await
            {
                Ok(result) => {
                    let output = serde_json::to_value(result).unwrap_or_else(|error| {
                        serde_json::json!({
                            "serializationError": error.to_string(),
                        })
                    });
                    return Ok(Some(McpSearchToolExecution {
                        qualified_name: candidate.qualified_name,
                        server_name: candidate.server_name,
                        arguments,
                        output,
                    }));
                }
                Err(error) => {
                    tracing::warn!(
                        tenant_id,
                        tool = %candidate.qualified_name,
                        error = %error,
                        "MCP search-like tool execution failed; trying next candidate"
                    );
                }
            }
        }
        Ok(None)
    }

    /// Get or lazily initialize the MCP manager for a specific tenant.
    /// This is safe for concurrent use: if two sessions for the same tenant are
    /// created simultaneously, only one manager will be inserted.
    async fn get_or_init_mcp_manager(
        &self,
        tenant_id: &str,
    ) -> Arc<RwLock<McpServerSessionManager>> {
        // Fast path: check if already exists
        {
            let guard = self.mcp_managers.read().await;
            if let Some(mgr) = guard.get(tenant_id) {
                return Arc::clone(mgr);
            }
        }

        // Slow path: initialize
        let new_manager = Arc::new(RwLock::new(McpServerSessionManager::new()));
        let mut guard = self.mcp_managers.write().await;
        // Double-check after acquiring write lock
        if let Some(existing) = guard.get(tenant_id) {
            return Arc::clone(existing);
        }
        guard.insert(tenant_id.to_string(), Arc::clone(&new_manager));
        new_manager
    }

    /// Initialize the tenant's MCP manager with servers from a pre-loaded config.
    /// Populates the manager's session list so that `discover_all_tools()` finds them.
    async fn init_mcp_manager_from_config(
        &self,
        tenant_id: &str,
        config: &UserRuntimeConfig,
    ) -> Result<()> {
        let tenant_mcp = self.get_or_init_mcp_manager(tenant_id).await;
        let mut guard = tenant_mcp.write().await;

        // Clear all existing sessions first so that servers that were disabled
        // or deleted are removed from the manager. Without this, a disabled server
        // remains in the sessions map and can still be called (since the manager
        // is shared across all sessions for the same tenant).
        guard.shutdown_all().await;

        tracing::info!(
            "init_mcp_manager: {} server entries in config for tenant '{}'",
            config.mcp_servers.len(),
            tenant_id,
        );
        for entry in &config.mcp_servers {
            tracing::info!(
                "  entry: name={} transport={} enabled={} url={:?}",
                entry.name,
                entry.transport,
                entry.enabled,
                entry.url.as_deref().unwrap_or("(none)"),
            );
            if !entry.enabled {
                continue;
            }

            let build_result: std::result::Result<runtime::mcp::McpServerSession, String> =
                match entry.transport.as_str() {
                    "stdio" => {
                        let cmd = entry.command.clone().unwrap_or_default();
                        let mut builder =
                            runtime::mcp::StdioTransportBuilder::new(&entry.name, &cmd)
                                .args(entry.args.clone())
                                .env(entry.env.clone());
                        if let Some(ms) = entry.timeout_ms {
                            builder = builder.timeout_ms(u64::from(ms));
                        }
                        builder.build().map_err(|e| {
                            format!("failed to build stdio session for '{}': {}", entry.name, e)
                        })
                    }
                    "http" | "sse" => {
                        use std::collections::BTreeMap;
                        let mut headers: BTreeMap<String, String> = BTreeMap::new();

                        if let Some(serde_json::Value::Object(map)) = &entry.extra_headers {
                            for (k, v) in map {
                                if let Some(s) = v.as_str() {
                                    headers.insert(k.clone(), s.to_string());
                                }
                            }
                        }

                        if entry.auth_type == "bearer_token" {
                            // Authorization is set via HttpStreamTransportBuilder::bearer_token() below.
                            // We intentionally do NOT add it to extra_headers here to avoid
                            // duplication ("Bearer Bearer ...").
                        }

                        let mut builder = runtime::mcp::HttpStreamTransportBuilder::new(
                            &entry.name,
                            entry.url.as_deref().unwrap_or_default(),
                        )
                        .headers(headers)
                        .auto_detect_sse_fallback(true);

                        if let Some(ms) = entry.timeout_ms {
                            builder = builder.timeout_ms(u64::from(ms));
                        }

                        if entry.auth_type == "bearer_token" {
                            if let Some(token) = &entry.auth_token {
                                builder = builder.bearer_token(token.clone());
                            }
                        }

                        builder.build().map_err(|e| {
                            format!("failed to build HTTP session for '{}': {}", entry.name, e)
                        })
                    }
                    other => {
                        tracing::warn!(
                            "init_mcp_manager: transport '{other}' for '{}' not supported (only stdio/http/sse), skipping",
                            entry.name
                        );
                        continue;
                    }
                };

            match build_result {
                Ok(session) => {
                    let name = entry.name.clone();
                    tracing::info!(
                        "init_mcp_manager: registered server '{}' (transport={}); handshake is verified during tool discovery",
                        name,
                        entry.transport
                    );
                    guard.add_session(session);
                }
                Err(e) => {
                    tracing::warn!("init_mcp_manager: FAILED to build '{}': {}", entry.name, e);
                }
            }
        }

        tracing::info!(
            "init_mcp_manager: {} servers registered for tenant '{}'",
            guard.len(),
            tenant_id
        );

        // Log all registered server names for traceability
        let registered_names: Vec<String> = guard.server_names().map(String::from).collect();
        tracing::info!(
            "init_mcp_manager: registered servers after init: {:?}",
            registered_names,
        );
        Ok(())
    }

    /// Reload MCP server config from the database for a given tenant/user.
    /// This invalidates the config cache, reloads from DB, and updates the manager.
    /// Used for hot-reload when operators change MCP server config from the `WebUI`.
    #[allow(clippy::too_many_lines)]
    pub async fn reload_mcp_servers(&self, tenant_id: &str, user_id: &str) -> Result<()> {
        self.config_registry
            .invalidate_tenant_cache(tenant_id)
            .await;

        let config = self
            .config_registry
            .load_user_config(tenant_id, user_id, None)
            .await
            .map_err(GatewayError::Database)?;

        // Re-initialize the manager with fresh sessions
        self.init_mcp_manager_from_config(tenant_id, &config)
            .await?;

        // Increment the config version so existing sessions hot-reload their runtime
        // on their next `run_turn` (to pick up the new MCP tools).
        {
            let mut guard = self.mcp_config_version.write().await;
            let entry = guard.entry(tenant_id.to_string()).or_insert(0);
            *entry = entry.saturating_add(1);
        }

        tracing::debug!(
            tenant_id,
            "MCP servers reloaded, config_version incremented"
        );
        Ok(())
    }

    /// Increment the config version for a tenant, signaling all active sessions
    /// to hot-reload their runtime on the next `run_turn`.
    ///
    /// This is called when Skills are modified (uploaded/updated/deleted/toggled).
    /// Unlike `reload_mcp_servers`, it does **not** rebuild the MCP manager — it only
    /// bumps the version counter so that existing sessions detect a mismatch and
    /// reload their system prompt with the updated skill instructions.
    pub async fn reload_skills(&self, tenant_id: &str) {
        self.config_registry
            .invalidate_tenant_cache(tenant_id)
            .await;
        let mut guard = self.mcp_config_version.write().await;
        let entry = guard.entry(tenant_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        tracing::debug!(
            tenant_id,
            new_version = *entry,
            "skills updated, config_version incremented"
        );
    }

    /// Increment the config version for tenant-scoped search provider changes.
    ///
    /// Search providers are loaded from the same runtime config snapshot as MCP
    /// and Skills, so active sessions must rebuild on the next turn after a
    /// provider is created, updated, disabled, deleted, reordered, or its health
    /// status changes.
    pub async fn reload_search_providers(&self, tenant_id: &str) {
        let mut guard = self.mcp_config_version.write().await;
        let entry = guard.entry(tenant_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        tracing::debug!(
            tenant_id,
            new_version = *entry,
            "search providers updated, config_version incremented"
        );
    }
}

fn estimate_text_tokens(text: &str) -> usize {
    runtime::estimate_text_tokens(text)
}

fn estimate_text_tokens_for_runtime(provider: &str, model: &str, text: &str) -> usize {
    runtime::estimate_text_tokens_with_options(
        text,
        token_estimate_options_for_runtime(provider, model),
    )
}

fn token_estimate_options_for_runtime(
    provider: &str,
    model: &str,
) -> runtime::TokenEstimateOptions {
    runtime::TokenEstimateOptions {
        tokenizer: tokenizer_kind_for_runtime(provider, model),
    }
}

fn tokenizer_kind_for_runtime(provider: &str, model: &str) -> runtime::TokenizerKind {
    let provider = provider.trim().to_ascii_lowercase();
    let model = model.trim().to_ascii_lowercase();
    if provider == "openai" || provider == "custom" || provider == "openai_compat" {
        if model.contains("gpt-3.5")
            || model.contains("gpt-4-")
            || model == "gpt-4"
            || model.contains("text-embedding")
        {
            return runtime::TokenizerKind::Cl100kBase;
        }
        return runtime::TokenizerKind::O200kBase;
    }
    if model.starts_with("openai/")
        || model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("codex-")
    {
        return runtime::TokenizerKind::O200kBase;
    }
    runtime::TokenizerKind::UnicodeHeuristic
}

fn continuity_compaction_strategy() -> String {
    if runtime::trident_compaction_enabled_from_env() {
        "deterministic_trident".to_string()
    } else {
        "deterministic_summary".to_string()
    }
}

fn model_summary_compaction_enabled() -> bool {
    std::env::var("AOS_CONTEXT_MODEL_SUMMARY_COMPACTION")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "disabled"
            )
        })
        .unwrap_or(true)
}

fn should_pre_turn_compact(
    runtime: &runtime::ConversationRuntime<impl runtime::ApiClient, impl runtime::ToolExecutor>,
    provider: &str,
    model: &str,
    context_window_tokens: Option<usize>,
    trigger: &str,
) -> bool {
    if trigger.eq_ignore_ascii_case("manual")
        || trigger.eq_ignore_ascii_case("context_window_exceeded")
    {
        return true;
    }
    let token_options = token_estimate_options_for_runtime(provider, model);
    let estimated_tokens =
        runtime::estimate_session_tokens_with_options(runtime.session(), token_options);
    let context_window = resolve_context_window(model, context_window_tokens).0;
    let effective_context_limit = context_window.saturating_mul(80) / 100;
    effective_context_limit > 0 && estimated_tokens >= effective_context_limit
}

async fn compact_runtime_for_continuity(
    runtime: &mut runtime::ConversationRuntime<GatewayApiClient, GatewayToolExecutor>,
    session_id: &str,
    provider: &str,
    model: &str,
    context_window_tokens: Option<usize>,
    trigger: &str,
    provider_native_supported: bool,
    native_compaction_bridge: Option<&ProviderNativeCompactionBridge>,
) -> Result<Option<ManualCompactionResult>> {
    if !should_pre_turn_compact(runtime, provider, model, context_window_tokens, trigger) {
        return Ok(None);
    }
    let token_options = token_estimate_options_for_runtime(provider, model);
    let before_tokens =
        runtime::estimate_session_tokens_with_options(runtime.session(), token_options);
    let context_window = resolve_context_window(model, context_window_tokens).0;
    let tail_target = std::cmp::min(16_000, context_window.saturating_mul(20) / 100);
    let preserve_recent_messages =
        tail_message_count_for_budget(runtime.session(), tail_target, token_options).max(4);
    let compaction_config = runtime::CompactionConfig {
        preserve_recent_messages,
        // We already made the trigger decision with the runtime-specific
        // tokenizer above. Force compaction here so the fallback heuristic
        // inside runtime::compact_session cannot veto a real context-pressure
        // case for CJK or provider-specific tokenization.
        max_estimated_tokens: 0,
    };
    let baseline = runtime.compact(compaction_config);
    if baseline.removed_message_count == 0 {
        return Ok(None);
    }
    let mut selected_summary = baseline.summary.clone();
    let mut strategy = continuity_compaction_strategy();
    let mut provider_compaction_endpoint_called = false;
    let mut provider_compaction_output_applied = false;
    let mut provider_compaction_fallback_reason = None;
    if let Some(bridge) = native_compaction_bridge {
        match compact_session_with_provider_native(runtime, bridge, trigger).await {
            Ok(outcome) => {
                provider_compaction_endpoint_called = outcome.endpoint_called;
                provider_compaction_output_applied = outcome.output_applied;
                provider_compaction_fallback_reason = outcome.fallback_reason;
                if let Some(summary) = outcome.summary.filter(|value| !value.trim().is_empty()) {
                    selected_summary = summary;
                    strategy = format!("provider_compaction:{}", bridge.protocol.as_str());
                } else {
                    tracing::warn!(
                        session_id,
                        trigger,
                        normalized_output_count = outcome.normalized_output_count,
                        fallback_reason = ?provider_compaction_fallback_reason,
                        "provider compaction output cannot be safely installed; falling back to AOS compaction"
                    );
                }
            }
            Err(error) => {
                provider_compaction_endpoint_called = true;
                provider_compaction_fallback_reason =
                    Some("responses_compact_v1_request_failed".to_string());
                tracing::warn!(
                    session_id,
                    trigger,
                    error = %error,
                    "provider-native compaction failed; falling back to AOS compaction"
                );
            }
        }
    } else if provider_native_supported {
        provider_compaction_fallback_reason =
            Some("declared_provider_compaction_adapter_unavailable".to_string());
        tracing::warn!(
            session_id,
            trigger,
            "provider compaction was reported supported without an active endpoint adapter; using AOS compaction"
        );
    }
    if !provider_compaction_output_applied && model_summary_compaction_enabled() {
        match summarize_session_for_compaction(runtime, &selected_summary).await {
            Ok(summary) if !summary.trim().is_empty() => {
                selected_summary = summary;
                strategy = "model_summary_compaction".to_string();
            }
            Ok(_) => {
                tracing::warn!(
                    session_id,
                    trigger,
                    "model summary compaction returned empty summary; falling back to deterministic compaction"
                );
            }
            Err(error) => {
                tracing::warn!(
                    session_id,
                    trigger,
                    error = %error,
                    "model summary compaction failed; falling back to deterministic compaction"
                );
            }
        }
    }
    let compact_result = runtime
        .compact_transactionally(compaction_config, trigger, Some(selected_summary))
        .await
        .map_err(|error| GatewayError::RuntimeExecution(error.to_string()))?
        .ok_or_else(|| {
            GatewayError::RuntimeExecution(
                "compaction source window disappeared before transactional commit".to_string(),
            )
        })?;
    let after_tokens =
        runtime::estimate_session_tokens_with_options(runtime.session(), token_options);
    let archive_entries = build_compaction_archive_entries(&compact_result.archived_messages);
    Ok(Some(ManualCompactionResult {
        session_id: session_id.to_string(),
        trigger: trigger.to_string(),
        strategy,
        provider_compaction_endpoint_supported: provider_native_supported,
        provider_compaction_endpoint_called,
        provider_compaction_output_applied,
        provider_compaction_fallback_reason,
        summary: compact_result.formatted_summary,
        summary_tokens: estimate_text_tokens_for_runtime(provider, model, &compact_result.summary),
        removed_message_count: compact_result.removed_message_count,
        retained_tail_tokens: after_tokens.min(before_tokens),
        message_count_after: runtime.message_count(),
        archive_entries,
    }))
}

async fn record_turn_compaction(
    registry: &TenantConfigRegistry,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    compaction: &crate::events::CompactionRecord,
    archive_entries: Option<&[CompactionArchiveEntry]>,
) {
    let trigger = compaction
        .trigger
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let strategy = compaction
        .strategy
        .clone()
        .unwrap_or_else(continuity_compaction_strategy);
    let archive_count = archive_entries.map_or(0, <[CompactionArchiveEntry]>::len);
    let window_id = registry
        .record_session_compaction(SessionCompactionParams {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            trigger,
            strategy,
            summary_tokens: compaction
                .summary_tokens
                .unwrap_or_else(|| estimate_text_tokens(&compaction.summary))
                as i64,
            removed_message_count: compaction.removed_messages as i64,
            retained_tail_tokens: compaction.retained_tail_tokens.unwrap_or(0) as i64,
            used_memory_refs_json: Some(serde_json::json!([])),
            metadata_json: Some(serde_json::json!({
                "memoryRefsPolicy": "refs_only",
                "source": "agent_gateway_turn",
                "providerCompactionEndpointSupported": compaction.provider_compaction_endpoint_supported.unwrap_or(false),
                "providerCompactionEndpointCalled": compaction.provider_compaction_endpoint_called.unwrap_or(false),
                "providerCompactionOutputApplied": compaction.provider_compaction_output_applied.unwrap_or(false),
                "providerCompactionFallbackReason": compaction.provider_compaction_fallback_reason,
                "contextArchiveCount": archive_count,
            })),
        })
        .await;
    let Ok(window_id) = window_id else {
        return;
    };
    let Some(archive_entries) = archive_entries.filter(|entries| !entries.is_empty()) else {
        return;
    };
    let params = archive_entries
        .iter()
        .map(|entry| ContextArchiveParams {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            window_id: window_id.clone(),
            source: "compaction".to_string(),
            role: entry.role.clone(),
            ordinal: entry.ordinal as i64,
            content: entry.content.clone(),
            content_hash: context_archive_hash(&entry.role, entry.ordinal, &entry.content),
            content_kind: entry.content_kind.clone(),
            metadata_json: Some(serde_json::json!({
                "source": "agent_gateway_turn",
                "reason": "compacted_context_recovery",
            })),
        })
        .collect::<Vec<_>>();
    if let Err(error) = registry.record_context_archives(params).await {
        tracing::warn!(
            tenant_id,
            user_id,
            session_id,
            error = %error,
            "failed to persist context archive entries after compaction"
        );
    }
}

fn build_compaction_archive_entries(
    messages: &[ConversationMessage],
) -> Vec<CompactionArchiveEntry> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(ordinal, message)| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System | MessageRole::Tool => return None,
            };
            let content = archive_message_text(message);
            if !should_archive_context_text(&content, message.role) {
                return None;
            }
            let content = truncate_context_archive_content(&content);
            let content_kind = classify_context_archive_kind(&content);
            Some(CompactionArchiveEntry {
                role: role.to_string(),
                ordinal,
                content,
                content_kind,
            })
        })
        .collect()
}

fn archive_message_text(message: &ConversationMessage) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            RuntimeContentBlock::Text { text } => Some(text.as_str()),
            RuntimeContentBlock::ToolUse { .. } | RuntimeContentBlock::ToolResult { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn should_archive_context_text(text: &str, role: MessageRole) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || context_archive_text_is_sensitive(trimmed) {
        return false;
    }
    let char_count = trimmed.chars().count();
    if role == MessageRole::User {
        return true;
    }
    if char_count >= 800 {
        return true;
    }
    if char_count < 80 {
        return false;
    }
    classify_context_archive_kind(trimmed) != "text" && char_count >= 80
}

fn classify_context_archive_kind(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let line_count = text.lines().filter(|line| !line.trim().is_empty()).count();
    let table_like_lines = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.contains('\t')
                || trimmed.matches(',').count() >= 3
                || (trimmed.contains('|') && trimmed.matches('|').count() >= 2)
        })
        .count();
    if lower.contains("select ")
        || lower.contains("with ")
        || lower.contains("create table")
        || lower.contains("insert into")
        || lower.contains(" join ")
        || lower.contains(" group by")
    {
        return "sql".to_string();
    }
    if text.contains("```") {
        return "code".to_string();
    }
    if table_like_lines >= 2 || (line_count >= 4 && table_like_lines >= 1) {
        return "table".to_string();
    }
    if text.lines().any(|line| line.trim_start().starts_with('#')) {
        return "markdown".to_string();
    }
    "text".to_string()
}

fn context_archive_text_is_sensitive(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let sensitive_needles = [
        "api_key",
        "apikey",
        "secret",
        "password",
        "passwd",
        "token=",
        "bearer ",
        "private key",
        "access key",
        "authorization:",
        "cookie:",
        "set-cookie:",
        "sk-",
    ];
    if sensitive_needles
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return true;
    }
    let digit_count = text.chars().filter(|ch| ch.is_ascii_digit()).count();
    digit_count >= 14 && digit_count * 2 >= text.chars().count()
}

fn truncate_context_archive_content(text: &str) -> String {
    if text.chars().count() <= CONTEXT_ARCHIVE_MAX_CHARS {
        text.trim().to_string()
    } else {
        text.chars()
            .take(CONTEXT_ARCHIVE_MAX_CHARS)
            .collect::<String>()
    }
}

fn context_archive_hash(role: &str, ordinal: usize, content: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(role.as_bytes());
    hasher.update(b":");
    hasher.update(ordinal.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(content.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn tail_message_count_for_budget(
    session: &runtime::Session,
    target_tokens: usize,
    token_options: runtime::TokenEstimateOptions,
) -> usize {
    if session.messages.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    let mut count = 0usize;
    for message in session.messages.iter().rev() {
        let tokens = estimate_runtime_message_tokens(message, token_options);
        if count >= 4 && total.saturating_add(tokens) > target_tokens {
            break;
        }
        total = total.saturating_add(tokens);
        count = count.saturating_add(1);
    }
    count
}

fn estimate_runtime_message_tokens(
    message: &runtime::ConversationMessage,
    token_options: runtime::TokenEstimateOptions,
) -> usize {
    runtime::estimate_message_tokens_with_options(message, token_options)
}

fn resolve_context_window(model: &str, configured_context_window: Option<usize>) -> (usize, bool) {
    if let Some(value) = configured_context_window.filter(|value| *value >= 8_000) {
        return (value, false);
    }
    let lower = model.to_ascii_lowercase();
    let explicit = std::env::var("AOS_CONTEXT_WINDOW_TOKENS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value >= 8_000);
    if let Some(value) = explicit {
        return (value, false);
    }
    if let Some(value) = parse_context_window_hint(&lower) {
        return (value, false);
    }
    (128_000, true)
}

fn parse_context_window_hint(model: &str) -> Option<usize> {
    for suffix in ["tokens", "token", "ctx", "context"] {
        if let Some(value) = parse_number_before_suffix(model, suffix) {
            return Some(value);
        }
    }
    parse_trailing_scaled_context_hint(model)
}

fn parse_trailing_scaled_context_hint(text: &str) -> Option<usize> {
    let mut unit = None;
    let mut digits = String::new();
    for ch in text.chars().rev() {
        if matches!(ch, 'k' | 'm') && unit.is_none() && digits.is_empty() {
            unit = Some(ch);
        } else if ch.is_ascii_digit() {
            digits.push(ch);
        } else if ch == '-' || ch == '_' || ch == ' ' {
            if !digits.is_empty() {
                break;
            }
        } else if unit.is_some() || !digits.is_empty() {
            return None;
        }
    }
    let unit = unit?;
    if digits.is_empty() {
        return None;
    }
    let raw: String = digits.chars().rev().collect();
    let value = raw.parse::<usize>().ok()?;
    let scaled = match unit {
        'm' => value.saturating_mul(1_000_000),
        _ => value.saturating_mul(1_000),
    };
    (scaled >= 8_000).then_some(scaled)
}

fn parse_number_before_suffix(text: &str, suffix: &str) -> Option<usize> {
    let idx = text.find(suffix)?;
    let prefix = &text[..idx];
    let mut digits = String::new();
    let mut unit = None;
    for ch in prefix.chars().rev() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if matches!(ch, 'k' | 'm') && digits.is_empty() {
            unit = Some(ch);
        } else if ch == '-' || ch == '_' || ch == ' ' {
            if !digits.is_empty() {
                break;
            }
        } else if !digits.is_empty() {
            break;
        } else {
            return None;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let raw: String = digits.chars().rev().collect();
    let value = raw.parse::<usize>().ok()?;
    let scaled = match unit {
        Some('m') => value.saturating_mul(1_000_000),
        _ => value.saturating_mul(1_000),
    };
    (scaled >= 8_000).then_some(scaled)
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub name: String,
    pub state: SessionState,
    pub model: String,
    pub workspace: PathBuf,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub is_pinned: bool,
    pub is_bookmarked: bool,
    pub source: String,
    pub session_metadata: crate::events::SessionMetadata,
}

fn session_metadata_from_config(
    config: &UserRuntimeConfig,
    fallback_model: &str,
) -> crate::events::SessionMetadata {
    crate::events::SessionMetadata {
        mcp_servers: config
            .mcp_servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.name.clone())
            .collect(),
        skills: config
            .skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect(),
        permission_mode: format!("{:?}", config.permission_mode),
        model: config
            .api_keys
            .first()
            .and_then(|key| key.model.clone())
            .unwrap_or_else(|| fallback_model.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_registry::{ApiKeyEntry, UserRuntimeConfig};
    use proptest::prelude::*;

    #[tokio::test]
    async fn turn_admission_rejects_shutdown_races_and_notifies_when_idle() {
        let admission = Arc::new(TurnAdmission::new());
        let active_turn = admission.admit().expect("first turn should be admitted");
        admission.accepting.store(false, Ordering::Release);

        assert!(matches!(admission.admit(), Err(GatewayError::ShuttingDown)));
        assert!(!admission.wait_until_idle(Duration::from_millis(1)).await);

        drop(active_turn);
        assert!(admission.wait_until_idle(Duration::from_millis(100)).await);
    }

    fn runtime_ledger_envelope(
        thread_id: &str,
        kind: &str,
        payload: Value,
        sequence: u64,
    ) -> agent_protocol::AgentEventEnvelope {
        agent_protocol::AgentEventEnvelope::new(
            thread_id,
            Some("turn-1"),
            None,
            format!("runtime-{kind}-{sequence}"),
            agent_protocol::AgentEventV1::Domain(agent_protocol::DomainEvent {
                domain: "runtime".into(),
                kind: kind.into(),
                payload,
            }),
            sequence,
        )
    }

    fn ledger_event_type(event: &agent_protocol::AgentEventEnvelope) -> String {
        match &event.event {
            agent_protocol::AgentEventV1::Domain(domain) => {
                format!("{}.{}", domain.domain, domain.kind)
            }
            _ => "agent.event".into(),
        }
    }

    #[test]
    fn ledger_recovery_rebuilds_messages_without_jsonl() {
        let assistant =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "done".into(),
            }]);
        let background =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "background delivery".into(),
            }]);
        let mut visible = runtime_ledger_envelope(
            "s",
            "visible_message",
            serde_json::json!({"message":serde_json::to_value(&background).unwrap()}),
            4,
        );
        visible.turn_id = None;
        visible.payload_hash = visible.compute_payload_hash().unwrap();
        let envelopes = vec![
            runtime_ledger_envelope(
                "s",
                "turn_started",
                serde_json::json!({"userInput":"hello"}),
                1,
            ),
            runtime_ledger_envelope(
                "s",
                "assistant_message",
                serde_json::json!({"iteration":1,"message":{"message":serde_json::to_value(&assistant).unwrap()}}),
                2,
            ),
            runtime_ledger_envelope(
                "s",
                "turn_terminal",
                serde_json::json!({"status":"completed"}),
                3,
            ),
            visible,
        ];
        let mut rows = envelopes
            .iter()
            .map(|envelope| {
                (
                    i64::try_from(envelope.sequence).unwrap(),
                    ledger_event_type(envelope),
                    serde_json::to_string(envelope).unwrap(),
                    envelope.payload_hash.clone(),
                    None,
                )
            })
            .collect::<Vec<_>>();
        let mut checkpoint = rebuild_session_from_ledger_rows("s", "t", "u", &rows)
            .unwrap()
            .unwrap();
        checkpoint.stage_runtime_context(runtime::SessionRuntimeContext {
            model: Some("checkpoint-model".into()),
            provider: Some("checkpoint-provider".into()),
            tools_enabled: true,
            allowed_tools: vec!["read_file".into()],
            ..runtime::SessionRuntimeContext::default()
        });
        checkpoint.stage_context_baseline(runtime::SessionContextBaseline {
            label: Some("canonical-baseline".into()),
            system_prompt_sections: vec!["system section".into()],
            tool_names: vec!["read_file".into()],
            ..runtime::SessionContextBaseline::default()
        });
        checkpoint.fork = Some(runtime::SessionFork {
            parent_session_id: "parent-session".into(),
            branch_name: Some("audit-branch".into()),
        });
        checkpoint.record_compaction_with_replacement(
            "exact compaction summary",
            1,
            checkpoint.messages.clone(),
        );
        let checkpoint_json = checkpoint.to_recovery_json().unwrap();
        let checkpoint_hash = format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&checkpoint_json).unwrap())
        );
        let checkpoint_event = runtime_ledger_envelope(
            "s",
            "session_checkpoint",
            serde_json::json!({
                "schemaVersion": "runtime-session-checkpoint-v1",
                "reason": "test",
                "stateHash": checkpoint_hash,
                "session": checkpoint_json,
            }),
            5,
        );
        rows.push((
            5,
            ledger_event_type(&checkpoint_event),
            serde_json::to_string(&checkpoint_event).unwrap(),
            checkpoint_event.payload_hash.clone(),
            None,
        ));
        let session = rebuild_session_from_ledger_rows("s", "t", "u", &rows)
            .unwrap()
            .unwrap();
        assert_eq!(session.messages.len(), 3);
        assert_eq!(
            session.messages[0],
            runtime::ConversationMessage::user_text("hello")
        );
        assert_eq!(session.messages[1], assistant);
        assert_eq!(session.messages[2], background);
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].status,
            runtime::SessionTurnStatus::Completed
        );
        assert_eq!(
            session.to_recovery_json().unwrap(),
            checkpoint.to_recovery_json().unwrap()
        );
        assert_eq!(
            session
                .runtime_context
                .as_ref()
                .and_then(|context| context.model.as_deref()),
            Some("checkpoint-model")
        );
        assert!(session.compaction.is_some());
        assert!(session.fork.is_some());

        let mut gap = rows.clone();
        gap[1].0 = 4;
        assert!(rebuild_session_from_ledger_rows("s", "t", "u", &gap).is_err());

        let mut tampered = rows.clone();
        tampered[1].2 = tampered[1].2.replace("done", "tampered");
        assert!(rebuild_session_from_ledger_rows("s", "t", "u", &tampered).is_err());

        let mut unknown = runtime_ledger_envelope(
            "s",
            "future_required_state",
            serde_json::json!({"_requiredForRecovery": true}),
            1,
        );
        unknown.payload_hash = unknown.compute_payload_hash().unwrap();
        let unknown_rows = vec![(
            1,
            ledger_event_type(&unknown),
            serde_json::to_string(&unknown).unwrap(),
            unknown.payload_hash.clone(),
            None,
        )];
        let error = rebuild_session_from_ledger_rows("s", "t", "u", &unknown_rows)
            .expect_err("unknown required event must fail closed");
        assert!(error.contains("unknown required runtime event"));
    }

    #[tokio::test]
    async fn durable_ledger_loader_recovers_exact_encrypted_runtime_without_jsonl() {
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE agent_event_ledger (
                tenant_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                payload_hash TEXT NOT NULL,
                raw_payload_ciphertext TEXT,
                durable INTEGER NOT NULL
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        let assistant =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "ledger answer with sk-fake-recovery-secret-1234".into(),
            }]);
        let protected_assistant =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "ledger answer with [REDACTED_API_KEY]".into(),
            }]);
        let background =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "background delivery with sk-fake-background-secret-5678".into(),
            }]);
        let protected_background =
            runtime::ConversationMessage::assistant(vec![runtime::ContentBlock::Text {
                text: "background delivery with [REDACTED_API_KEY]".into(),
            }]);
        let mut visible = runtime_ledger_envelope(
            "session",
            "visible_message",
            serde_json::json!({"message":protected_background}),
            4,
        );
        visible.turn_id = None;
        visible.payload_hash = visible.compute_payload_hash().unwrap();
        let events = [
            (
                runtime_ledger_envelope(
                    "session",
                    "turn_started",
                    serde_json::json!({"userInput":"ledger question [REDACTED_API_KEY]"}),
                    1,
                ),
                serde_json::json!({"userInput":"ledger question sk-fake-recovery-secret-1234"}),
            ),
            (
                runtime_ledger_envelope(
                    "session",
                    "assistant_message",
                    serde_json::json!({"message":{"message":protected_assistant}}),
                    2,
                ),
                serde_json::json!({"message":{"message":assistant}}),
            ),
            (
                runtime_ledger_envelope(
                    "session",
                    "turn_terminal",
                    serde_json::json!({"status":"completed"}),
                    3,
                ),
                serde_json::json!({"status":"completed"}),
            ),
            (visible, serde_json::json!({"message":background})),
        ];
        for (mut event, recovery_payload) in events {
            let recovery_payload_raw = recovery_payload.to_string();
            let recovery_payload_hash = format!(
                "{:x}",
                sha2::Sha256::digest(recovery_payload_raw.as_bytes())
            );
            let agent_protocol::AgentEventV1::Domain(domain) = &mut event.event else {
                panic!("runtime fixture must be a domain event");
            };
            domain.payload.as_object_mut().unwrap().insert(
                "_recoveryPayloadHash".to_string(),
                serde_json::Value::String(recovery_payload_hash),
            );
            event.payload_hash = event.compute_payload_hash().unwrap();
            let recovery_payload_ciphertext =
                crate::crypto::encrypt(&recovery_payload_raw).unwrap();
            sqlx::query(
                "INSERT INTO agent_event_ledger
                 (tenant_id, thread_id, sequence, event_type, payload_json, payload_hash,
                  raw_payload_ciphertext, durable)
                 VALUES ('tenant', 'session', ?, ?, ?, ?, ?, 1)",
            )
            .bind(i64::try_from(event.sequence).unwrap())
            .bind(ledger_event_type(&event))
            .bind(serde_json::to_string(&event).unwrap())
            .bind(&event.payload_hash)
            .bind(recovery_payload_ciphertext)
            .execute(&db)
            .await
            .unwrap();
        }

        let recovered = load_durable_ledger_session(&db, "session", "tenant", "user")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.messages.len(), 3);
        assert_eq!(
            recovered.messages[0],
            runtime::ConversationMessage::user_text("ledger question sk-fake-recovery-secret-1234")
        );
        assert_eq!(recovered.messages[1], assistant);
        assert_eq!(recovered.messages[2], background);
        assert_eq!(recovered.turns.len(), 1);
        assert_eq!(
            recovered.turns[0].status,
            runtime::SessionTurnStatus::Completed
        );

        let workspace =
            std::env::temp_dir().join(format!("aos-ledger-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let built = RuntimeBuilder::restore(
            test_runtime_config(&["test-model"]),
            workspace.clone(),
            "session".into(),
            Arc::new(RwLock::new(McpServerSessionManager::new())),
        )
        .with_restored_session(recovered)
        .build()
        .await
        .unwrap();
        let runtime_session = built.try_session_snapshot().unwrap();
        assert_eq!(
            runtime_session.messages[0],
            runtime::ConversationMessage::user_text("ledger question sk-fake-recovery-secret-1234")
        );
        assert_eq!(runtime_session.messages[1], assistant);
        assert_eq!(runtime_session.messages[2], background);
        let persisted_projection: String =
            sqlx::query_scalar("SELECT payload_json FROM agent_event_ledger WHERE sequence = 1")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(!persisted_projection.contains("sk-fake-recovery-secret-1234"));
        let persisted_background: String =
            sqlx::query_scalar("SELECT payload_json FROM agent_event_ledger WHERE sequence = 4")
                .fetch_one(&db)
                .await
                .unwrap();
        assert!(!persisted_background.contains("sk-fake-background-secret-5678"));

        let replacement = crate::crypto::encrypt(
            &serde_json::json!({"userInput":"tampered but valid ciphertext"}).to_string(),
        )
        .unwrap();
        sqlx::query("UPDATE agent_event_ledger SET raw_payload_ciphertext = ? WHERE sequence = 1")
            .bind(replacement)
            .execute(&db)
            .await
            .unwrap();
        let error = load_durable_ledger_session(&db, "session", "tenant", "user")
            .await
            .expect_err("recovery payload replacement must fail closed");
        assert!(error
            .to_string()
            .contains("recovery payload hash does not match"));
        sqlx::query(
            "UPDATE agent_event_ledger SET raw_payload_ciphertext = NULL WHERE sequence = 1",
        )
        .execute(&db)
        .await
        .unwrap();
        let error = load_durable_ledger_session(&db, "session", "tenant", "user")
            .await
            .expect_err("removing a required recovery payload must fail closed");
        assert!(error.to_string().contains("missing the encrypted payload"));
        std::fs::remove_dir_all(workspace).unwrap();
    }

    fn test_runtime_config(models: &[&str]) -> UserRuntimeConfig {
        UserRuntimeConfig {
            db: None,
            user_id: "user-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            api_keys: models
                .iter()
                .enumerate()
                .map(|(idx, model)| ApiKeyEntry {
                    id: format!("key-{idx}"),
                    key: format!("sk-test-{idx}"),
                    provider: "openai".to_string(),
                    base_url: Some("https://example.test/v1".to_string()),
                    model: Some((*model).to_string()),
                    audio_generate_path: None,
                    audio_query_path: None,
                    priority: idx as i32,
                    is_primary: idx == 0,
                    input_price_per_million: None,
                    output_price_per_million: None,
                    capabilities_json: None,
                })
                .collect(),
            provider: "openai".to_string(),
            model: "default-model".to_string(),
            permission_mode: runtime::PermissionMode::ReadOnly,
            allowed_tools: None,
            blocked_tools: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            hooks: runtime::RuntimeHookConfig::default(),
            pm_search_providers: Vec::new(),
            scenario_scoped: true,
            scenario: Some("rd".to_string()),
        }
    }

    #[test]
    fn token_usage_delta_records_only_the_current_turn() {
        let before = RuntimeTokenUsage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_creation_input_tokens: 80,
            cache_read_input_tokens: 40,
        };
        let after = RuntimeTokenUsage {
            input_tokens: 1_150,
            output_tokens: 260,
            cache_creation_input_tokens: 90,
            cache_read_input_tokens: 75,
        };

        assert_eq!(
            token_usage_delta(after, before),
            RuntimeTokenUsage {
                input_tokens: 150,
                output_tokens: 60,
                cache_creation_input_tokens: 10,
                cache_read_input_tokens: 35,
            }
        );
    }

    #[test]
    fn token_usage_delta_saturates_when_a_runtime_counter_resets() {
        let before = RuntimeTokenUsage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_creation_input_tokens: 80,
            cache_read_input_tokens: 40,
        };
        let after = RuntimeTokenUsage {
            input_tokens: 25,
            output_tokens: 10,
            cache_creation_input_tokens: 5,
            cache_read_input_tokens: 2,
        };

        assert_eq!(
            token_usage_delta(after, before),
            RuntimeTokenUsage::default()
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        // Feature: unified-agent-workspace, Property: a scoped session lookup accepts only the exact tenant/user owner.
        #[test]
        fn scoped_session_owner_never_accepts_another_identity(
            tenant in "[a-z0-9-]{1,24}",
            user in "[a-z0-9-]{1,24}",
            other_tenant in "[a-z0-9-]{1,24}",
            other_user in "[a-z0-9-]{1,24}",
        ) {
            prop_assert!(session_owner_matches(&tenant, &user, Some(&tenant), Some(&user)));
            if other_tenant != tenant {
                prop_assert!(!session_owner_matches(&tenant, &user, Some(&other_tenant), Some(&user)));
            }
            if other_user != user {
                prop_assert!(!session_owner_matches(&tenant, &user, Some(&tenant), Some(&other_user)));
            }
            prop_assert!(session_owner_matches(&tenant, &user, None, None));
        }
    }

    #[test]
    fn rd_candidate_source_is_hidden_internal_runtime() {
        for source in ["rd_internal_cand", "rd_internal_candidate"] {
            assert!(is_rd_candidate_source(source));
            assert!(is_internal_transient_source(source));
            assert!(is_hidden_runtime_source(source));
        }
    }

    #[test]
    fn bot_parent_sessions_are_hidden_transient_runtimes() {
        assert!(is_internal_transient_source("bot_internal_super_assistant"));
        assert!(is_hidden_runtime_source("bot_internal_super_assistant"));
    }

    #[test]
    fn rd_thread_source_is_hidden_persistent_runtime() {
        let source = "rd_thread";

        assert!(!is_internal_transient_source(source));
        assert!(is_hidden_runtime_source(source));
        assert_eq!(runtime_scenario_from_source(source), "rd");
    }

    #[test]
    fn super_assistant_sources_use_the_general_chat_runtime() {
        for source in ["super_assistant", "super-assistant", "chat"] {
            assert_eq!(runtime_scenario_from_source(source), "chat");
        }
    }

    #[test]
    fn user_visible_sources_are_not_hidden() {
        for source in ["chat", "pm", "rd", "agent"] {
            assert!(
                !is_hidden_runtime_source(source),
                "{source} should remain visible"
            );
        }
    }

    #[test]
    fn compaction_archive_keeps_user_context_and_structured_content() {
        let sql = format!(
            "WITH revenue AS (SELECT user_id, SUM(ad_rev) AS ad_rev FROM fact GROUP BY user_id)\nSELECT user_id, ad_rev FROM revenue;\n{}",
            "dt,group_id,dau,roi\n20260701,1,10,1.2\n".repeat(30)
        );
        let messages = vec![
            ConversationMessage::user_text("hello"),
            ConversationMessage::user_text(sql.clone()),
            ConversationMessage::assistant(vec![RuntimeContentBlock::Text {
                text: "Short acknowledgement.".to_string(),
            }]),
        ];

        let entries = build_compaction_archive_entries(&messages);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content_kind, "text");
        assert_eq!(entries[0].content, "hello");
        assert_eq!(entries[1].role, "user");
        assert_eq!(entries[1].content_kind, "sql");
        assert_eq!(entries[1].content, sql.trim());
    }

    #[test]
    fn compaction_archive_skips_sensitive_context() {
        let messages = vec![ConversationMessage::user_text(format!(
            "apikey=sk-{}\n{}",
            "x".repeat(80),
            "SELECT * FROM t;\n".repeat(100)
        ))];

        let entries = build_compaction_archive_entries(&messages);

        assert!(entries.is_empty());
    }

    #[test]
    fn mcp_search_like_argument_builder_uses_schema_query_key() {
        let candidate = McpSearchToolCandidate {
            qualified_name: "mcp__search__run".to_string(),
            server_name: "search".to_string(),
            description: Some("Search the web".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "q": {"type": "string"},
                    "limit": {"type": "integer"}
                }
            })),
        };
        let args =
            mcp_search_tool_arguments(&candidate, "open source aos", 7).expect("search args");
        assert_eq!(
            args.get("q").and_then(Value::as_str),
            Some("open source aos")
        );
        assert_eq!(args.get("limit").and_then(Value::as_u64), Some(7));
    }

    #[test]
    fn mcp_search_like_filter_uses_name_description_and_schema() {
        let tool = runtime::mcp::McpTool {
            name: "mcp__srv__lookup".to_string(),
            description: Some("Fetch public web pages".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {"url": {"type": "string"}}
            })),
            annotations: None,
            meta: None,
        };
        assert!(is_search_like_mcp_tool(&tool));
    }

    #[test]
    fn model_override_keeps_only_matching_api_keys() {
        let mut config = test_runtime_config(&["gpt-5.5", "gpt-4o"]);

        apply_model_override(&mut config, Some("gpt-4o"));

        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.api_keys.len(), 1);
        assert_eq!(config.api_keys[0].model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn model_override_without_matching_key_preserves_failover_set() {
        let mut config = test_runtime_config(&["gpt-5.5", "gpt-4o"]);

        apply_model_override(&mut config, Some("custom-model"));

        assert_eq!(config.model, "custom-model");
        assert_eq!(config.api_keys.len(), 2);
    }

    #[test]
    fn unpinned_session_hot_reload_follows_new_key_priority() {
        let mut config = test_runtime_config(&["new-high-priority-model", "old-model"]);

        apply_model_override(&mut config, reload_model_override(None, "old-model", false));

        assert_eq!(config.api_keys.len(), 2);
        assert_eq!(config.api_keys[0].id, "key-0");
        assert_eq!(
            config.api_keys[0].model.as_deref(),
            Some("new-high-priority-model")
        );
    }

    #[test]
    fn pinned_session_hot_reload_preserves_selected_model() {
        let mut config = test_runtime_config(&["new-high-priority-model", "selected-model"]);

        apply_model_override(
            &mut config,
            reload_model_override(None, "selected-model", true),
        );

        assert_eq!(config.api_keys.len(), 1);
        assert_eq!(config.api_keys[0].id, "key-1");
        assert_eq!(config.model, "selected-model");
    }

    #[test]
    fn explicit_reload_model_pins_even_an_unpinned_session() {
        let mut config = test_runtime_config(&["old-model", "new-model"]);

        apply_model_override(
            &mut config,
            reload_model_override(Some("  new-model  "), "old-model", false),
        );

        assert_eq!(config.api_keys.len(), 1);
        assert_eq!(config.api_keys[0].id, "key-1");
        assert_eq!(config.model, "new-model");
    }

    #[test]
    fn context_window_hint_is_model_name_agnostic() {
        assert_eq!(
            parse_context_window_hint("vendor-alpha-256k").unwrap(),
            256_000
        );
        assert_eq!(
            parse_context_window_hint("custom-model-1m-context").unwrap(),
            1_000_000
        );
        assert_eq!(parse_context_window_hint("tiny-7k"), None);
    }

    #[test]
    fn configured_context_window_takes_precedence_over_model_hint() {
        assert_eq!(
            resolve_context_window("vendor-alpha-256k", Some(131_072)),
            (131_072, false)
        );
        assert_eq!(
            resolve_context_window("future-model", Some(1_000_000)),
            (1_000_000, false)
        );
    }

    #[test]
    fn context_window_recovery_converges_without_tools_or_internal_search() {
        let original = StreamingTurnOptions {
            blocked_tools: vec!["dangerous".to_string()],
            disable_tools: false,
            disable_provider_thinking: false,
            enable_deferred_tools: true,
            system_instructions: vec!["original".to_string()],
            reasoning_budget: InternalReasoningBudget::Deep,
            prefer_native_web_search: true,
            suppress_native_web_search: false,
            stream_timeout_secs: Some(450),
            disable_stream_timeout: true,
            web_tool_result_budget: Some(8),
            model_budget_stage: runtime::RuntimeModelBudgetStage::General,
        };

        let recovery = context_window_recovery_options(&original);

        assert!(recovery.disable_tools);
        assert!(recovery.disable_provider_thinking);
        assert!(!recovery.enable_deferred_tools);
        assert_eq!(recovery.reasoning_budget, InternalReasoningBudget::Fast);
        assert!(!recovery.prefer_native_web_search);
        assert!(recovery.suppress_native_web_search);
        assert_eq!(recovery.stream_timeout_secs, Some(90));
        assert!(!recovery.disable_stream_timeout);
        assert_eq!(recovery.web_tool_result_budget, None);
        assert_eq!(
            recovery.model_budget_stage,
            runtime::RuntimeModelBudgetStage::UserVisibleError
        );
        assert_eq!(recovery.system_instructions.len(), 2);
    }

    #[test]
    fn continuity_strategy_uses_deterministic_fallback_name() {
        let strategy = continuity_compaction_strategy();
        assert!(matches!(
            strategy.as_str(),
            "deterministic_trident" | "deterministic_summary"
        ));
    }
}
