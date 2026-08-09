use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{JsonError, JsonValue};
use crate::usage::TokenUsage;

const SESSION_VERSION: u32 = 1;
const ROTATE_AFTER_BYTES: u64 = 256 * 1024;
const MAX_ROTATED_FILES: usize = 3;
static SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
static LAST_TIMESTAMP_MS: AtomicU64 = AtomicU64::new(0);

/// Speaker role associated with a persisted conversation message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Structured message content stored inside a [`Session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    ToolResult {
        tool_use_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
}

/// One conversation message with optional token-usage metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    /// Extended thinking/reasoning content (e.g. Claude's internal reasoning).
    /// Stored separately from blocks so it can be shown as a distinct UI element.
    pub thinking: Option<String>,
    /// Cryptographic signature for the thinking block, when the provider
    /// supplies one (Anthropic extended thinking). Echoed back to the provider
    /// on subsequent turns to preserve reasoning continuity.
    pub thinking_signature: Option<String>,
    pub usage: Option<TokenUsage>,
}

/// Metadata describing the latest compaction that summarized a session.
/// Running-state liveness classification for a session heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLiveness {
    Healthy,
    Stalled,
    TransportDead,
    Unknown,
}

/// Heartbeat emitted from canonical session state, independent of terminal
/// rendering. Used to surface "phantom completion" situations where the
/// transport is alive but no health check has landed within the stall window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionHeartbeat {
    pub session_id: String,
    pub observed_at_ms: u64,
    pub transport_alive: bool,
    pub liveness: SessionLiveness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCompaction {
    pub count: u32,
    pub removed_message_count: usize,
    pub summary: String,
    pub window_number: u64,
    pub first_window_id: String,
    pub window_id: String,
    pub previous_window_id: Option<String>,
    pub previous_window_number: Option<u64>,
    /// Canonical model-visible history after this compaction.
    ///
    /// This mirrors Codex's `replacement_history`: when replaying an
    /// append-style session log, the newest compaction checkpoint becomes the
    /// baseline and later message records are applied as a suffix. It is kept
    /// separate from the recoverable archive side channel so resume/fork/hot
    /// reload semantics do not depend on memory search.
    pub replacement_messages: Vec<ConversationMessage>,
    /// Runtime/model/tool context that was active when this replacement
    /// checkpoint became live.
    pub runtime_context: Option<SessionRuntimeContext>,
    /// Canonical model-visible context baseline that was active when this
    /// replacement checkpoint became live.
    pub context_baseline: Option<SessionContextBaseline>,
    /// Exact messages removed by this compaction.
    ///
    /// These are never replayed into the model context automatically, but they
    /// stay in the session checkpoint so "recall the prior SQL/data/report
    /// exactly" can be served from a durable source even after snapshot rewrites
    /// or when external DB archive persistence is unavailable.
    pub archived_messages: Vec<ConversationMessage>,
    /// Exact archives for every compaction window retained in this session.
    ///
    /// `archived_messages` is the latest window for compatibility and quick
    /// access. This chain preserves older windows across repeated compactions,
    /// which gives resumed sessions a durable Codex-like way to recover exact
    /// compacted-away SQL/data/code without depending on an external DB archive.
    pub archive_windows: Vec<SessionArchiveWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArchiveWindow {
    pub window_number: u64,
    pub window_id: String,
    pub previous_window_id: Option<String>,
    pub removed_message_count: usize,
    pub archived_messages: Vec<ConversationMessage>,
}

/// Model/runtime environment visible to a turn or compaction window.
///
/// Codex stores equivalent resume metadata as `TurnContext`, `PreviousTurnSettings`,
/// `SessionContextWindow`, and world-state records in the rollout. AOS uses this
/// product-neutral snapshot so Chat, PM, and adversarial sessions can reconstruct
/// not only messages, but also the model/tool/memory/file/search environment that
/// shaped those messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRuntimeContext {
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub scenario: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub stream_timeout_secs: Option<u64>,
    pub disable_stream_timeout: bool,
    pub prefer_native_web_search: bool,
    pub suppress_native_web_search: bool,
    pub tools_enabled: bool,
    pub allowed_tools: Vec<String>,
    pub blocked_tools: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub memory_enabled: bool,
    pub file_context_enabled: bool,
    pub pm_search_provider_count: u32,
    pub permission_mode: Option<String>,
    pub context_window_tokens: Option<u32>,
    pub system_prompt_hash: Option<String>,
    pub system_prompt_section_count: Option<u32>,
    pub window_number: Option<u64>,
    pub window_id: Option<String>,
    pub previous_window_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

/// Canonical model-visible context baseline for a session window.
///
/// Codex persists `TurnContextItem` and `WorldState` rollout items so resume,
/// fork, rollback, and compaction can reconstruct not only chat messages but
/// the prompt/context environment that shaped them. AOS keeps this
/// product-neutral baseline alongside the existing runtime context. It records
/// the actual system/developer context sections and source manifests visible
/// to the model so compacted sessions can recover exact memory/file/tool
/// grounding instead of depending only on a summary or prompt hash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionContextBaseline {
    pub turn_id: Option<String>,
    pub label: Option<String>,
    pub system_prompt_hash: Option<String>,
    pub system_prompt_sections: Vec<String>,
    pub system_prompt_section_hashes: Vec<String>,
    pub tool_names: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub skills: Vec<String>,
    pub memory_enabled: bool,
    pub file_context_enabled: bool,
    pub search_provider_count: u32,
    pub window_number: Option<u64>,
    pub window_id: Option<String>,
    pub previous_window_id: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

/// Provenance recorded when a session is forked from another session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFork {
    pub parent_session_id: String,
    pub branch_name: Option<String>,
}

/// A single user prompt recorded with a timestamp for history tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPromptEntry {
    pub timestamp_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTurn {
    pub turn_id: String,
    pub user_input: String,
    pub start_message_count: usize,
    pub end_message_count: Option<usize>,
    pub status: SessionTurnStatus,
    pub runtime_context: Option<SessionRuntimeContext>,
    pub context_baseline: Option<SessionContextBaseline>,
}

impl SessionTurn {
    fn remap_message_counts_after_compaction(
        &mut self,
        removed_start: usize,
        removed_message_count: usize,
        replacement_prefix_count: usize,
    ) {
        self.start_message_count = remap_message_count_after_compaction(
            self.start_message_count,
            removed_start,
            removed_message_count,
            replacement_prefix_count,
        );
        self.end_message_count = self.end_message_count.map(|count| {
            remap_message_count_after_compaction(
                count,
                removed_start,
                removed_message_count,
                replacement_prefix_count,
            )
        });
    }
}

fn remap_message_count_after_compaction(
    count: usize,
    removed_start: usize,
    removed_message_count: usize,
    replacement_prefix_count: usize,
) -> usize {
    if removed_message_count == 0 || count <= removed_start {
        return count;
    }
    let removed_end = removed_start.saturating_add(removed_message_count);
    if count <= removed_end {
        return replacement_prefix_count;
    }
    replacement_prefix_count.saturating_add(count.saturating_sub(removed_end))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTurnStatus {
    Running,
    Suspended,
    Completed,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionPersistence {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionReplayRecord {
    Message(ConversationMessage),
    Compaction(SessionCompaction),
    RuntimeContext(SessionRuntimeContext),
    ContextBaseline(SessionContextBaseline),
    TurnStarted(SessionTurn),
    TurnCompleted {
        turn_id: String,
        end_message_count: usize,
        status: SessionTurnStatus,
    },
    RollbackTo {
        message_count: usize,
    },
    RollbackTurn {
        turn_id: String,
        message_count: usize,
    },
    RollbackTurns {
        turn_count: usize,
    },
}

/// Persisted conversational state for the runtime and CLI session manager.
///
/// `workspace_root` binds the session to the worktree it was created in. The
/// global session store under `~/.local/share/opencode` is shared across every
/// `opencode serve` instance, so without an explicit workspace root parallel
/// lanes can race and report success while writes land in the wrong CWD.
#[derive(Debug, Clone)]
pub struct Session {
    pub version: u32,
    pub session_id: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub messages: Vec<ConversationMessage>,
    pub compaction: Option<SessionCompaction>,
    pub fork: Option<SessionFork>,
    pub workspace_root: Option<PathBuf>,
    pub prompt_history: Vec<SessionPromptEntry>,
    pub turns: Vec<SessionTurn>,
    /// Latest known model/tool/memory/file/search environment for this session.
    pub runtime_context: Option<SessionRuntimeContext>,
    /// Latest known model-visible context baseline for this session.
    pub context_baseline: Option<SessionContextBaseline>,
    /// The model used in this session, persisted so resumed sessions can
    /// report which model was originally used.
    /// Timestamp of last successful runtime health check.
    pub last_health_check_ms: Option<u64>,
    pub model: Option<String>,
    /// User ID associated with this session (from web-server auth).
    pub user_id: Option<String>,
    /// Tenant ID associated with this session (from web-server auth).
    pub tenant_id: Option<String>,
    persistence: Option<SessionPersistence>,
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.session_id == other.session_id
            && self.created_at_ms == other.created_at_ms
            && self.updated_at_ms == other.updated_at_ms
            && self.messages == other.messages
            && self.compaction == other.compaction
            && self.fork == other.fork
            && self.workspace_root == other.workspace_root
            && self.prompt_history == other.prompt_history
            && self.turns == other.turns
            && self.runtime_context == other.runtime_context
            && self.context_baseline == other.context_baseline
            && self.last_health_check_ms == other.last_health_check_ms
            && self.user_id == other.user_id
            && self.tenant_id == other.tenant_id
    }
}

impl Eq for Session {}

/// Errors raised while loading, parsing, or saving sessions.
#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    Json(JsonError),
    Format(String),
}

impl Display for SessionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Format(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<std::io::Error> for SessionError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<JsonError> for SessionError {
    fn from(value: JsonError) -> Self {
        Self::Json(value)
    }
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        let now = current_time_millis();
        Self {
            version: SESSION_VERSION,
            session_id: generate_session_id(),
            created_at_ms: now,
            updated_at_ms: now,
            messages: Vec::new(),
            compaction: None,
            fork: None,
            workspace_root: None,
            prompt_history: Vec::new(),
            turns: Vec::new(),
            runtime_context: None,
            context_baseline: None,
            last_health_check_ms: None,
            model: None,
            user_id: None,
            tenant_id: None,
            persistence: None,
        }
    }

    #[must_use]
    pub fn with_persistence_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.persistence = Some(SessionPersistence { path: path.into() });
        self
    }

    pub fn set_persistence_path(&mut self, path: impl Into<PathBuf>) {
        self.persistence = Some(SessionPersistence { path: path.into() });
    }

    /// Bind this session to the workspace root it was created in.
    ///
    /// This is the per-worktree counterpart to the global session store and
    /// lets downstream tooling reject writes that drift to the wrong CWD when
    /// multiple `opencode serve` instances share `~/.local/share/opencode`.
    #[must_use]
    pub fn with_workspace_root(mut self, workspace_root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    /// Associate this session with a user ID (from web-server auth).
    #[must_use]
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Associate this session with a tenant ID (from web-server auth).
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    #[must_use]
    pub fn persistence_path(&self) -> Option<&Path> {
        self.persistence.as_ref().map(|value| value.path.as_path())
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        let path = path.as_ref();
        let snapshot = self.render_jsonl_snapshot()?;
        rotate_session_file_if_needed(path)?;
        write_atomic(path, &snapshot)?;
        cleanup_rotated_logs(path)?;
        Ok(())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)?;
        let session = match JsonValue::parse(&contents) {
            Ok(value)
                if value
                    .as_object()
                    .is_some_and(|object| object.contains_key("messages")) =>
            {
                Self::from_json(&value)?
            }
            Err(_) | Ok(_) => Self::from_jsonl(&contents)?,
        };
        Ok(session.with_persistence_path(path.to_path_buf()))
    }

    pub fn push_message(&mut self, message: ConversationMessage) -> Result<(), SessionError> {
        self.touch();
        self.messages.push(message);
        let persist_result = {
            let message_ref = self.messages.last().ok_or_else(|| {
                SessionError::Format("message was just pushed but missing".to_string())
            })?;
            self.append_persisted_message(message_ref)
        };
        if let Err(error) = persist_result {
            self.messages.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn rollback_messages_to_len(&mut self, len: usize) -> Result<(), SessionError> {
        if len >= self.messages.len() {
            return Ok(());
        }
        self.messages.truncate(len);
        for turn in &mut self.turns {
            if turn.start_message_count >= len {
                turn.end_message_count = Some(len);
                turn.status = SessionTurnStatus::RolledBack;
            } else if turn.end_message_count.is_some_and(|end| end > len) {
                turn.end_message_count = Some(len);
                turn.status = SessionTurnStatus::RolledBack;
            }
        }
        self.touch();
        if self.persistence_path().is_some() {
            self.append_persisted_rollback(len)?;
        }
        Ok(())
    }

    pub fn rollback_last_turns(&mut self, turn_count: usize) -> Result<(), SessionError> {
        if turn_count == 0 {
            return Ok(());
        }
        let Some(target_len) = rollback_message_count_for_last_turns(&self.turns, turn_count)
        else {
            return Ok(());
        };
        self.messages.truncate(target_len);
        let mut remaining = turn_count;
        for turn in self.turns.iter_mut().rev() {
            if remaining == 0 {
                break;
            }
            if matches!(turn.status, SessionTurnStatus::RolledBack) {
                continue;
            }
            turn.end_message_count = Some(target_len);
            turn.status = SessionTurnStatus::RolledBack;
            remaining -= 1;
        }
        self.touch();
        if self.persistence_path().is_some() {
            self.append_persisted_turn_rollback(turn_count)?;
        }
        Ok(())
    }

    pub fn rollback_latest_turn_started_at(
        &mut self,
        start_message_count: usize,
    ) -> Result<bool, SessionError> {
        let index = self.turns.iter().rposition(|turn| {
            turn.start_message_count == start_message_count
                && !matches!(turn.status, SessionTurnStatus::RolledBack)
        });
        let index = index.or_else(|| {
            let current_len = self.messages.len();
            self.turns.iter().rposition(|turn| {
                matches!(
                    turn.status,
                    SessionTurnStatus::Running | SessionTurnStatus::Suspended
                ) && turn.start_message_count <= current_len
                    && turn.start_message_count <= start_message_count
            })
        });
        let Some(index) = index else {
            return Ok(false);
        };
        let turn_id = self.turns[index].turn_id.clone();
        let rollback_to = self.turns[index]
            .start_message_count
            .min(self.messages.len());
        self.messages.truncate(rollback_to);
        self.turns[index].end_message_count = Some(rollback_to);
        self.turns[index].status = SessionTurnStatus::RolledBack;
        self.touch();
        if self.persistence_path().is_some() {
            self.append_persisted_turn_rollback_to(&turn_id, rollback_to)?;
        }
        Ok(true)
    }

    pub fn push_user_text(&mut self, text: impl Into<String>) -> Result<(), SessionError> {
        self.push_message(ConversationMessage::user_text(text))
    }

    /// Record that a runtime health check landed at `timestamp_ms`. Drives the
    /// `Healthy`/`Stalled` classification produced by [`Self::heartbeat_at`].
    pub fn record_health_check(&mut self, timestamp_ms: u64) {
        self.last_health_check_ms = Some(timestamp_ms);
        self.touch();
    }

    /// Classify session liveness from canonical state at `now_ms`.
    ///
    /// A session is `Stalled` when the transport is alive but no health check
    /// has landed within `stalled_after_ms` — the signature of a "phantom
    /// completion" where the lane looks finished but the runtime is wedged.
    #[must_use]
    pub fn heartbeat_at(
        &self,
        now_ms: u64,
        stalled_after_ms: u64,
        transport_alive: bool,
    ) -> SessionHeartbeat {
        let liveness = match (transport_alive, self.last_health_check_ms) {
            (false, _) => SessionLiveness::TransportDead,
            (true, Some(last)) if now_ms.saturating_sub(last) <= stalled_after_ms => {
                SessionLiveness::Healthy
            }
            (true, Some(_)) => SessionLiveness::Stalled,
            (true, None) => SessionLiveness::Unknown,
        };

        SessionHeartbeat {
            session_id: self.session_id.clone(),
            observed_at_ms: now_ms,
            transport_alive,
            liveness,
        }
    }

    pub fn record_compaction(&mut self, summary: impl Into<String>, removed_message_count: usize) {
        self.record_compaction_with_replacement(
            summary,
            removed_message_count,
            self.messages.clone(),
        );
    }

    pub fn record_compaction_with_replacement(
        &mut self,
        summary: impl Into<String>,
        removed_message_count: usize,
        replacement_messages: Vec<ConversationMessage>,
    ) {
        self.record_compaction_checkpoint(
            summary,
            removed_message_count,
            replacement_messages,
            Vec::new(),
        );
    }

    pub fn record_compaction_checkpoint(
        &mut self,
        summary: impl Into<String>,
        removed_message_count: usize,
        replacement_messages: Vec<ConversationMessage>,
        archived_messages: Vec<ConversationMessage>,
    ) {
        self.record_compaction_checkpoint_with_removed_start(
            summary,
            0,
            removed_message_count,
            replacement_messages,
            archived_messages,
        );
    }

    pub(crate) fn record_compaction_checkpoint_with_removed_start(
        &mut self,
        summary: impl Into<String>,
        removed_start: usize,
        removed_message_count: usize,
        replacement_messages: Vec<ConversationMessage>,
        archived_messages: Vec<ConversationMessage>,
    ) {
        self.touch();
        let count = self.compaction.as_ref().map_or(1, |value| value.count + 1);
        let mut archive_windows = self
            .compaction
            .as_ref()
            .map(|value| value.archive_windows.clone())
            .unwrap_or_default();
        let previous_window_number = self
            .compaction
            .as_ref()
            .map(|value| value.window_number)
            .filter(|value| *value > 0);
        let window_number = previous_window_number.map_or(1, |value| value.saturating_add(1));
        let first_window_id = self
            .compaction
            .as_ref()
            .map(|value| value.first_window_id.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(generate_window_id);
        let previous_window_id = self
            .compaction
            .as_ref()
            .map(|value| value.window_id.clone())
            .filter(|value| !value.trim().is_empty());
        let window_id = generate_window_id();
        if !archived_messages.is_empty() {
            archive_windows.push(SessionArchiveWindow {
                window_number,
                window_id: window_id.clone(),
                previous_window_id: previous_window_id.clone(),
                removed_message_count,
                archived_messages: archived_messages.clone(),
            });
        }
        if removed_message_count > 0 {
            let replacement_prefix_count = usize::from(!replacement_messages.is_empty());
            for turn in &mut self.turns {
                turn.remap_message_counts_after_compaction(
                    removed_start,
                    removed_message_count,
                    replacement_prefix_count,
                );
            }
        }
        let mut runtime_context = self.runtime_context.clone();
        if let Some(context) = runtime_context.as_mut() {
            context.window_number = Some(window_number);
            context.window_id = Some(window_id.clone());
            context.previous_window_id = previous_window_id.clone();
        }
        let mut context_baseline = self.context_baseline.clone();
        if let Some(baseline) = context_baseline.as_mut() {
            baseline.window_number = Some(window_number);
            baseline.window_id = Some(window_id.clone());
            baseline.previous_window_id = previous_window_id.clone();
        }
        self.compaction = Some(SessionCompaction {
            count,
            removed_message_count,
            summary: summary.into(),
            window_number,
            first_window_id,
            window_id,
            previous_window_id,
            previous_window_number,
            replacement_messages,
            runtime_context,
            context_baseline,
            archived_messages,
            archive_windows,
        });
    }

    pub fn record_runtime_context(
        &mut self,
        context: SessionRuntimeContext,
    ) -> Result<(), SessionError> {
        self.stage_runtime_context(context);
        if let Some(context) = self.runtime_context.as_ref() {
            self.append_persisted_runtime_context(context)?;
        }
        Ok(())
    }

    pub fn stage_runtime_context(&mut self, mut context: SessionRuntimeContext) {
        self.touch();
        if context.window_number.is_none() {
            context.window_number = self.compaction.as_ref().map(|value| value.window_number);
        }
        if context.window_id.is_none() {
            context.window_id = self
                .compaction
                .as_ref()
                .map(|value| value.window_id.clone());
        }
        if context.previous_window_id.is_none() {
            context.previous_window_id = self
                .compaction
                .as_ref()
                .and_then(|value| value.previous_window_id.clone());
        }
        self.runtime_context = Some(context);
        if let Some(compaction) = self.compaction.as_mut() {
            compaction.runtime_context = self.runtime_context.clone();
        }
    }

    pub fn record_context_baseline(
        &mut self,
        baseline: SessionContextBaseline,
    ) -> Result<(), SessionError> {
        self.stage_context_baseline(baseline);
        if let Some(baseline) = self.context_baseline.as_ref() {
            self.append_persisted_context_baseline(baseline)?;
        }
        Ok(())
    }

    pub fn stage_context_baseline(&mut self, mut baseline: SessionContextBaseline) {
        self.touch();
        if baseline.window_number.is_none() {
            baseline.window_number = self.compaction.as_ref().map(|value| value.window_number);
        }
        if baseline.window_id.is_none() {
            baseline.window_id = self
                .compaction
                .as_ref()
                .map(|value| value.window_id.clone());
        }
        if baseline.previous_window_id.is_none() {
            baseline.previous_window_id = self
                .compaction
                .as_ref()
                .and_then(|value| value.previous_window_id.clone());
        }
        self.context_baseline = Some(baseline);
        if let Some(compaction) = self.compaction.as_mut() {
            compaction.context_baseline = self.context_baseline.clone();
        }
    }

    pub fn begin_turn(&mut self, user_input: impl Into<String>) -> Result<String, SessionError> {
        self.touch();
        let turn_id = generate_turn_id();
        let mut runtime_context = self.runtime_context.clone();
        if let Some(context) = runtime_context.as_mut() {
            context.turn_id = Some(turn_id.clone());
        }
        let mut context_baseline = self.context_baseline.clone();
        if let Some(baseline) = context_baseline.as_mut() {
            baseline.turn_id = Some(turn_id.clone());
        }
        let turn = SessionTurn {
            turn_id: turn_id.clone(),
            user_input: user_input.into(),
            start_message_count: self.messages.len(),
            end_message_count: None,
            status: SessionTurnStatus::Running,
            runtime_context,
            context_baseline,
        };
        self.turns.push(turn);
        if let Some(turn) = self.turns.last() {
            if let Err(error) = self.append_persisted_turn_started(turn) {
                self.turns.pop();
                return Err(error);
            }
        }
        Ok(turn_id)
    }

    pub fn complete_turn(
        &mut self,
        turn_id: &str,
        status: SessionTurnStatus,
    ) -> Result<(), SessionError> {
        self.touch();
        let end_message_count = self.messages.len();
        if let Some(turn) = self
            .turns
            .iter_mut()
            .rev()
            .find(|turn| turn.turn_id == turn_id)
        {
            turn.end_message_count = Some(end_message_count);
            turn.status = status;
        }
        self.append_persisted_turn_completed(turn_id, end_message_count, status)
    }

    #[must_use]
    pub fn fork(&self, branch_name: Option<String>) -> Self {
        let now = current_time_millis();
        let messages = self.messages.clone();
        let mut compaction = self.compaction.clone();
        if let Some(compaction) = compaction.as_mut() {
            compaction.replacement_messages = messages.clone();
        }
        let mut turns = self.turns.clone();
        for turn in &mut turns {
            if turn.end_message_count.is_none()
                || matches!(
                    turn.status,
                    SessionTurnStatus::Running | SessionTurnStatus::Suspended
                )
            {
                turn.end_message_count = Some(messages.len());
                turn.status = SessionTurnStatus::Completed;
            }
        }
        Self {
            version: self.version,
            session_id: generate_session_id(),
            created_at_ms: now,
            updated_at_ms: now,
            messages,
            compaction,
            fork: Some(SessionFork {
                parent_session_id: self.session_id.clone(),
                branch_name: normalize_optional_string(branch_name),
            }),
            workspace_root: self.workspace_root.clone(),
            prompt_history: self.prompt_history.clone(),
            turns,
            runtime_context: self.runtime_context.clone(),
            context_baseline: self.context_baseline.clone(),
            last_health_check_ms: self.last_health_check_ms,
            model: self.model.clone(),
            user_id: self.user_id.clone(),
            tenant_id: self.tenant_id.clone(),
            persistence: None,
        }
    }

    pub fn to_json(&self) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        object.insert(
            "version".to_string(),
            JsonValue::Number(i64::from(self.version)),
        );
        object.insert(
            "session_id".to_string(),
            JsonValue::String(self.session_id.clone()),
        );
        object.insert(
            "created_at_ms".to_string(),
            JsonValue::Number(i64_from_u64(self.created_at_ms, "created_at_ms")?),
        );
        object.insert(
            "updated_at_ms".to_string(),
            JsonValue::Number(i64_from_u64(self.updated_at_ms, "updated_at_ms")?),
        );
        object.insert(
            "messages".to_string(),
            JsonValue::Array(
                self.messages
                    .iter()
                    .map(ConversationMessage::to_json)
                    .collect(),
            ),
        );
        if let Some(compaction) = &self.compaction {
            object.insert(
                "compaction".to_string(),
                compaction.to_json_with_replacement(&self.messages)?,
            );
        }
        if let Some(fork) = &self.fork {
            object.insert("fork".to_string(), fork.to_json());
        }
        if let Some(workspace_root) = &self.workspace_root {
            object.insert(
                "workspace_root".to_string(),
                JsonValue::String(workspace_root_to_string(workspace_root)?),
            );
        }
        if !self.prompt_history.is_empty() {
            object.insert(
                "prompt_history".to_string(),
                JsonValue::Array(
                    self.prompt_history
                        .iter()
                        .map(SessionPromptEntry::to_jsonl_record)
                        .collect(),
                ),
            );
        }
        if !self.turns.is_empty() {
            object.insert(
                "turns".to_string(),
                JsonValue::Array(self.turns.iter().map(SessionTurn::to_json).collect()),
            );
        }
        if let Some(runtime_context) = &self.runtime_context {
            object.insert("runtime_context".to_string(), runtime_context.to_json()?);
        }
        if let Some(context_baseline) = &self.context_baseline {
            object.insert("context_baseline".to_string(), context_baseline.to_json()?);
        }
        if let Some(user_id) = &self.user_id {
            object.insert("user_id".to_string(), JsonValue::String(user_id.clone()));
        }
        if let Some(tenant_id) = &self.tenant_id {
            object.insert(
                "tenant_id".to_string(),
                JsonValue::String(tenant_id.clone()),
            );
        }
        Ok(JsonValue::Object(object))
    }

    pub fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("session must be an object".to_string()))?;
        let version = object
            .get("version")
            .and_then(JsonValue::as_i64)
            .ok_or_else(|| SessionError::Format("missing version".to_string()))?;
        let version = u32::try_from(version)
            .map_err(|_| SessionError::Format("version out of range".to_string()))?;
        let messages = object
            .get("messages")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| SessionError::Format("missing messages".to_string()))?
            .iter()
            .map(ConversationMessage::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        let now = current_time_millis();
        let session_id = object
            .get("session_id")
            .and_then(JsonValue::as_str)
            .map_or_else(generate_session_id, ToOwned::to_owned);
        let created_at_ms = object
            .get("created_at_ms")
            .map(|value| required_u64_from_value(value, "created_at_ms"))
            .transpose()?
            .unwrap_or(now);
        let updated_at_ms = object
            .get("updated_at_ms")
            .map(|value| required_u64_from_value(value, "updated_at_ms"))
            .transpose()?
            .unwrap_or(created_at_ms);
        let compaction = object
            .get("compaction")
            .map(SessionCompaction::from_json)
            .transpose()?;
        let fork = object.get("fork").map(SessionFork::from_json).transpose()?;
        let workspace_root = object
            .get("workspace_root")
            .and_then(JsonValue::as_str)
            .map(PathBuf::from);
        let prompt_history = object
            .get("prompt_history")
            .and_then(JsonValue::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(SessionPromptEntry::from_json_opt)
                    .collect()
            })
            .unwrap_or_default();
        let turns = object
            .get("turns")
            .and_then(JsonValue::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(SessionTurn::from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let runtime_context = object
            .get("runtime_context")
            .map(SessionRuntimeContext::from_json)
            .transpose()?
            .or_else(|| {
                compaction
                    .as_ref()
                    .and_then(|value| value.runtime_context.clone())
            });
        let context_baseline = object
            .get("context_baseline")
            .or_else(|| object.get("reference_context_item"))
            .map(SessionContextBaseline::from_json)
            .transpose()?
            .or_else(|| {
                compaction
                    .as_ref()
                    .and_then(|value| value.context_baseline.clone())
            });
        let model = object
            .get("model")
            .and_then(JsonValue::as_str)
            .map(String::from);
        let user_id = object
            .get("user_id")
            .and_then(JsonValue::as_str)
            .map(String::from);
        let tenant_id = object
            .get("tenant_id")
            .and_then(JsonValue::as_str)
            .map(String::from);
        Ok(Self {
            version,
            session_id,
            created_at_ms,
            updated_at_ms,
            messages,
            compaction,
            fork,
            workspace_root,
            prompt_history,
            turns,
            runtime_context,
            context_baseline,
            last_health_check_ms: None,
            model,
            user_id,
            tenant_id,
            persistence: None,
        })
    }

    fn from_jsonl(contents: &str) -> Result<Self, SessionError> {
        let mut version = SESSION_VERSION;
        let mut session_id = None;
        let mut created_at_ms = None;
        let mut updated_at_ms = None;
        let mut replay_records = Vec::new();
        let mut compaction = None;
        let mut fork = None;
        let mut workspace_root = None;
        let mut model = None;
        let mut user_id = None;
        let mut tenant_id = None;
        let mut prompt_history = Vec::new();
        let mut runtime_context = None;
        let mut context_baseline = None;
        for (line_number, raw_line) in contents.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let value = JsonValue::parse(line).map_err(|error| {
                SessionError::Format(format!(
                    "invalid JSONL record at line {}: {}",
                    line_number + 1,
                    error
                ))
            })?;
            let Some(object) = value.as_object() else {
                continue;
            };
            Self::parse_jsonl_record(
                object,
                line_number + 1,
                &mut version,
                &mut session_id,
                &mut created_at_ms,
                &mut updated_at_ms,
                &mut replay_records,
                &mut compaction,
                &mut fork,
                &mut workspace_root,
                &mut model,
                &mut user_id,
                &mut tenant_id,
                &mut prompt_history,
                &mut runtime_context,
                &mut context_baseline,
            )?;
        }

        let now = current_time_millis();
        let (messages, replayed_turns) = reconstruct_session_from_replay(&replay_records);
        let replayed_runtime_context = replay_runtime_context_from(&replay_records)
            .or(runtime_context)
            .or_else(|| {
                compaction
                    .as_ref()
                    .and_then(|value| value.runtime_context.clone())
            });
        let replayed_context_baseline = replay_context_baseline_from(&replay_records)
            .or(context_baseline)
            .or_else(|| {
                compaction
                    .as_ref()
                    .and_then(|value| value.context_baseline.clone())
            });
        Ok(Self {
            version,
            session_id: session_id.unwrap_or_else(generate_session_id),
            created_at_ms: created_at_ms.unwrap_or(now),
            updated_at_ms: updated_at_ms.unwrap_or(created_at_ms.unwrap_or(now)),
            messages,
            compaction,
            fork,
            workspace_root,
            prompt_history,
            turns: replayed_turns,
            runtime_context: replayed_runtime_context,
            context_baseline: replayed_context_baseline,
            last_health_check_ms: None,
            model,
            user_id,
            tenant_id,
            persistence: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_jsonl_record(
        object: &BTreeMap<String, crate::json::JsonValue>,
        line_number: usize,
        version: &mut u32,
        session_id: &mut Option<String>,
        created_at_ms: &mut Option<u64>,
        updated_at_ms: &mut Option<u64>,
        replay_records: &mut Vec<SessionReplayRecord>,
        compaction: &mut Option<SessionCompaction>,
        fork: &mut Option<SessionFork>,
        workspace_root: &mut Option<PathBuf>,
        model: &mut Option<String>,
        user_id: &mut Option<String>,
        tenant_id: &mut Option<String>,
        prompt_history: &mut Vec<SessionPromptEntry>,
        runtime_context: &mut Option<SessionRuntimeContext>,
        context_baseline: &mut Option<SessionContextBaseline>,
    ) -> Result<(), SessionError> {
        let record_type = object
            .get("type")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                SessionError::Format(format!("JSONL record at line {line_number} missing type"))
            })?;

        match record_type {
            "session_meta" => {
                *version = required_u32(object, "version")?;
                *session_id = Some(required_string(object, "session_id")?);
                *created_at_ms = Some(required_u64(object, "created_at_ms")?);
                *updated_at_ms = Some(required_u64(object, "updated_at_ms")?);
                *fork = object.get("fork").map(SessionFork::from_json).transpose()?;
                *workspace_root = object
                    .get("workspace_root")
                    .and_then(JsonValue::as_str)
                    .map(PathBuf::from);
                *model = object
                    .get("model")
                    .and_then(JsonValue::as_str)
                    .map(String::from);
                *user_id = object
                    .get("user_id")
                    .and_then(JsonValue::as_str)
                    .map(String::from);
                *tenant_id = object
                    .get("tenant_id")
                    .and_then(JsonValue::as_str)
                    .map(String::from);
                *runtime_context = object
                    .get("runtime_context")
                    .map(SessionRuntimeContext::from_json)
                    .transpose()?;
                *context_baseline = object
                    .get("context_baseline")
                    .map(SessionContextBaseline::from_json)
                    .transpose()?;
            }
            "runtime_context" | "turn_context" | "world_state" => {
                let parsed = SessionRuntimeContext::from_json(&JsonValue::Object(object.clone()))?;
                replay_records.push(SessionReplayRecord::RuntimeContext(parsed.clone()));
                *runtime_context = Some(parsed);
            }
            "context_baseline" | "reference_context_item" => {
                let parsed = SessionContextBaseline::from_json(&JsonValue::Object(object.clone()))?;
                replay_records.push(SessionReplayRecord::ContextBaseline(parsed.clone()));
                *context_baseline = Some(parsed);
            }
            "message" => {
                let message_value = object.get("message").ok_or_else(|| {
                    SessionError::Format(format!(
                        "JSONL record at line {line_number} missing message"
                    ))
                })?;
                replay_records.push(SessionReplayRecord::Message(
                    ConversationMessage::from_json(message_value)?,
                ));
            }
            "compaction" => {
                let mut parsed = SessionCompaction::from_json(&JsonValue::Object(object.clone()))?;
                if let Some(previous) = compaction.as_ref() {
                    merge_archive_windows(&mut parsed, &previous.archive_windows);
                }
                replay_records.push(SessionReplayRecord::Compaction(parsed.clone()));
                *compaction = Some(parsed);
            }
            "rollback" => {
                replay_records.push(SessionReplayRecord::RollbackTo {
                    message_count: required_usize_any(
                        object,
                        &["message_count", "target_message_count"],
                    )?,
                });
            }
            "turn_started" => {
                let turn = SessionTurn::from_json(&JsonValue::Object(object.clone()))?;
                replay_records.push(SessionReplayRecord::TurnStarted(turn));
            }
            "turn_completed" => {
                let turn_id = required_string(object, "turn_id")?;
                let end_message_count = required_usize(object, "end_message_count")?;
                let status = object
                    .get("status")
                    .and_then(JsonValue::as_str)
                    .map(SessionTurnStatus::from_str)
                    .transpose()?
                    .unwrap_or(SessionTurnStatus::Completed);
                replay_records.push(SessionReplayRecord::TurnCompleted {
                    turn_id: turn_id.clone(),
                    end_message_count,
                    status,
                });
            }
            "turn_rollback" => {
                if let Some(turn_id) = object.get("turn_id").and_then(JsonValue::as_str) {
                    let message_count =
                        required_usize_any(object, &["message_count", "target_message_count"])?;
                    replay_records.push(SessionReplayRecord::RollbackTurn {
                        turn_id: turn_id.to_string(),
                        message_count,
                    });
                } else {
                    let turn_count = required_usize_any(object, &["turn_count", "num_turns"])?;
                    replay_records.push(SessionReplayRecord::RollbackTurns { turn_count });
                }
            }
            "prompt_history" => {
                if let Some(entry) =
                    SessionPromptEntry::from_json_opt(&JsonValue::Object(object.clone()))
                {
                    prompt_history.push(entry);
                }
            }
            other => {
                return Err(SessionError::Format(format!(
                    "unsupported JSONL record type at line {line_number}: {other}"
                )));
            }
        }
        Ok(())
    }

    /// Record a user prompt with the current wall-clock timestamp.
    ///
    /// The entry is appended to the in-memory history and, when a persistence
    /// path is configured, incrementally written to the JSONL session file.
    pub fn push_prompt_entry(&mut self, text: impl Into<String>) -> Result<(), SessionError> {
        let timestamp_ms = current_time_millis();
        let entry = SessionPromptEntry {
            timestamp_ms,
            text: text.into(),
        };
        self.prompt_history.push(entry);
        let Some(entry_ref) = self.prompt_history.last() else {
            return Err(SessionError::Format(
                "prompt history append did not retain the new entry".to_string(),
            ));
        };
        self.append_persisted_prompt_entry(entry_ref)
    }

    fn render_jsonl_snapshot(&self) -> Result<String, SessionError> {
        let mut lines = vec![self.meta_record()?.render()];
        lines.extend(
            self.prompt_history
                .iter()
                .map(|entry| entry.to_jsonl_record().render()),
        );
        if let Some(runtime_context) = &self.runtime_context {
            lines.push(runtime_context.to_jsonl_record()?.render());
        }
        if let Some(context_baseline) = &self.context_baseline {
            lines.push(context_baseline.to_jsonl_record()?.render());
        }
        lines.extend(
            self.messages
                .iter()
                .map(|message| message_record(message).render()),
        );
        lines.extend(self.turns.iter().map(|turn| turn.to_jsonl_start().render()));
        lines.extend(
            self.turns
                .iter()
                .filter_map(|turn| turn.to_jsonl_completion().map(|value| value.render())),
        );
        if let Some(compaction) = &self.compaction {
            lines.push(
                compaction
                    .to_jsonl_record_with_replacement(&self.messages)?
                    .render(),
            );
        }
        let mut rendered = lines.join("\n");
        rendered.push('\n');
        Ok(protect_session_persistence_text(&rendered))
    }

    fn append_persisted_runtime_context(
        &self,
        context: &SessionRuntimeContext,
    ) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &context.to_jsonl_record()?.render())?;
        Ok(())
    }

    fn append_persisted_context_baseline(
        &self,
        baseline: &SessionContextBaseline,
    ) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &baseline.to_jsonl_record()?.render())?;
        Ok(())
    }

    fn append_persisted_message(&self, message: &ConversationMessage) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };

        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }

        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &message_record(message).render())?;
        Ok(())
    }

    fn append_persisted_prompt_entry(
        &self,
        entry: &SessionPromptEntry,
    ) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };

        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }

        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &entry.to_jsonl_record().render())?;
        Ok(())
    }

    fn append_persisted_rollback(&self, message_count: usize) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };

        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }

        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("rollback".to_string()),
        );
        object.insert(
            "message_count".to_string(),
            JsonValue::Number(i64_from_usize(message_count, "message_count")?),
        );
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &JsonValue::Object(object).render())?;
        Ok(())
    }

    fn append_persisted_turn_rollback(&self, turn_count: usize) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("turn_rollback".to_string()),
        );
        object.insert(
            "turn_count".to_string(),
            JsonValue::Number(i64_from_usize(turn_count, "turn_count")?),
        );
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &JsonValue::Object(object).render())?;
        Ok(())
    }

    fn append_persisted_turn_rollback_to(
        &self,
        turn_id: &str,
        message_count: usize,
    ) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("turn_rollback".to_string()),
        );
        object.insert(
            "turn_id".to_string(),
            JsonValue::String(turn_id.to_string()),
        );
        object.insert(
            "message_count".to_string(),
            JsonValue::Number(i64_from_usize(message_count, "message_count")?),
        );
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &JsonValue::Object(object).render())?;
        Ok(())
    }

    fn append_persisted_turn_started(&self, turn: &SessionTurn) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(&mut file, &turn.to_jsonl_start().render())?;
        Ok(())
    }

    fn append_persisted_turn_completed(
        &self,
        turn_id: &str,
        end_message_count: usize,
        status: SessionTurnStatus,
    ) -> Result<(), SessionError> {
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let needs_bootstrap = !path.exists() || fs::metadata(path)?.len() == 0;
        if needs_bootstrap {
            self.save_to_path(path)?;
            return Ok(());
        }
        let mut file = OpenOptions::new().append(true).open(path)?;
        write_protected_jsonl_record(
            &mut file,
            &turn_completion_record(turn_id, end_message_count, status)?.render(),
        )?;
        Ok(())
    }

    fn meta_record(&self) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("session_meta".to_string()),
        );
        object.insert(
            "version".to_string(),
            JsonValue::Number(i64::from(self.version)),
        );
        object.insert(
            "session_id".to_string(),
            JsonValue::String(self.session_id.clone()),
        );
        object.insert(
            "created_at_ms".to_string(),
            JsonValue::Number(i64_from_u64(self.created_at_ms, "created_at_ms")?),
        );
        object.insert(
            "updated_at_ms".to_string(),
            JsonValue::Number(i64_from_u64(self.updated_at_ms, "updated_at_ms")?),
        );
        if let Some(fork) = &self.fork {
            object.insert("fork".to_string(), fork.to_json());
        }
        if let Some(workspace_root) = &self.workspace_root {
            object.insert(
                "workspace_root".to_string(),
                JsonValue::String(workspace_root_to_string(workspace_root)?),
            );
        }
        if let Some(model) = &self.model {
            object.insert("model".to_string(), JsonValue::String(model.clone()));
        }
        if let Some(user_id) = &self.user_id {
            object.insert("user_id".to_string(), JsonValue::String(user_id.clone()));
        }
        if let Some(tenant_id) = &self.tenant_id {
            object.insert(
                "tenant_id".to_string(),
                JsonValue::String(tenant_id.clone()),
            );
        }
        Ok(JsonValue::Object(object))
    }

    fn touch(&mut self) {
        self.updated_at_ms = current_time_millis();
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text { text: text.into() }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }
    }

    #[must_use]
    pub fn assistant(blocks: Vec<ContentBlock>) -> Self {
        Self {
            role: MessageRole::Assistant,
            blocks,
            thinking: None,
            thinking_signature: None,
            usage: None,
        }
    }

    #[must_use]
    pub fn assistant_with_usage(blocks: Vec<ContentBlock>, usage: Option<TokenUsage>) -> Self {
        Self {
            role: MessageRole::Assistant,
            blocks,
            thinking: None,
            thinking_signature: None,
            usage,
        }
    }

    #[must_use]
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        tool_name: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                tool_name: tool_name.into(),
                output: output.into(),
                is_error,
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "role".to_string(),
            JsonValue::String(
                match self.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                }
                .to_string(),
            ),
        );
        object.insert(
            "blocks".to_string(),
            JsonValue::Array(self.blocks.iter().map(ContentBlock::to_json).collect()),
        );
        if let Some(ref thinking) = self.thinking {
            object.insert("thinking".to_string(), JsonValue::String(thinking.clone()));
        }
        if let Some(ref signature) = self.thinking_signature {
            object.insert(
                "thinking_signature".to_string(),
                JsonValue::String(signature.clone()),
            );
        }
        if let Some(usage) = self.usage {
            object.insert("usage".to_string(), usage_to_json(usage));
        }
        JsonValue::Object(object)
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("message must be an object".to_string()))?;
        let role = match object
            .get("role")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SessionError::Format("missing role".to_string()))?
        {
            "system" => MessageRole::System,
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            other => {
                return Err(SessionError::Format(format!(
                    "unsupported message role: {other}"
                )))
            }
        };
        let blocks = object
            .get("blocks")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| SessionError::Format("missing blocks".to_string()))?
            .iter()
            .map(ContentBlock::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        let thinking = object
            .get("thinking")
            .and_then(JsonValue::as_str)
            .map(String::from);
        let thinking_signature = object
            .get("thinking_signature")
            .and_then(JsonValue::as_str)
            .map(String::from);
        let usage = object.get("usage").map(usage_from_json).transpose()?;
        Ok(Self {
            role,
            blocks,
            thinking,
            thinking_signature,
            usage,
        })
    }
}

impl ContentBlock {
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        match self {
            Self::Text { text } => {
                object.insert("type".to_string(), JsonValue::String("text".to_string()));
                object.insert("text".to_string(), JsonValue::String(text.clone()));
            }
            Self::ToolUse { id, name, input } => {
                object.insert(
                    "type".to_string(),
                    JsonValue::String("tool_use".to_string()),
                );
                object.insert("id".to_string(), JsonValue::String(id.clone()));
                object.insert("name".to_string(), JsonValue::String(name.clone()));
                object.insert("input".to_string(), JsonValue::String(input.clone()));
            }
            Self::ToolResult {
                tool_use_id,
                tool_name,
                output,
                is_error,
            } => {
                object.insert(
                    "type".to_string(),
                    JsonValue::String("tool_result".to_string()),
                );
                object.insert(
                    "tool_use_id".to_string(),
                    JsonValue::String(tool_use_id.clone()),
                );
                object.insert(
                    "tool_name".to_string(),
                    JsonValue::String(tool_name.clone()),
                );
                object.insert("output".to_string(), JsonValue::String(output.clone()));
                object.insert("is_error".to_string(), JsonValue::Bool(*is_error));
            }
        }
        JsonValue::Object(object)
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("block must be an object".to_string()))?;
        match object
            .get("type")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SessionError::Format("missing block type".to_string()))?
        {
            "text" => Ok(Self::Text {
                text: required_string(object, "text")?,
            }),
            "tool_use" => Ok(Self::ToolUse {
                id: required_string(object, "id")?,
                name: required_string(object, "name")?,
                input: required_string(object, "input")?,
            }),
            "tool_result" => Ok(Self::ToolResult {
                tool_use_id: required_string(object, "tool_use_id")?,
                tool_name: required_string(object, "tool_name")?,
                output: required_string(object, "output")?,
                is_error: object
                    .get("is_error")
                    .and_then(JsonValue::as_bool)
                    .ok_or_else(|| SessionError::Format("missing is_error".to_string()))?,
            }),
            other => Err(SessionError::Format(format!(
                "unsupported block type: {other}"
            ))),
        }
    }
}

impl SessionCompaction {
    pub fn to_json(&self) -> Result<JsonValue, SessionError> {
        self.to_json_with_replacement(&self.replacement_messages)
    }

    pub fn to_json_with_replacement(
        &self,
        replacement_messages: &[ConversationMessage],
    ) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        object.insert(
            "count".to_string(),
            JsonValue::Number(i64::from(self.count)),
        );
        object.insert(
            "removed_message_count".to_string(),
            JsonValue::Number(i64_from_usize(
                self.removed_message_count,
                "removed_message_count",
            )?),
        );
        object.insert(
            "summary".to_string(),
            JsonValue::String(self.summary.clone()),
        );
        object.insert(
            "window_number".to_string(),
            JsonValue::Number(i64_from_u64(self.window_number, "window_number")?),
        );
        object.insert(
            "first_window_id".to_string(),
            JsonValue::String(self.first_window_id.clone()),
        );
        object.insert(
            "window_id".to_string(),
            JsonValue::String(self.window_id.clone()),
        );
        if let Some(previous_window_id) = &self.previous_window_id {
            object.insert(
                "previous_window_id".to_string(),
                JsonValue::String(previous_window_id.clone()),
            );
        }
        if let Some(previous_window_number) = self.previous_window_number {
            object.insert(
                "previous_window_number".to_string(),
                JsonValue::Number(i64_from_u64(
                    previous_window_number,
                    "previous_window_number",
                )?),
            );
        }
        if !self.archived_messages.is_empty() {
            object.insert(
                "archived_messages".to_string(),
                JsonValue::Array(
                    self.archived_messages
                        .iter()
                        .map(ConversationMessage::to_json)
                        .collect(),
                ),
            );
        }
        if let Some(runtime_context) = &self.runtime_context {
            object.insert("runtime_context".to_string(), runtime_context.to_json()?);
        }
        if let Some(context_baseline) = &self.context_baseline {
            object.insert("context_baseline".to_string(), context_baseline.to_json()?);
        }
        if !self.archive_windows.is_empty() {
            object.insert(
                "archive_windows".to_string(),
                JsonValue::Array(
                    self.archive_windows
                        .iter()
                        .map(SessionArchiveWindow::to_json)
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            );
        }
        if !replacement_messages.is_empty() {
            object.insert(
                "replacement_messages".to_string(),
                JsonValue::Array(
                    replacement_messages
                        .iter()
                        .map(ConversationMessage::to_json)
                        .collect(),
                ),
            );
        }
        Ok(JsonValue::Object(object))
    }

    pub fn to_jsonl_record(&self) -> Result<JsonValue, SessionError> {
        self.to_jsonl_record_with_replacement(&self.replacement_messages)
    }

    pub fn to_jsonl_record_with_replacement(
        &self,
        replacement_messages: &[ConversationMessage],
    ) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("compaction".to_string()),
        );
        object.insert(
            "count".to_string(),
            JsonValue::Number(i64::from(self.count)),
        );
        object.insert(
            "removed_message_count".to_string(),
            JsonValue::Number(i64_from_usize(
                self.removed_message_count,
                "removed_message_count",
            )?),
        );
        object.insert(
            "summary".to_string(),
            JsonValue::String(self.summary.clone()),
        );
        object.insert(
            "window_number".to_string(),
            JsonValue::Number(i64_from_u64(self.window_number, "window_number")?),
        );
        object.insert(
            "first_window_id".to_string(),
            JsonValue::String(self.first_window_id.clone()),
        );
        object.insert(
            "window_id".to_string(),
            JsonValue::String(self.window_id.clone()),
        );
        if let Some(previous_window_id) = &self.previous_window_id {
            object.insert(
                "previous_window_id".to_string(),
                JsonValue::String(previous_window_id.clone()),
            );
        }
        if let Some(previous_window_number) = self.previous_window_number {
            object.insert(
                "previous_window_number".to_string(),
                JsonValue::Number(i64_from_u64(
                    previous_window_number,
                    "previous_window_number",
                )?),
            );
        }
        if !self.archived_messages.is_empty() {
            object.insert(
                "archived_messages".to_string(),
                JsonValue::Array(
                    self.archived_messages
                        .iter()
                        .map(ConversationMessage::to_json)
                        .collect(),
                ),
            );
        }
        if let Some(runtime_context) = &self.runtime_context {
            object.insert("runtime_context".to_string(), runtime_context.to_json()?);
        }
        if let Some(context_baseline) = &self.context_baseline {
            object.insert("context_baseline".to_string(), context_baseline.to_json()?);
        }
        if !self.archive_windows.is_empty() {
            object.insert(
                "archive_windows".to_string(),
                JsonValue::Array(
                    self.archive_windows
                        .iter()
                        .map(SessionArchiveWindow::to_json)
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            );
        }
        if !replacement_messages.is_empty() {
            object.insert(
                "replacement_messages".to_string(),
                JsonValue::Array(
                    replacement_messages
                        .iter()
                        .map(ConversationMessage::to_json)
                        .collect(),
                ),
            );
        }
        Ok(JsonValue::Object(object))
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("compaction must be an object".to_string()))?;
        let replacement_messages = object
            .get("replacement_messages")
            .or_else(|| object.get("replacement_history"))
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(ConversationMessage::from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let archived_messages = object
            .get("archived_messages")
            .or_else(|| object.get("archived_history"))
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(ConversationMessage::from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let runtime_context = object
            .get("runtime_context")
            .or_else(|| object.get("turn_context"))
            .or_else(|| object.get("world_state"))
            .map(SessionRuntimeContext::from_json)
            .transpose()?;
        let context_baseline = object
            .get("context_baseline")
            .or_else(|| object.get("reference_context_item"))
            .map(SessionContextBaseline::from_json)
            .transpose()?;
        let mut archive_windows = object
            .get("archive_windows")
            .or_else(|| object.get("archived_windows"))
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(SessionArchiveWindow::from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let count = required_u32(object, "count")?;
        let window_number = object
            .get("window_number")
            .map(|value| required_u64_from_value(value, "window_number"))
            .transpose()?
            .unwrap_or(u64::from(count).max(1));
        let window_id = object
            .get("window_id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("window-legacy-{window_number}"));
        let first_window_id = object
            .get("first_window_id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| window_id.clone());
        let previous_window_id = object
            .get("previous_window_id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned);
        let previous_window_number = object
            .get("previous_window_number")
            .map(|value| required_u64_from_value(value, "previous_window_number"))
            .transpose()?;
        if archive_windows.is_empty() && !archived_messages.is_empty() {
            archive_windows.push(SessionArchiveWindow {
                window_number,
                window_id: window_id.clone(),
                previous_window_id: previous_window_id.clone(),
                removed_message_count: required_usize(object, "removed_message_count")?,
                archived_messages: archived_messages.clone(),
            });
        }
        Ok(Self {
            count,
            removed_message_count: required_usize(object, "removed_message_count")?,
            summary: required_string(object, "summary")?,
            window_number,
            first_window_id,
            window_id,
            previous_window_id,
            previous_window_number,
            replacement_messages,
            runtime_context,
            context_baseline,
            archived_messages,
            archive_windows,
        })
    }
}

impl SessionArchiveWindow {
    fn to_json(&self) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        object.insert(
            "window_number".to_string(),
            JsonValue::Number(i64_from_u64(self.window_number, "window_number")?),
        );
        object.insert(
            "window_id".to_string(),
            JsonValue::String(self.window_id.clone()),
        );
        if let Some(previous_window_id) = &self.previous_window_id {
            object.insert(
                "previous_window_id".to_string(),
                JsonValue::String(previous_window_id.clone()),
            );
        }
        object.insert(
            "removed_message_count".to_string(),
            JsonValue::Number(i64_from_usize(
                self.removed_message_count,
                "removed_message_count",
            )?),
        );
        object.insert(
            "archived_messages".to_string(),
            JsonValue::Array(
                self.archived_messages
                    .iter()
                    .map(ConversationMessage::to_json)
                    .collect(),
            ),
        );
        Ok(JsonValue::Object(object))
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("archive window must be an object".to_string()))?;
        let archived_messages = object
            .get("archived_messages")
            .or_else(|| object.get("archived_history"))
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(ConversationMessage::from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            window_number: object
                .get("window_number")
                .map(|value| required_u64_from_value(value, "window_number"))
                .transpose()?
                .unwrap_or(1),
            window_id: object
                .get("window_id")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(generate_window_id),
            previous_window_id: object
                .get("previous_window_id")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned),
            removed_message_count: object
                .get("removed_message_count")
                .map(|_| required_usize(object, "removed_message_count"))
                .transpose()?
                .unwrap_or(archived_messages.len()),
            archived_messages,
        })
    }
}

impl SessionFork {
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "parent_session_id".to_string(),
            JsonValue::String(self.parent_session_id.clone()),
        );
        if let Some(branch_name) = &self.branch_name {
            object.insert(
                "branch_name".to_string(),
                JsonValue::String(branch_name.clone()),
            );
        }
        JsonValue::Object(object)
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("fork metadata must be an object".to_string()))?;
        Ok(Self {
            parent_session_id: required_string(object, "parent_session_id")?,
            branch_name: object
                .get("branch_name")
                .and_then(JsonValue::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

impl SessionPromptEntry {
    #[must_use]
    pub fn to_jsonl_record(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "type".to_string(),
            JsonValue::String("prompt_history".to_string()),
        );
        object.insert(
            "timestamp_ms".to_string(),
            JsonValue::Number(i64::try_from(self.timestamp_ms).unwrap_or(i64::MAX)),
        );
        object.insert("text".to_string(), JsonValue::String(self.text.clone()));
        JsonValue::Object(object)
    }

    fn from_json_opt(value: &JsonValue) -> Option<Self> {
        let object = value.as_object()?;
        let timestamp_ms = object
            .get("timestamp_ms")
            .and_then(JsonValue::as_i64)
            .and_then(|value| u64::try_from(value).ok())?;
        let text = object.get("text").and_then(JsonValue::as_str)?.to_string();
        Some(Self { timestamp_ms, text })
    }
}

impl SessionRuntimeContext {
    pub fn to_json(&self) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        insert_optional_string(&mut object, "turn_id", self.turn_id.as_deref());
        insert_optional_string(&mut object, "model", self.model.as_deref());
        insert_optional_string(&mut object, "provider", self.provider.as_deref());
        insert_optional_string(&mut object, "scenario", self.scenario.as_deref());
        insert_optional_string(
            &mut object,
            "reasoning_effort",
            self.reasoning_effort.as_deref(),
        );
        insert_optional_string(
            &mut object,
            "reasoning_budget",
            self.reasoning_budget.as_deref(),
        );
        insert_optional_u32(&mut object, "max_output_tokens", self.max_output_tokens);
        insert_optional_u64(&mut object, "stream_timeout_secs", self.stream_timeout_secs)?;
        object.insert(
            "disable_stream_timeout".to_string(),
            JsonValue::Bool(self.disable_stream_timeout),
        );
        object.insert(
            "prefer_native_web_search".to_string(),
            JsonValue::Bool(self.prefer_native_web_search),
        );
        object.insert(
            "suppress_native_web_search".to_string(),
            JsonValue::Bool(self.suppress_native_web_search),
        );
        object.insert(
            "tools_enabled".to_string(),
            JsonValue::Bool(self.tools_enabled),
        );
        object.insert(
            "allowed_tools".to_string(),
            string_array_to_json(&self.allowed_tools),
        );
        object.insert(
            "blocked_tools".to_string(),
            string_array_to_json(&self.blocked_tools),
        );
        object.insert(
            "mcp_servers".to_string(),
            string_array_to_json(&self.mcp_servers),
        );
        object.insert("skills".to_string(), string_array_to_json(&self.skills));
        object.insert(
            "memory_enabled".to_string(),
            JsonValue::Bool(self.memory_enabled),
        );
        object.insert(
            "file_context_enabled".to_string(),
            JsonValue::Bool(self.file_context_enabled),
        );
        object.insert(
            "pm_search_provider_count".to_string(),
            JsonValue::Number(i64::from(self.pm_search_provider_count)),
        );
        insert_optional_string(
            &mut object,
            "permission_mode",
            self.permission_mode.as_deref(),
        );
        insert_optional_u32(
            &mut object,
            "context_window_tokens",
            self.context_window_tokens,
        );
        insert_optional_string(
            &mut object,
            "system_prompt_hash",
            self.system_prompt_hash.as_deref(),
        );
        insert_optional_u32(
            &mut object,
            "system_prompt_section_count",
            self.system_prompt_section_count,
        );
        insert_optional_u64(&mut object, "window_number", self.window_number)?;
        insert_optional_string(&mut object, "window_id", self.window_id.as_deref());
        insert_optional_string(
            &mut object,
            "previous_window_id",
            self.previous_window_id.as_deref(),
        );
        if !self.metadata.is_empty() {
            object.insert(
                "metadata".to_string(),
                JsonValue::Object(
                    self.metadata
                        .iter()
                        .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                        .collect(),
                ),
            );
        }
        Ok(JsonValue::Object(object))
    }

    fn to_jsonl_record(&self) -> Result<JsonValue, SessionError> {
        let mut object = self
            .to_json()?
            .as_object()
            .expect("runtime context json should be object")
            .clone();
        object.insert(
            "type".to_string(),
            JsonValue::String("runtime_context".to_string()),
        );
        Ok(JsonValue::Object(object))
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("runtime context must be an object".to_string()))?;
        let metadata = object
            .get("metadata")
            .and_then(JsonValue::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            turn_id: optional_string(object, "turn_id"),
            model: optional_string(object, "model"),
            provider: optional_string(object, "provider"),
            scenario: optional_string(object, "scenario"),
            reasoning_effort: optional_string(object, "reasoning_effort"),
            reasoning_budget: optional_string(object, "reasoning_budget"),
            max_output_tokens: optional_u32(object, "max_output_tokens")?,
            stream_timeout_secs: optional_u64(object, "stream_timeout_secs")?,
            disable_stream_timeout: optional_bool(object, "disable_stream_timeout"),
            prefer_native_web_search: optional_bool(object, "prefer_native_web_search"),
            suppress_native_web_search: optional_bool(object, "suppress_native_web_search"),
            tools_enabled: object
                .get("tools_enabled")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
            allowed_tools: optional_string_array(object, "allowed_tools"),
            blocked_tools: optional_string_array(object, "blocked_tools"),
            mcp_servers: optional_string_array(object, "mcp_servers"),
            skills: optional_string_array(object, "skills"),
            memory_enabled: optional_bool(object, "memory_enabled"),
            file_context_enabled: optional_bool(object, "file_context_enabled"),
            pm_search_provider_count: optional_u32(object, "pm_search_provider_count")?
                .unwrap_or_default(),
            permission_mode: optional_string(object, "permission_mode"),
            context_window_tokens: optional_u32(object, "context_window_tokens")?,
            system_prompt_hash: optional_string(object, "system_prompt_hash"),
            system_prompt_section_count: optional_u32(object, "system_prompt_section_count")?,
            window_number: optional_u64(object, "window_number")?,
            window_id: optional_string(object, "window_id"),
            previous_window_id: optional_string(object, "previous_window_id"),
            metadata,
        })
    }
}

impl SessionContextBaseline {
    pub fn to_json(&self) -> Result<JsonValue, SessionError> {
        let mut object = BTreeMap::new();
        insert_optional_string(&mut object, "turn_id", self.turn_id.as_deref());
        insert_optional_string(&mut object, "label", self.label.as_deref());
        insert_optional_string(
            &mut object,
            "system_prompt_hash",
            self.system_prompt_hash.as_deref(),
        );
        object.insert(
            "system_prompt_sections".to_string(),
            string_array_to_json(&self.system_prompt_sections),
        );
        object.insert(
            "system_prompt_section_hashes".to_string(),
            string_array_to_json(&self.system_prompt_section_hashes),
        );
        object.insert(
            "tool_names".to_string(),
            string_array_to_json(&self.tool_names),
        );
        object.insert(
            "mcp_servers".to_string(),
            string_array_to_json(&self.mcp_servers),
        );
        object.insert("skills".to_string(), string_array_to_json(&self.skills));
        object.insert(
            "memory_enabled".to_string(),
            JsonValue::Bool(self.memory_enabled),
        );
        object.insert(
            "file_context_enabled".to_string(),
            JsonValue::Bool(self.file_context_enabled),
        );
        object.insert(
            "search_provider_count".to_string(),
            JsonValue::Number(i64::from(self.search_provider_count)),
        );
        insert_optional_u64(&mut object, "window_number", self.window_number)?;
        insert_optional_string(&mut object, "window_id", self.window_id.as_deref());
        insert_optional_string(
            &mut object,
            "previous_window_id",
            self.previous_window_id.as_deref(),
        );
        if !self.metadata.is_empty() {
            object.insert("metadata".to_string(), string_map_to_json(&self.metadata));
        }
        Ok(JsonValue::Object(object))
    }

    fn to_jsonl_record(&self) -> Result<JsonValue, SessionError> {
        let mut object = self
            .to_json()?
            .as_object()
            .expect("context baseline json should be object")
            .clone();
        object.insert(
            "type".to_string(),
            JsonValue::String("context_baseline".to_string()),
        );
        Ok(JsonValue::Object(object))
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value.as_object().ok_or_else(|| {
            SessionError::Format("context baseline must be an object".to_string())
        })?;
        Ok(Self {
            turn_id: optional_string(object, "turn_id"),
            label: optional_string(object, "label"),
            system_prompt_hash: optional_string(object, "system_prompt_hash")
                .or_else(|| optional_string(object, "prompt_hash")),
            system_prompt_sections: optional_string_array(object, "system_prompt_sections")
                .into_iter()
                .chain(optional_string_array(object, "sections"))
                .collect(),
            system_prompt_section_hashes: optional_string_array(
                object,
                "system_prompt_section_hashes",
            )
            .into_iter()
            .chain(optional_string_array(object, "section_hashes"))
            .collect(),
            tool_names: optional_string_array(object, "tool_names")
                .into_iter()
                .chain(optional_string_array(object, "tools"))
                .collect(),
            mcp_servers: optional_string_array(object, "mcp_servers"),
            skills: optional_string_array(object, "skills"),
            memory_enabled: optional_bool(object, "memory_enabled"),
            file_context_enabled: optional_bool(object, "file_context_enabled"),
            search_provider_count: optional_u32(object, "search_provider_count")?
                .or_else(|| {
                    optional_u32(object, "pm_search_provider_count")
                        .ok()
                        .flatten()
                })
                .unwrap_or_default(),
            window_number: optional_u64(object, "window_number")?,
            window_id: optional_string(object, "window_id"),
            previous_window_id: optional_string(object, "previous_window_id"),
            metadata: optional_string_map(object, "metadata"),
        })
    }
}

impl SessionTurnStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Suspended => "suspended",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }

    fn from_str(value: &str) -> Result<Self, SessionError> {
        match value {
            "running" => Ok(Self::Running),
            "suspended" => Ok(Self::Suspended),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "rolled_back" => Ok(Self::RolledBack),
            other => Err(SessionError::Format(format!(
                "unsupported turn status: {other}"
            ))),
        }
    }
}

impl SessionTurn {
    fn to_json(&self) -> JsonValue {
        let mut object = BTreeMap::new();
        object.insert(
            "turn_id".to_string(),
            JsonValue::String(self.turn_id.clone()),
        );
        object.insert(
            "user_input".to_string(),
            JsonValue::String(self.user_input.clone()),
        );
        object.insert(
            "start_message_count".to_string(),
            JsonValue::Number(i64::try_from(self.start_message_count).unwrap_or(i64::MAX)),
        );
        if let Some(end_message_count) = self.end_message_count {
            object.insert(
                "end_message_count".to_string(),
                JsonValue::Number(i64::try_from(end_message_count).unwrap_or(i64::MAX)),
            );
        }
        object.insert(
            "status".to_string(),
            JsonValue::String(self.status.as_str().to_string()),
        );
        if let Some(runtime_context) = &self.runtime_context {
            if let Ok(value) = runtime_context.to_json() {
                object.insert("runtime_context".to_string(), value);
            }
        }
        if let Some(context_baseline) = &self.context_baseline {
            if let Ok(value) = context_baseline.to_json() {
                object.insert("context_baseline".to_string(), value);
            }
        }
        JsonValue::Object(object)
    }

    fn to_jsonl_start(&self) -> JsonValue {
        let mut object = self
            .to_json()
            .as_object()
            .expect("turn json should be object")
            .clone();
        object.insert(
            "type".to_string(),
            JsonValue::String("turn_started".to_string()),
        );
        object.remove("end_message_count");
        object.insert(
            "status".to_string(),
            JsonValue::String(SessionTurnStatus::Running.as_str().to_string()),
        );
        JsonValue::Object(object)
    }

    fn to_jsonl_completion(&self) -> Option<JsonValue> {
        let end_message_count = self.end_message_count?;
        turn_completion_record(&self.turn_id, end_message_count, self.status).ok()
    }

    fn from_json(value: &JsonValue) -> Result<Self, SessionError> {
        let object = value
            .as_object()
            .ok_or_else(|| SessionError::Format("turn must be an object".to_string()))?;
        Ok(Self {
            turn_id: required_string(object, "turn_id")?,
            user_input: required_string(object, "user_input")?,
            start_message_count: required_usize(object, "start_message_count")?,
            end_message_count: object
                .get("end_message_count")
                .map(|_| required_usize(object, "end_message_count"))
                .transpose()?,
            status: object
                .get("status")
                .and_then(JsonValue::as_str)
                .map(SessionTurnStatus::from_str)
                .transpose()?
                .unwrap_or(SessionTurnStatus::Running),
            runtime_context: object
                .get("runtime_context")
                .or_else(|| object.get("turn_context"))
                .map(SessionRuntimeContext::from_json)
                .transpose()?,
            context_baseline: object
                .get("context_baseline")
                .or_else(|| object.get("reference_context_item"))
                .map(SessionContextBaseline::from_json)
                .transpose()?,
        })
    }
}

fn message_record(message: &ConversationMessage) -> JsonValue {
    let mut object = BTreeMap::new();
    object.insert("type".to_string(), JsonValue::String("message".to_string()));
    object.insert("message".to_string(), message.to_json());
    JsonValue::Object(object)
}

fn protect_session_persistence_text(value: &str) -> String {
    crate::protect_sensitive_text(value, crate::configured_data_protection_mode()).value
}

fn write_protected_jsonl_record(
    writer: &mut impl Write,
    rendered: &str,
) -> Result<(), std::io::Error> {
    writeln!(writer, "{}", protect_session_persistence_text(rendered))
}

fn turn_completion_record(
    turn_id: &str,
    end_message_count: usize,
    status: SessionTurnStatus,
) -> Result<JsonValue, SessionError> {
    let mut object = BTreeMap::new();
    object.insert(
        "type".to_string(),
        JsonValue::String("turn_completed".to_string()),
    );
    object.insert(
        "turn_id".to_string(),
        JsonValue::String(turn_id.to_string()),
    );
    object.insert(
        "end_message_count".to_string(),
        JsonValue::Number(i64_from_usize(end_message_count, "end_message_count")?),
    );
    object.insert(
        "status".to_string(),
        JsonValue::String(status.as_str().to_string()),
    );
    Ok(JsonValue::Object(object))
}

fn reconstruct_session_from_replay(
    records: &[SessionReplayRecord],
) -> (Vec<ConversationMessage>, Vec<SessionTurn>) {
    let messages = reconstruct_messages_from_replay(records);
    let turns = replay_turns_from(records, messages.len());
    (messages, turns)
}

fn reconstruct_messages_from_replay(records: &[SessionReplayRecord]) -> Vec<ConversationMessage> {
    let Some((checkpoint_index, checkpoint)) = newest_replacement_checkpoint(records)
        .map(|(index, compaction)| (index, compaction.replacement_messages.as_slice().to_vec()))
    else {
        return replay_records_from(Vec::new(), records);
    };

    let suffix = &records[(checkpoint_index + 1)..];
    let replayed = replay_records_from(checkpoint.clone(), suffix);

    // Snapshot files may contain compacted messages immediately before a final
    // compaction record that stores the same replacement history. In that case
    // replaying from the checkpoint alone is correct. Append-style logs, on the
    // other hand, place new suffix events after the checkpoint, which are kept.
    if replayed == checkpoint {
        checkpoint
    } else {
        replayed
    }
}

fn merge_archive_windows(
    compaction: &mut SessionCompaction,
    previous_windows: &[SessionArchiveWindow],
) {
    if previous_windows.is_empty() {
        return;
    }
    let mut merged = previous_windows.to_vec();
    for window in &compaction.archive_windows {
        if !merged
            .iter()
            .any(|existing| existing.window_id == window.window_id)
        {
            merged.push(window.clone());
        }
    }
    compaction.archive_windows = merged;
}

fn newest_replacement_checkpoint(
    records: &[SessionReplayRecord],
) -> Option<(usize, &SessionCompaction)> {
    records
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, record)| match record {
            SessionReplayRecord::Compaction(compaction)
                if !compaction.replacement_messages.is_empty() =>
            {
                Some((index, compaction))
            }
            SessionReplayRecord::Message(_)
            | SessionReplayRecord::Compaction(_)
            | SessionReplayRecord::RuntimeContext(_)
            | SessionReplayRecord::ContextBaseline(_)
            | SessionReplayRecord::TurnStarted(_)
            | SessionReplayRecord::TurnCompleted { .. }
            | SessionReplayRecord::RollbackTo { .. }
            | SessionReplayRecord::RollbackTurn { .. }
            | SessionReplayRecord::RollbackTurns { .. } => None,
        })
}

fn replay_records_from(
    mut messages: Vec<ConversationMessage>,
    records: &[SessionReplayRecord],
) -> Vec<ConversationMessage> {
    let mut turns: Vec<SessionTurn> = Vec::new();
    for record in records {
        match record {
            SessionReplayRecord::Message(message) => messages.push(message.clone()),
            SessionReplayRecord::Compaction(compaction) => {
                if !compaction.replacement_messages.is_empty() {
                    messages = compaction.replacement_messages.clone();
                }
            }
            SessionReplayRecord::RuntimeContext(_) => {}
            SessionReplayRecord::ContextBaseline(_) => {}
            SessionReplayRecord::RollbackTo { message_count } => {
                messages.truncate((*message_count).min(messages.len()));
                for turn in &mut turns {
                    if turn.start_message_count >= *message_count
                        || turn
                            .end_message_count
                            .is_some_and(|end| end > *message_count)
                    {
                        turn.end_message_count = Some(*message_count);
                        turn.status = SessionTurnStatus::RolledBack;
                    }
                }
            }
            SessionReplayRecord::RollbackTurn {
                turn_id,
                message_count,
            } => {
                messages.truncate((*message_count).min(messages.len()));
                apply_turn_rollback_to(&mut turns, turn_id, *message_count);
            }
            SessionReplayRecord::RollbackTurns { turn_count } => {
                if *turn_count > 0 {
                    if let Some(message_count) =
                        rollback_message_count_for_last_turns(&turns, *turn_count)
                    {
                        messages.truncate(message_count.min(messages.len()));
                    }
                    apply_turn_rollback_count(&mut turns, *turn_count);
                }
            }
            SessionReplayRecord::TurnStarted(turn) => turns.push(turn.clone()),
            SessionReplayRecord::TurnCompleted {
                turn_id,
                end_message_count,
                status,
            } => apply_turn_completion(&mut turns, turn_id, *end_message_count, *status),
        }
    }
    messages
}

fn replay_turns_from(
    records: &[SessionReplayRecord],
    final_message_count: usize,
) -> Vec<SessionTurn> {
    let mut turns = Vec::new();
    for record in records {
        match record {
            SessionReplayRecord::TurnStarted(turn) => {
                turns.push(turn.clone());
            }
            SessionReplayRecord::TurnCompleted {
                turn_id,
                end_message_count,
                status,
            } => {
                apply_turn_completion(&mut turns, turn_id, *end_message_count, *status);
            }
            SessionReplayRecord::RollbackTo { message_count } => {
                for turn in &mut turns {
                    if turn.start_message_count >= *message_count
                        || turn
                            .end_message_count
                            .is_some_and(|end| end > *message_count)
                    {
                        turn.end_message_count = Some(*message_count);
                        turn.status = SessionTurnStatus::RolledBack;
                    }
                }
            }
            SessionReplayRecord::RollbackTurn {
                turn_id,
                message_count,
            } => apply_turn_rollback_to(&mut turns, turn_id, *message_count),
            SessionReplayRecord::RollbackTurns { turn_count } => {
                apply_turn_rollback_count(&mut turns, *turn_count);
            }
            SessionReplayRecord::Message(_)
            | SessionReplayRecord::Compaction(_)
            | SessionReplayRecord::RuntimeContext(_)
            | SessionReplayRecord::ContextBaseline(_) => {}
        }
    }
    for turn in &mut turns {
        if turn.end_message_count.is_none() {
            turn.end_message_count = Some(final_message_count);
            turn.status = SessionTurnStatus::Failed;
        }
    }
    turns
}

fn replay_runtime_context_from(records: &[SessionReplayRecord]) -> Option<SessionRuntimeContext> {
    let mut fallback_contexts = Vec::<SessionRuntimeContext>::new();
    let mut turns = Vec::<SessionTurn>::new();
    for record in records {
        match record {
            SessionReplayRecord::RuntimeContext(context) => fallback_contexts.push(context.clone()),
            SessionReplayRecord::Compaction(compaction) => {
                if let Some(context) = &compaction.runtime_context {
                    fallback_contexts.push(context.clone());
                }
            }
            SessionReplayRecord::TurnStarted(turn) => {
                turns.push(turn.clone());
            }
            SessionReplayRecord::TurnCompleted {
                turn_id,
                end_message_count,
                status,
            } => apply_turn_completion(&mut turns, turn_id, *end_message_count, *status),
            SessionReplayRecord::RollbackTo { message_count } => {
                for turn in &mut turns {
                    if turn.start_message_count >= *message_count
                        || turn
                            .end_message_count
                            .is_some_and(|end| end > *message_count)
                    {
                        turn.end_message_count = Some(*message_count);
                        turn.status = SessionTurnStatus::RolledBack;
                    }
                }
            }
            SessionReplayRecord::RollbackTurn {
                turn_id,
                message_count,
            } => apply_turn_rollback_to(&mut turns, turn_id, *message_count),
            SessionReplayRecord::RollbackTurns { turn_count } => {
                apply_turn_rollback_count(&mut turns, *turn_count);
            }
            SessionReplayRecord::Message(_) => {}
            SessionReplayRecord::ContextBaseline(_) => {}
        }
    }
    turns
        .iter()
        .rev()
        .find(|turn| !matches!(turn.status, SessionTurnStatus::RolledBack))
        .and_then(|turn| turn.runtime_context.clone())
        .or_else(|| fallback_contexts.pop())
}

fn replay_context_baseline_from(records: &[SessionReplayRecord]) -> Option<SessionContextBaseline> {
    let mut fallback_baselines = Vec::<SessionContextBaseline>::new();
    let mut turns = Vec::<SessionTurn>::new();
    for record in records {
        match record {
            SessionReplayRecord::ContextBaseline(baseline) => {
                fallback_baselines.push(baseline.clone());
            }
            SessionReplayRecord::Compaction(compaction) => {
                if let Some(baseline) = &compaction.context_baseline {
                    fallback_baselines.push(baseline.clone());
                }
            }
            SessionReplayRecord::TurnStarted(turn) => {
                turns.push(turn.clone());
            }
            SessionReplayRecord::TurnCompleted {
                turn_id,
                end_message_count,
                status,
            } => apply_turn_completion(&mut turns, turn_id, *end_message_count, *status),
            SessionReplayRecord::RollbackTo { message_count } => {
                for turn in &mut turns {
                    if turn.start_message_count >= *message_count
                        || turn
                            .end_message_count
                            .is_some_and(|end| end > *message_count)
                    {
                        turn.end_message_count = Some(*message_count);
                        turn.status = SessionTurnStatus::RolledBack;
                    }
                }
            }
            SessionReplayRecord::RollbackTurn {
                turn_id,
                message_count,
            } => apply_turn_rollback_to(&mut turns, turn_id, *message_count),
            SessionReplayRecord::RollbackTurns { turn_count } => {
                apply_turn_rollback_count(&mut turns, *turn_count);
            }
            SessionReplayRecord::Message(_) | SessionReplayRecord::RuntimeContext(_) => {}
        }
    }
    turns
        .iter()
        .rev()
        .find(|turn| !matches!(turn.status, SessionTurnStatus::RolledBack))
        .and_then(|turn| turn.context_baseline.clone())
        .or_else(|| fallback_baselines.pop())
}

fn apply_turn_completion(
    turns: &mut [SessionTurn],
    turn_id: &str,
    end_message_count: usize,
    status: SessionTurnStatus,
) {
    if let Some(turn) = turns.iter_mut().rev().find(|turn| turn.turn_id == turn_id) {
        turn.end_message_count = Some(end_message_count);
        turn.status = status;
    }
}

fn apply_turn_rollback_to(turns: &mut [SessionTurn], turn_id: &str, message_count: usize) {
    if let Some(turn) = turns.iter_mut().rev().find(|turn| turn.turn_id == turn_id) {
        turn.end_message_count = Some(message_count);
        turn.status = SessionTurnStatus::RolledBack;
    }
}

fn apply_turn_rollback_count(turns: &mut [SessionTurn], turn_count: usize) {
    let mut remaining = turn_count;
    for turn in turns.iter_mut().rev() {
        if remaining == 0 {
            break;
        }
        if matches!(turn.status, SessionTurnStatus::RolledBack) {
            continue;
        }
        turn.end_message_count = Some(turn.start_message_count);
        turn.status = SessionTurnStatus::RolledBack;
        remaining -= 1;
    }
}

fn rollback_message_count_for_last_turns(
    turns: &[SessionTurn],
    turn_count: usize,
) -> Option<usize> {
    if turn_count == 0 {
        return None;
    }
    let mut remaining = turn_count;
    for turn in turns.iter().rev() {
        if matches!(turn.status, SessionTurnStatus::RolledBack) {
            continue;
        }
        remaining -= 1;
        if remaining == 0 {
            return Some(turn.start_message_count);
        }
    }
    None
}

fn usage_to_json(usage: TokenUsage) -> JsonValue {
    let mut object = BTreeMap::new();
    object.insert(
        "input_tokens".to_string(),
        JsonValue::Number(i64::from(usage.input_tokens)),
    );
    object.insert(
        "output_tokens".to_string(),
        JsonValue::Number(i64::from(usage.output_tokens)),
    );
    object.insert(
        "cache_creation_input_tokens".to_string(),
        JsonValue::Number(i64::from(usage.cache_creation_input_tokens)),
    );
    object.insert(
        "cache_read_input_tokens".to_string(),
        JsonValue::Number(i64::from(usage.cache_read_input_tokens)),
    );
    JsonValue::Object(object)
}

fn usage_from_json(value: &JsonValue) -> Result<TokenUsage, SessionError> {
    let object = value
        .as_object()
        .ok_or_else(|| SessionError::Format("usage must be an object".to_string()))?;
    Ok(TokenUsage {
        input_tokens: required_u32(object, "input_tokens")?,
        output_tokens: required_u32(object, "output_tokens")?,
        cache_creation_input_tokens: required_u32(object, "cache_creation_input_tokens")?,
        cache_read_input_tokens: required_u32(object, "cache_read_input_tokens")?,
    })
}

fn required_string(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<String, SessionError> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| SessionError::Format(format!("missing {key}")))
}

fn optional_string(object: &BTreeMap<String, JsonValue>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn optional_bool(object: &BTreeMap<String, JsonValue>, key: &str) -> bool {
    object
        .get(key)
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
}

fn optional_u32(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<u32>, SessionError> {
    object
        .get(key)
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| SessionError::Format(format!("{key} must be a number")))
                .and_then(|value| {
                    u32::try_from(value)
                        .map_err(|_| SessionError::Format(format!("{key} out of range")))
                })
        })
        .transpose()
}

fn optional_u64(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> Result<Option<u64>, SessionError> {
    object
        .get(key)
        .map(|value| required_u64_from_value(value, key))
        .transpose()
}

fn optional_string_array(object: &BTreeMap<String, JsonValue>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn optional_string_map(
    object: &BTreeMap<String, JsonValue>,
    key: &str,
) -> BTreeMap<String, String> {
    object
        .get(key)
        .and_then(JsonValue::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn insert_optional_string(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        object.insert(key.to_string(), JsonValue::String(value.to_string()));
    }
}

fn insert_optional_u32(object: &mut BTreeMap<String, JsonValue>, key: &str, value: Option<u32>) {
    if let Some(value) = value {
        object.insert(key.to_string(), JsonValue::Number(i64::from(value)));
    }
}

fn insert_optional_u64(
    object: &mut BTreeMap<String, JsonValue>,
    key: &str,
    value: Option<u64>,
) -> Result<(), SessionError> {
    if let Some(value) = value {
        object.insert(
            key.to_string(),
            JsonValue::Number(i64_from_u64(value, key)?),
        );
    }
    Ok(())
}

fn string_array_to_json(values: &[String]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .map(|value| JsonValue::String(value.clone()))
            .collect(),
    )
}

fn string_map_to_json(values: &BTreeMap<String, String>) -> JsonValue {
    JsonValue::Object(
        values
            .iter()
            .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
            .collect(),
    )
}

fn required_u32(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<u32, SessionError> {
    let value = object
        .get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| SessionError::Format(format!("missing {key}")))?;
    u32::try_from(value).map_err(|_| SessionError::Format(format!("{key} out of range")))
}

fn required_u64(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<u64, SessionError> {
    let value = object
        .get(key)
        .ok_or_else(|| SessionError::Format(format!("missing {key}")))?;
    required_u64_from_value(value, key)
}

fn required_u64_from_value(value: &JsonValue, key: &str) -> Result<u64, SessionError> {
    let value = value
        .as_i64()
        .ok_or_else(|| SessionError::Format(format!("missing {key}")))?;
    u64::try_from(value).map_err(|_| SessionError::Format(format!("{key} out of range")))
}

fn required_usize(object: &BTreeMap<String, JsonValue>, key: &str) -> Result<usize, SessionError> {
    let value = object
        .get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| SessionError::Format(format!("missing {key}")))?;
    usize::try_from(value).map_err(|_| SessionError::Format(format!("{key} out of range")))
}

fn required_usize_any(
    object: &BTreeMap<String, JsonValue>,
    keys: &[&str],
) -> Result<usize, SessionError> {
    for key in keys {
        if object.contains_key(*key) {
            return required_usize(object, key);
        }
    }
    Err(SessionError::Format(format!(
        "missing {}",
        keys.join(" or ")
    )))
}

fn i64_from_u64(value: u64, key: &str) -> Result<i64, SessionError> {
    i64::try_from(value)
        .map_err(|_| SessionError::Format(format!("{key} out of range for JSON number")))
}

fn i64_from_usize(value: usize, key: &str) -> Result<i64, SessionError> {
    i64::try_from(value)
        .map_err(|_| SessionError::Format(format!("{key} out of range for JSON number")))
}

fn workspace_root_to_string(path: &Path) -> Result<String, SessionError> {
    path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        SessionError::Format(format!(
            "workspace_root is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn current_time_millis() -> u64 {
    let wall_clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();

    let mut candidate = wall_clock;
    loop {
        let previous = LAST_TIMESTAMP_MS.load(Ordering::Relaxed);
        if candidate <= previous {
            candidate = previous.saturating_add(1);
        }
        match LAST_TIMESTAMP_MS.compare_exchange(
            previous,
            candidate,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return candidate,
            Err(actual) => candidate = actual.saturating_add(1),
        }
    }
}

fn generate_session_id() -> String {
    let millis = current_time_millis();
    let counter = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("session-{millis}-{counter}")
}

fn generate_turn_id() -> String {
    let millis = current_time_millis();
    let counter = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("turn-{millis}-{counter}")
}

fn generate_window_id() -> String {
    let millis = current_time_millis();
    let counter = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("window-{millis}-{counter}")
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = temporary_path_for(path);
    fs::write(&temp_path, contents)?;
    fs::rename(temp_path, path)?;
    Ok(())
}

fn temporary_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session");
    path.with_file_name(format!(
        "{file_name}.tmp-{}-{}",
        current_time_millis(),
        SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn rotate_session_file_if_needed(path: &Path) -> Result<(), SessionError> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < ROTATE_AFTER_BYTES {
        return Ok(());
    }
    let rotated_path = rotated_log_path(path);
    fs::rename(path, rotated_path)?;
    Ok(())
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("session");
    path.with_file_name(format!("{stem}.rot-{}.jsonl", current_time_millis()))
}

fn cleanup_rotated_logs(path: &Path) -> Result<(), SessionError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("session");
    let prefix = format!("{stem}.rot-");
    let mut rotated_paths = fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry_path| {
            entry_path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with(&prefix)
                        && Path::new(name)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                })
        })
        .collect::<Vec<_>>();

    rotated_paths.sort_by_key(|entry_path| {
        fs::metadata(entry_path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH)
    });

    let remove_count = rotated_paths.len().saturating_sub(MAX_ROTATED_FILES);
    for stale_path in rotated_paths.into_iter().take(remove_count) {
        fs::remove_file(stale_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_rotated_logs, current_time_millis, rotate_session_file_if_needed, ContentBlock,
        ConversationMessage, MessageRole, Session, SessionContextBaseline, SessionFork,
        SessionRuntimeContext, SessionTurnStatus,
    };
    use crate::json::JsonValue;
    use crate::usage::TokenUsage;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn session_timestamps_are_monotonic_under_tight_loops() {
        let first = current_time_millis();
        let second = current_time_millis();
        let third = current_time_millis();

        assert!(first < second);
        assert!(second < third);
    }

    #[test]
    fn persists_and_restores_session_jsonl() {
        let mut session = Session::new();
        session
            .push_user_text("hello")
            .expect("user message should append");
        session
            .push_message(ConversationMessage::assistant_with_usage(
                vec![
                    ContentBlock::Text {
                        text: "thinking".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-1".to_string(),
                        name: "bash".to_string(),
                        input: "echo hi".to_string(),
                    },
                ],
                Some(TokenUsage {
                    input_tokens: 10,
                    output_tokens: 4,
                    cache_creation_input_tokens: 1,
                    cache_read_input_tokens: 2,
                }),
            ))
            .expect("assistant message should append");
        session
            .push_message(ConversationMessage::tool_result(
                "tool-1", "bash", "hi", false,
            ))
            .expect("tool result should append");

        let path = temp_session_path("jsonl");
        session.save_to_path(&path).expect("session should save");
        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored, session);
        assert_eq!(restored.messages[2].role, MessageRole::Tool);
        assert_eq!(
            restored.messages[1].usage.expect("usage").total_tokens(),
            17
        );
        assert_eq!(restored.session_id, session.session_id);
    }

    #[test]
    fn loads_legacy_session_json_object() {
        let path = temp_session_path("legacy");
        let legacy = JsonValue::Object(
            [
                ("version".to_string(), JsonValue::Number(1)),
                (
                    "messages".to_string(),
                    JsonValue::Array(vec![ConversationMessage::user_text("legacy").to_json()]),
                ),
            ]
            .into_iter()
            .collect(),
        );
        fs::write(&path, legacy.render()).expect("legacy file should write");

        let restored = Session::load_from_path(&path).expect("legacy session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages.len(), 1);
        assert_eq!(
            restored.messages[0],
            ConversationMessage::user_text("legacy")
        );
        assert!(!restored.session_id.is_empty());
    }

    #[test]
    fn appends_messages_to_persisted_jsonl_session() {
        let path = temp_session_path("append");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");
        session
            .push_user_text("hi")
            .expect("user append should succeed");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "hello".to_string(),
            }]))
            .expect("assistant append should succeed");

        let restored = Session::load_from_path(&path).expect("session should replay from jsonl");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[0], ConversationMessage::user_text("hi"));
    }

    #[test]
    fn persisted_session_records_redact_plaintext_credentials() {
        let path = temp_session_path("secret-redaction");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");
        session
            .push_user_text(
                "api_key=sk-1234567890abcdef https://reader:db-secret@example.test/report",
            )
            .expect("secret-bearing message should persist in protected form");

        let persisted = fs::read_to_string(&path).expect("session file should be readable");
        assert!(!persisted.contains("sk-1234567890abcdef"));
        assert!(!persisted.contains("db-secret"));
        assert!(persisted.contains("[REDACTED"));

        let restored = Session::load_from_path(&path).expect("protected session should replay");
        fs::remove_file(&path).expect("temp file should be removable");
        let restored_text = restored.messages[0]
            .blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => None,
            })
            .collect::<String>();
        assert!(!restored_text.contains("sk-1234567890abcdef"));
        assert!(!restored_text.contains("db-secret"));
    }

    #[test]
    fn replays_latest_compaction_checkpoint_plus_suffix() {
        let path = temp_session_path("checkpoint-suffix");
        let mut original = Session::new();
        original
            .push_user_text("old user message")
            .expect("message should append");
        let baseline = vec![ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: "summary baseline".to_string(),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }];
        let mut compacted = original.clone();
        compacted.messages = baseline.clone();
        compacted.record_compaction_with_replacement("summary", 1, baseline.clone());

        let lines = [
            original.meta_record().expect("meta").render(),
            super::message_record(&ConversationMessage::user_text("old user message")).render(),
            compacted
                .compaction
                .as_ref()
                .expect("compaction")
                .to_jsonl_record()
                .expect("record")
                .render(),
            super::message_record(&ConversationMessage::user_text("new suffix")).render(),
        ];
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("session file should write");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored.messages,
            vec![
                baseline[0].clone(),
                ConversationMessage::user_text("new suffix")
            ]
        );
    }

    #[test]
    fn replay_checkpoint_dedupes_snapshot_messages() {
        let path = temp_session_path("checkpoint-snapshot");
        let mut session = Session::new();
        session.messages = vec![
            ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: "summary baseline".to_string(),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            },
            ConversationMessage::user_text("preserved tail"),
        ];
        session.record_compaction_with_replacement("summary", 4, session.messages.clone());
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages, session.messages);
    }

    #[test]
    fn persists_and_replays_rollback_events() {
        let path = temp_session_path("rollback");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");
        session.push_user_text("one").expect("append should work");
        session.push_user_text("two").expect("append should work");
        session
            .rollback_messages_to_len(1)
            .expect("rollback should append event");
        session.push_user_text("three").expect("append should work");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored.messages,
            vec![
                ConversationMessage::user_text("one"),
                ConversationMessage::user_text("three"),
            ]
        );
    }

    #[test]
    fn persists_and_replays_turn_lifecycle_events() {
        let path = temp_session_path("turn-lifecycle");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");

        let turn_id = session.begin_turn("remember my SQL").expect("turn start");
        session
            .push_user_text("remember my SQL")
            .expect("user append");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "stored".to_string(),
            }]))
            .expect("assistant append");
        session
            .complete_turn(&turn_id, SessionTurnStatus::Completed)
            .expect("turn complete");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages, session.messages);
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(restored.turns[0].turn_id, turn_id);
        assert_eq!(restored.turns[0].start_message_count, 0);
        assert_eq!(restored.turns[0].end_message_count, Some(2));
        assert_eq!(restored.turns[0].status, SessionTurnStatus::Completed);
    }

    #[test]
    fn persists_and_replays_runtime_context_records() {
        let path = temp_session_path("runtime-context");
        let mut session = Session::new().with_persistence_path(path.clone());
        session.save_to_path(&path).expect("initial save");
        let mut context = SessionRuntimeContext {
            model: Some("gpt-test".to_string()),
            provider: Some("openai".to_string()),
            scenario: Some("chat".to_string()),
            reasoning_budget: Some("deep".to_string()),
            prefer_native_web_search: true,
            tools_enabled: true,
            allowed_tools: vec!["memory_read".to_string()],
            blocked_tools: vec!["WebSearch".to_string()],
            memory_enabled: true,
            file_context_enabled: true,
            pm_search_provider_count: 2,
            system_prompt_hash: Some("abc123".to_string()),
            system_prompt_section_count: Some(7),
            ..Default::default()
        };
        context
            .metadata
            .insert("turn_mode".to_string(), "streaming".to_string());
        session
            .record_runtime_context(context.clone())
            .expect("runtime context should persist");
        let turn_id = session.begin_turn("hello").expect("turn start");
        session
            .complete_turn(&turn_id, SessionTurnStatus::Completed)
            .expect("turn complete");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        let restored_context = restored.runtime_context.as_ref().expect("runtime context");
        assert_eq!(restored_context.model, context.model);
        assert_eq!(restored_context.provider, context.provider);
        assert_eq!(restored_context.scenario, context.scenario);
        assert_eq!(restored_context.turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(
            restored.turns[0]
                .runtime_context
                .as_ref()
                .and_then(|value| value.turn_id.as_deref()),
            Some(turn_id.as_str())
        );
    }

    #[test]
    fn persists_and_replays_context_baseline_records() {
        let path = temp_session_path("context-baseline");
        let mut session = Session::new().with_persistence_path(path.clone());
        session.save_to_path(&path).expect("initial save");
        let mut baseline = SessionContextBaseline {
            label: Some("initial_runtime_context".to_string()),
            system_prompt_hash: Some("prompt-hash".to_string()),
            system_prompt_sections: vec![
                "base instructions".to_string(),
                "memory instructions".to_string(),
            ],
            system_prompt_section_hashes: vec!["h1".to_string(), "h2".to_string()],
            tool_names: vec!["memory_search".to_string(), "read_file".to_string()],
            mcp_servers: vec!["docs".to_string()],
            skills: vec!["analysis".to_string()],
            memory_enabled: true,
            file_context_enabled: true,
            search_provider_count: 2,
            ..Default::default()
        };
        baseline
            .metadata
            .insert("scenario".to_string(), "chat".to_string());
        session
            .record_context_baseline(baseline.clone())
            .expect("baseline should persist");
        let turn_id = session.begin_turn("hello").expect("turn start");
        session
            .complete_turn(&turn_id, SessionTurnStatus::Completed)
            .expect("turn complete");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        let restored_baseline = restored.context_baseline.as_ref().expect("baseline");
        assert_eq!(restored_baseline.label, baseline.label);
        assert_eq!(
            restored_baseline.system_prompt_sections,
            baseline.system_prompt_sections
        );
        assert_eq!(restored_baseline.turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(
            restored.turns[0]
                .context_baseline
                .as_ref()
                .and_then(|value| value.turn_id.as_deref()),
            Some(turn_id.as_str())
        );
    }

    #[test]
    fn rollback_replays_context_baseline_from_latest_surviving_turn() {
        let path = temp_session_path("context-baseline-rollback");
        let mut session = Session::new().with_persistence_path(path.clone());
        session.save_to_path(&path).expect("initial save");

        session.stage_context_baseline(SessionContextBaseline {
            label: Some("first".to_string()),
            system_prompt_sections: vec!["first prompt".to_string()],
            ..Default::default()
        });
        let first_turn = session.begin_turn("first").expect("first turn");
        session.push_user_text("first").expect("append first user");
        session
            .complete_turn(&first_turn, SessionTurnStatus::Completed)
            .expect("first complete");

        let second_start = session.messages.len();
        session.stage_context_baseline(SessionContextBaseline {
            label: Some("second".to_string()),
            system_prompt_sections: vec!["second prompt".to_string()],
            ..Default::default()
        });
        let second_turn = session.begin_turn("second").expect("second turn");
        session
            .push_user_text("second")
            .expect("append second user");
        assert!(session
            .rollback_latest_turn_started_at(second_start)
            .expect("rollback should succeed"));

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored
                .context_baseline
                .as_ref()
                .and_then(|baseline| baseline.label.as_deref()),
            Some("first")
        );
        assert_eq!(restored.turns.len(), 2);
        assert_eq!(restored.turns[1].turn_id, second_turn);
        assert_eq!(restored.turns[1].status, SessionTurnStatus::RolledBack);
    }

    #[test]
    fn rollback_replays_runtime_context_from_latest_surviving_turn() {
        let path = temp_session_path("runtime-context-rollback");
        let mut session = Session::new().with_persistence_path(path.clone());
        session.save_to_path(&path).expect("initial save");

        session.stage_runtime_context(SessionRuntimeContext {
            model: Some("first-model".to_string()),
            scenario: Some("chat".to_string()),
            ..Default::default()
        });
        let first_turn = session.begin_turn("first").expect("first turn");
        session.push_user_text("first").expect("append first user");
        session
            .complete_turn(&first_turn, SessionTurnStatus::Completed)
            .expect("first complete");

        let second_start = session.messages.len();
        session.stage_runtime_context(SessionRuntimeContext {
            model: Some("second-model".to_string()),
            scenario: Some("pm".to_string()),
            ..Default::default()
        });
        let second_turn = session.begin_turn("second").expect("second turn");
        session
            .push_user_text("second")
            .expect("append second user");
        assert!(session
            .rollback_latest_turn_started_at(second_start)
            .expect("rollback should succeed"));

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored
                .runtime_context
                .as_ref()
                .and_then(|context| context.model.as_deref()),
            Some("first-model")
        );
        assert_eq!(restored.turns.len(), 2);
        assert_eq!(restored.turns[1].turn_id, second_turn);
        assert_eq!(restored.turns[1].status, SessionTurnStatus::RolledBack);
    }

    #[test]
    fn compaction_checkpoint_carries_runtime_context_window_metadata() {
        let path = temp_session_path("runtime-context-compaction");
        let mut session = Session::new();
        session
            .record_runtime_context(SessionRuntimeContext {
                model: Some("gpt-test".to_string()),
                scenario: Some("pm".to_string()),
                tools_enabled: true,
                ..Default::default()
            })
            .expect("runtime context should persist in memory");
        session.messages = vec![ConversationMessage::user_text("summary baseline")];
        session.record_compaction_with_replacement("summary", 3, session.messages.clone());
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let context = restored.runtime_context.expect("runtime context");
        let compaction = restored.compaction.expect("compaction");
        assert_eq!(context.model.as_deref(), Some("gpt-test"));
        assert_eq!(context.window_number, Some(compaction.window_number));
        assert_eq!(
            context.window_id.as_deref(),
            Some(compaction.window_id.as_str())
        );
        assert_eq!(
            compaction
                .runtime_context
                .as_ref()
                .and_then(|value| value.window_id.as_deref()),
            Some(compaction.window_id.as_str())
        );
    }

    #[test]
    fn compaction_checkpoint_carries_context_baseline_window_metadata() {
        let path = temp_session_path("context-baseline-compaction");
        let mut session = Session::new();
        session
            .record_context_baseline(SessionContextBaseline {
                label: Some("initial".to_string()),
                system_prompt_hash: Some("hash".to_string()),
                system_prompt_sections: vec!["base".to_string()],
                tool_names: vec!["memory_search".to_string()],
                memory_enabled: true,
                ..Default::default()
            })
            .expect("context baseline should persist in memory");
        session.messages = vec![ConversationMessage::user_text("summary baseline")];
        session.record_compaction_with_replacement("summary", 3, session.messages.clone());
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let baseline = restored.context_baseline.expect("context baseline");
        let compaction = restored.compaction.expect("compaction");
        assert_eq!(baseline.label.as_deref(), Some("initial"));
        assert_eq!(baseline.window_number, Some(compaction.window_number));
        assert_eq!(
            baseline.window_id.as_deref(),
            Some(compaction.window_id.as_str())
        );
        assert_eq!(
            compaction
                .context_baseline
                .as_ref()
                .and_then(|value| value.window_id.as_deref()),
            Some(compaction.window_id.as_str())
        );
    }

    #[test]
    fn turn_rollback_removes_only_the_current_turn_suffix() {
        let path = temp_session_path("turn-rollback");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");

        let first_turn = session.begin_turn("first").expect("first turn");
        session.push_user_text("first").expect("append first user");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "first answer".to_string(),
            }]))
            .expect("append first assistant");
        session
            .complete_turn(&first_turn, SessionTurnStatus::Completed)
            .expect("complete first");

        let second_start = session.messages.len();
        let second_turn = session.begin_turn("second").expect("second turn");
        session
            .push_user_text("second")
            .expect("append second user");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "second answer".to_string(),
            }]))
            .expect("append second assistant");
        assert!(session
            .rollback_latest_turn_started_at(second_start)
            .expect("rollback should succeed"));

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored.messages,
            vec![
                ConversationMessage::user_text("first"),
                ConversationMessage::assistant(vec![ContentBlock::Text {
                    text: "first answer".to_string(),
                }]),
            ]
        );
        assert_eq!(restored.turns.len(), 2);
        assert_eq!(restored.turns[0].status, SessionTurnStatus::Completed);
        assert_eq!(restored.turns[1].turn_id, second_turn);
        assert_eq!(restored.turns[1].status, SessionTurnStatus::RolledBack);
        assert_eq!(restored.turns[1].end_message_count, Some(second_start));
    }

    #[test]
    fn turn_rollback_after_compaction_uses_rebased_message_counts() {
        let path = temp_session_path("turn-rollback-after-compaction");
        let mut session = Session::new().with_persistence_path(path.clone());
        session
            .save_to_path(&path)
            .expect("initial save should succeed");

        let first_turn = session.begin_turn("first").expect("first turn");
        session.push_user_text("first").expect("append first user");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "first answer".to_string(),
            }]))
            .expect("append first assistant");
        session
            .complete_turn(&first_turn, SessionTurnStatus::Completed)
            .expect("complete first");

        let second_start_before_compaction = session.messages.len();
        let second_turn = session.begin_turn("second").expect("second turn");
        session
            .push_user_text("second")
            .expect("append second user");
        session
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "second answer".to_string(),
            }]))
            .expect("append second assistant");
        let compacted_history = vec![
            ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: "summary baseline".to_string(),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            },
            ConversationMessage::user_text("second"),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "second answer".to_string(),
            }]),
        ];
        session.messages = compacted_history.clone();
        session.record_compaction_with_replacement(
            "summarized first turn",
            second_start_before_compaction,
            compacted_history,
        );
        session.save_to_path(&path).expect("compacted save");

        assert!(session
            .rollback_latest_turn_started_at(second_start_before_compaction)
            .expect("rollback should succeed"));
        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(
            restored.messages,
            vec![ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: "summary baseline".to_string(),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            }]
        );
        assert_eq!(restored.turns.len(), 2);
        assert_eq!(restored.turns[0].status, SessionTurnStatus::Completed);
        assert_eq!(restored.turns[1].turn_id, second_turn);
        assert_eq!(restored.turns[1].status, SessionTurnStatus::RolledBack);
        assert_eq!(restored.turns[1].start_message_count, 1);
        assert_eq!(restored.turns[1].end_message_count, Some(1));
    }

    #[test]
    fn persists_compaction_metadata() {
        let path = temp_session_path("compaction");
        let mut session = Session::new();
        session
            .push_user_text("before")
            .expect("message should append");
        session.record_compaction("summarized earlier work", 4);
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let compaction = restored.compaction.expect("compaction metadata");
        assert_eq!(compaction.count, 1);
        assert_eq!(compaction.window_number, 1);
        assert!(!compaction.first_window_id.is_empty());
        assert!(!compaction.window_id.is_empty());
        assert_eq!(compaction.previous_window_number, None);
        assert_eq!(compaction.previous_window_id, None);
        assert_eq!(compaction.removed_message_count, 4);
        assert!(compaction.summary.contains("summarized"));
        assert_eq!(compaction.replacement_messages, session.messages);
    }

    #[test]
    fn compaction_window_number_advances_across_checkpoints() {
        let path = temp_session_path("compaction-window");
        let mut session = Session::new();
        session.push_user_text("one").expect("append one");
        session.record_compaction("first summary", 1);
        session.push_user_text("two").expect("append two");
        session.record_compaction("second summary", 1);
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let compaction = restored.compaction.expect("compaction metadata");
        assert_eq!(compaction.count, 2);
        assert_eq!(compaction.window_number, 2);
        assert_eq!(compaction.previous_window_number, Some(1));
        assert_eq!(
            compaction.previous_window_id.as_deref(),
            session
                .compaction
                .as_ref()
                .and_then(|c| c.previous_window_id.as_deref())
        );
        assert_eq!(
            compaction.first_window_id,
            session
                .compaction
                .as_ref()
                .expect("session compaction")
                .first_window_id
        );
    }

    #[test]
    fn compaction_checkpoint_persists_exact_archived_messages() {
        let path = temp_session_path("compaction-archives");
        let archived = vec![
            ConversationMessage::user_text(
                "WITH base AS (SELECT * FROM fact)\nSELECT * FROM base;",
            ),
            ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "I read the SQL.".to_string(),
            }]),
        ];
        let mut session = Session::new();
        session.messages = vec![ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: "summary baseline".to_string(),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }];
        session.record_compaction_checkpoint(
            "summary with exact archive",
            archived.len(),
            session.messages.clone(),
            archived.clone(),
        );
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let compaction = restored.compaction.expect("compaction");
        assert_eq!(compaction.archived_messages, archived);
        assert_eq!(compaction.archive_windows.len(), 1);
        assert_eq!(compaction.archive_windows[0].archived_messages, archived);
        assert_eq!(restored.messages, session.messages);
    }

    #[test]
    fn compaction_checkpoint_preserves_archives_across_repeated_compactions() {
        let path = temp_session_path("compaction-archive-chain");
        let first_archive = vec![ConversationMessage::user_text(
            "WITH old_sql AS (SELECT 1 AS a)\nSELECT * FROM old_sql;",
        )];
        let second_archive = vec![ConversationMessage::user_text(
            "metric_a,metric_b,roi\n1,2,0.5",
        )];
        let mut session = Session::new();
        session.messages = vec![ConversationMessage::user_text("summary one")];
        session.record_compaction_checkpoint(
            "first summary",
            first_archive.len(),
            session.messages.clone(),
            first_archive.clone(),
        );
        let first_window_id = session
            .compaction
            .as_ref()
            .expect("first compaction")
            .window_id
            .clone();
        session.messages = vec![ConversationMessage::user_text("summary two")];
        session.record_compaction_checkpoint(
            "second summary",
            second_archive.len(),
            session.messages.clone(),
            second_archive.clone(),
        );
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let compaction = restored.compaction.expect("compaction");
        assert_eq!(compaction.archive_windows.len(), 2);
        assert_eq!(compaction.archive_windows[0].window_id, first_window_id);
        assert_eq!(
            compaction.archive_windows[0].archived_messages,
            first_archive
        );
        assert_eq!(
            compaction.archive_windows[1].archived_messages,
            second_archive
        );
        assert_eq!(compaction.archived_messages, second_archive);
    }

    #[test]
    fn jsonl_replay_merges_archive_windows_from_prior_compaction_records() {
        let path = temp_session_path("compaction-jsonl-archive-chain");
        let first_archive = vec![ConversationMessage::user_text(
            "CREATE TABLE first_window(id bigint);",
        )];
        let second_archive = vec![ConversationMessage::user_text(
            "CREATE TABLE second_window(id bigint);",
        )];

        let mut first = Session::new();
        first.messages = vec![ConversationMessage::user_text("summary one")];
        first.record_compaction_checkpoint(
            "first summary",
            first_archive.len(),
            first.messages.clone(),
            first_archive.clone(),
        );

        let mut second = Session::new();
        second.messages = vec![ConversationMessage::user_text("summary two")];
        second.record_compaction_checkpoint(
            "second summary",
            second_archive.len(),
            second.messages.clone(),
            second_archive.clone(),
        );

        let lines = [
            first.meta_record().expect("meta").render(),
            first
                .compaction
                .as_ref()
                .expect("first compaction")
                .to_jsonl_record()
                .expect("record")
                .render(),
            second
                .compaction
                .as_ref()
                .expect("second compaction")
                .to_jsonl_record()
                .expect("record")
                .render(),
        ];
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("session file should write");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        let compaction = restored.compaction.expect("compaction");
        assert_eq!(compaction.archive_windows.len(), 2);
        assert_eq!(
            compaction.archive_windows[0].archived_messages,
            first_archive
        );
        assert_eq!(
            compaction.archive_windows[1].archived_messages,
            second_archive
        );
    }

    #[test]
    fn compaction_checkpoint_replaces_prior_jsonl_messages() {
        let path = temp_session_path("compaction-checkpoint");
        let mut original = Session::new();
        original
            .push_user_text("old user message that should be removed")
            .expect("message should append");
        let mut compacted = original.clone();
        compacted.messages = vec![ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: "summary baseline".to_string(),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }];
        compacted.record_compaction_with_replacement(
            "summarized old turn",
            1,
            compacted.messages.clone(),
        );

        let mut lines = Vec::new();
        lines.push(original.meta_record().expect("meta").render());
        lines.extend(
            original
                .messages
                .iter()
                .map(|message| super::message_record(message).render()),
        );
        lines.push(
            compacted
                .compaction
                .as_ref()
                .expect("compaction")
                .to_jsonl_record()
                .expect("record")
                .render(),
        );
        lines.push(super::message_record(&ConversationMessage::user_text("new suffix")).render());
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("session file should write");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[0], compacted.messages[0]);
        assert_eq!(
            restored.messages[1],
            ConversationMessage::user_text("new suffix")
        );
        assert!(
            restored
                .messages
                .iter()
                .all(|message| !message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("old user message"))))
        );
    }

    #[test]
    fn saved_compaction_checkpoint_tracks_current_effective_history() {
        let path = temp_session_path("compaction-current-history");
        let mut session = Session::new();
        session
            .push_user_text("before compaction")
            .expect("message should append");
        session.record_compaction_with_replacement(
            "summary",
            1,
            vec![ConversationMessage {
                role: MessageRole::System,
                blocks: vec![ContentBlock::Text {
                    text: "summary baseline".to_string(),
                }],
                thinking: None,
                thinking_signature: None,
                usage: None,
            }],
        );
        session
            .push_user_text("after compaction")
            .expect("message should append");
        session.save_to_path(&path).expect("session should save");

        let restored = Session::load_from_path(&path).expect("session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages, session.messages);
        assert_eq!(
            restored
                .compaction
                .as_ref()
                .expect("compaction")
                .replacement_messages,
            session.messages
        );
    }

    #[test]
    fn replay_checkpoint_keeps_turn_suffix_after_compaction() {
        let path = temp_session_path("checkpoint-turn-suffix");
        let baseline = vec![ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: "summary baseline".to_string(),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }];
        let mut compacted = Session::new();
        compacted.messages = baseline.clone();
        compacted.record_compaction_with_replacement("summary", 8, baseline.clone());

        let mut suffix = compacted.clone();
        let turn_id = suffix.begin_turn("new question").expect("turn start");
        suffix.push_user_text("new question").expect("user append");
        suffix
            .push_message(ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "new answer".to_string(),
            }]))
            .expect("assistant append");
        suffix
            .complete_turn(&turn_id, SessionTurnStatus::Completed)
            .expect("turn complete");

        let lines = [
            compacted.meta_record().expect("meta").render(),
            compacted
                .compaction
                .as_ref()
                .expect("compaction")
                .to_jsonl_record()
                .expect("record")
                .render(),
            suffix.turns[0].to_jsonl_start().render(),
            super::message_record(&ConversationMessage::user_text("new question")).render(),
            super::message_record(&ConversationMessage::assistant(vec![ContentBlock::Text {
                text: "new answer".to_string(),
            }]))
            .render(),
            suffix.turns[0]
                .to_jsonl_completion()
                .expect("completion")
                .render(),
        ];
        fs::write(&path, format!("{}\n", lines.join("\n"))).expect("session file should write");

        let restored = Session::load_from_path(&path).expect("session should replay");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages.len(), 3);
        assert_eq!(restored.messages[0], baseline[0]);
        assert_eq!(
            restored.messages[1],
            ConversationMessage::user_text("new question")
        );
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(restored.turns[0].status, SessionTurnStatus::Completed);
    }

    #[test]
    fn forks_sessions_with_branch_metadata_and_persists_it() {
        let path = temp_session_path("fork");
        let mut session = Session::new();
        session
            .push_user_text("before fork")
            .expect("message should append");

        let forked = session
            .fork(Some("investigation".to_string()))
            .with_persistence_path(path.clone());
        forked
            .save_to_path(&path)
            .expect("forked session should save");

        let restored = Session::load_from_path(&path).expect("forked session should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_ne!(restored.session_id, session.session_id);
        assert_eq!(
            restored.fork,
            Some(SessionFork {
                parent_session_id: session.session_id,
                branch_name: Some("investigation".to_string()),
            })
        );
        assert_eq!(restored.messages, forked.messages);
    }

    #[test]
    fn fork_refreshes_compaction_replacement_checkpoint() {
        let path = temp_session_path("fork-checkpoint");
        let mut session = Session::new();
        session.messages = vec![ConversationMessage {
            role: MessageRole::System,
            blocks: vec![ContentBlock::Text {
                text: "summary baseline".to_string(),
            }],
            thinking: None,
            thinking_signature: None,
            usage: None,
        }];
        session.record_compaction_with_replacement("summary", 3, session.messages.clone());
        session
            .push_user_text("post-compaction branch context")
            .expect("message should append");

        let forked = session
            .fork(Some("branch".to_string()))
            .with_persistence_path(path.clone());
        forked.save_to_path(&path).expect("fork should save");
        let restored = Session::load_from_path(&path).expect("fork should load");
        fs::remove_file(&path).expect("temp file should be removable");

        assert_eq!(restored.messages, forked.messages);
        assert_eq!(
            restored
                .compaction
                .expect("compaction")
                .replacement_messages,
            forked.messages
        );
    }

    #[test]
    fn rotates_and_cleans_up_large_session_logs() {
        // given
        let path = temp_session_path("rotation");
        let oversized_length =
            usize::try_from(super::ROTATE_AFTER_BYTES + 10).expect("rotate threshold should fit");
        fs::write(&path, "x".repeat(oversized_length)).expect("oversized file should write");

        // when
        rotate_session_file_if_needed(&path).expect("rotation should succeed");

        // then
        assert!(
            !path.exists(),
            "original path should be rotated away before rewrite"
        );

        for _ in 0..5 {
            let rotated = super::rotated_log_path(&path);
            fs::write(&rotated, "old").expect("rotated file should write");
        }
        cleanup_rotated_logs(&path).expect("cleanup should succeed");

        let rotated_count = rotation_files(&path).len();
        assert!(rotated_count <= super::MAX_ROTATED_FILES);
        for rotated in rotation_files(&path) {
            fs::remove_file(rotated).expect("rotated file should be removable");
        }
    }

    #[test]
    fn rejects_jsonl_record_without_type() {
        // given
        let path = write_temp_session_file(
            "missing-type",
            r#"{"message":{"role":"user","blocks":[{"type":"text","text":"hello"}]}}"#,
        );

        // when
        let error = Session::load_from_path(&path)
            .expect_err("session should reject JSONL records without a type");

        // then
        assert!(error.to_string().contains("missing type"));
        fs::remove_file(path).expect("temp file should be removable");
    }

    #[test]
    fn rejects_jsonl_message_record_without_message_payload() {
        // given
        let path = write_temp_session_file("missing-message", r#"{"type":"message"}"#);

        // when
        let error = Session::load_from_path(&path)
            .expect_err("session should reject JSONL message records without message payload");

        // then
        assert!(error.to_string().contains("missing message"));
        fs::remove_file(path).expect("temp file should be removable");
    }

    #[test]
    fn rejects_jsonl_record_with_unknown_type() {
        // given
        let path = write_temp_session_file("unknown-type", r#"{"type":"mystery"}"#);

        // when
        let error = Session::load_from_path(&path)
            .expect_err("session should reject unknown JSONL record types");

        // then
        assert!(error.to_string().contains("unsupported JSONL record type"));
        fs::remove_file(path).expect("temp file should be removable");
    }

    #[test]
    fn rejects_legacy_session_json_without_messages() {
        // given
        let session = JsonValue::Object(
            [("version".to_string(), JsonValue::Number(1))]
                .into_iter()
                .collect(),
        );

        // when
        let error = Session::from_json(&session)
            .expect_err("legacy session objects should require messages");

        // then
        assert!(error.to_string().contains("missing messages"));
    }

    #[test]
    fn normalizes_blank_fork_branch_name_to_none() {
        // given
        let session = Session::new();

        // when
        let forked = session.fork(Some("   ".to_string()));

        // then
        assert_eq!(forked.fork.expect("fork metadata").branch_name, None);
    }

    #[test]
    fn rejects_unknown_content_block_type() {
        // given
        let block = JsonValue::Object(
            [("type".to_string(), JsonValue::String("unknown".to_string()))]
                .into_iter()
                .collect(),
        );

        // when
        let error = ContentBlock::from_json(&block)
            .expect_err("content blocks should reject unknown types");

        // then
        assert!(error.to_string().contains("unsupported block type"));
    }

    #[test]
    fn persists_workspace_root_round_trip_and_forks_inherit_it() {
        // given
        let path = temp_session_path("workspace-root");
        let workspace_root = PathBuf::from("/tmp/b4-phantom-diag");
        let mut session = Session::new().with_workspace_root(workspace_root.clone());
        session
            .push_user_text("write to the right cwd")
            .expect("user message should append");

        // when
        session
            .save_to_path(&path)
            .expect("workspace-bound session should save");
        let restored = Session::load_from_path(&path).expect("session should load");
        let forked = restored.fork(Some("phantom-diag".to_string()));
        fs::remove_file(&path).expect("temp file should be removable");

        // then
        assert_eq!(restored.workspace_root(), Some(workspace_root.as_path()));
        assert_eq!(forked.workspace_root(), Some(workspace_root.as_path()));
    }

    fn temp_session_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("runtime-session-{label}-{nanos}.json"))
    }

    fn write_temp_session_file(label: &str, contents: &str) -> PathBuf {
        let path = temp_session_path(label);
        fs::write(&path, format!("{contents}\n")).expect("temp session file should write");
        path
    }

    fn rotation_files(path: &Path) -> Vec<PathBuf> {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("temp path should have file stem")
            .to_string();
        fs::read_dir(path.parent().expect("temp path should have parent"))
            .expect("temp dir should read")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry_path| {
                entry_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!("{stem}.rot-"))
                            && Path::new(name)
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                    })
            })
            .collect()
    }
}

/// Per-worktree session isolation: returns a session directory namespaced
/// by the workspace fingerprint of the given working directory.
/// This prevents parallel `opencode serve` instances from colliding.
/// Called by external consumers (e.g. clawhip) to enumerate sessions for a CWD.
#[allow(dead_code)]
pub fn workspace_sessions_dir(cwd: &std::path::Path) -> Result<std::path::PathBuf, SessionError> {
    let store = crate::session_control::SessionStore::from_cwd(cwd)
        .map_err(|e| SessionError::Io(std::io::Error::other(e.to_string())))?;
    Ok(store.sessions_dir().to_path_buf())
}

#[cfg(test)]
mod workspace_sessions_dir_tests {
    use super::*;
    use std::fs;

    #[test]
    fn workspace_sessions_dir_returns_fingerprinted_path_for_valid_cwd() {
        let tmp = std::env::temp_dir().join("aos-session-dir-test");
        fs::create_dir_all(&tmp).expect("create temp dir");

        let result = workspace_sessions_dir(&tmp);
        assert!(
            result.is_ok(),
            "workspace_sessions_dir should succeed for a valid CWD, got: {result:?}"
        );
        let dir = result.unwrap();
        // The returned path should be non-empty and end with a hash component
        assert!(!dir.as_os_str().is_empty());
        // Two calls with the same CWD should produce identical paths (deterministic)
        let result2 = workspace_sessions_dir(&tmp).unwrap();
        assert_eq!(dir, result2, "workspace_sessions_dir must be deterministic");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn workspace_sessions_dir_differs_for_different_cwds() {
        let tmp_a = std::env::temp_dir().join("aos-session-dir-a");
        let tmp_b = std::env::temp_dir().join("aos-session-dir-b");
        fs::create_dir_all(&tmp_a).expect("create dir a");
        fs::create_dir_all(&tmp_b).expect("create dir b");

        let dir_a = workspace_sessions_dir(&tmp_a).expect("dir a");
        let dir_b = workspace_sessions_dir(&tmp_b).expect("dir b");
        assert_ne!(
            dir_a, dir_b,
            "different CWDs must produce different session dirs"
        );

        fs::remove_dir_all(&tmp_a).ok();
        fs::remove_dir_all(&tmp_b).ok();
    }
}
