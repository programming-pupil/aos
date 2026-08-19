//! Agent Gateway — HTTP/WebSocket interface layer that bridges web-ui sessions
//! to the full agent runtime (`ConversationRuntime`).
//!
//! ## Design Principles
//!
//! 1. **Zero core changes** — we only call public interfaces from `runtime`, `tools`, `api`.
//! 2. **User isolation by construction** — every path reference is validated against
//!    the authenticated user's workspace root before use. There is no way for one
//!    user's request to address another user's files.
//! 3. **Per-user workspaces** — each user has an isolated workspace directory
//!    `$data_dir/{tenant_id}/{user_id}/workspace` where the agent operates.
//! 4. **Concurrency control** — each user may have at most N active sessions
//!    (default 3) enforced by a semaphore.
//!
//! ## Architecture
//!
//! ```text
//! web-server routes
//!     └─► AgentSessionManager
//!              ├─► TenantConfigRegistry  (loads per-user settings from DB)
//!              ├─► RuntimeBuilder         (wires up ConversationRuntime)
//!              ├─► GitlabProjectManager   (git clone / sync)
//!              └─► EventBridge           (runtime events → SSE → browser)
//! ```
//!
//! ## Path Safety Invariant
//!
//! EVERY path returned to callers or passed to tool executors MUST satisfy:
//!
//! ```text
//! canonicalize(path).starts_with(canonicalize(user_workspace_root))
//! ```
//!
//! This invariant is enforced by the [`PathValidator`] helper and is tested
//! in the `path_safety` module.

mod config_registry;
pub mod crypto;
mod error;
mod events;
mod gitlab;
mod path_safety;
pub mod runtime_builder;
mod session_manager;
pub mod skill_tools;
mod workspace;
mod workspace_sandbox;

pub use workspace::cancel_active_workspace_executions;

pub use config_registry::{
    ApiKeyEntry, ContextArchiveParams, SessionCompactionParams, TenantConfigRegistry,
    TokenUsageParams, UserQuota, UserRuntimeConfig,
};
pub use crypto::{decrypt, encrypt};
pub use error::{GatewayError, Result};
pub use events::{
    AgentEvent, CompactionRecord, SessionMetadata, StreamingTurnResult, TokenUsageRecord,
    ToolCallRecord, TurnResult,
};
pub use gitlab::{
    decrypt_repository_token, AddProjectRequest, GitlabProject, GitlabProjectManager,
    UpdateProjectRequest,
};
pub use path_safety::PathValidator;
pub use runtime_builder::{CompactionHookContext, CompactionHookFactory, RuntimeBuilder};
pub use session_manager::{
    memory_app_for_session_source, AgentSession, AgentSessionManager, AgentSuspendedTurn,
    AgentTurnOptions, AgentTurnRunOutcome, InternalReasoningBudget, ManualCompactionResult,
    McpSearchToolCandidate, McpSearchToolExecution, SessionContextStatus, SessionHandle,
    SessionInfo, SessionState,
};

/// Character overlap used by the shared uploaded-file chunker and exact
/// workspace reconstruction. Keeping one constant prevents parser/index
/// changes from silently corrupting reconstructed source text.
pub const UNIFIED_WORKSPACE_UPLOAD_OVERLAP_CHARS: usize = 180;

use std::path::PathBuf;
use std::sync::Arc;

/// Build a fully configured `AgentSessionManager` wired to the shared database pool.
///
/// `compaction_hook_factory` injects the per-session "extract → persist →
/// compact" hook applied to every runtime the manager builds, connecting the
/// higher-layer orchestration to the runtime's real auto-compaction trigger
/// (Req 4.1 / 4.3 / 4.9). Pass `None` to keep the default heuristic compaction.
pub fn build_session_manager(
    db: &sqlx::SqlitePool,
    data_dir: PathBuf,
    config_home: PathBuf,
    compaction_hook_factory: Option<CompactionHookFactory>,
) -> Result<Arc<AgentSessionManager>> {
    let config_registry = Arc::new(TenantConfigRegistry::new(db.clone()));
    build_session_manager_with_registry(
        db,
        data_dir,
        config_home,
        config_registry,
        compaction_hook_factory,
    )
}

/// Build a fully configured session manager using the caller's config registry.
///
/// Web hosts should use this variant so configuration mutations and agent
/// sessions share one cache and observe API key, MCP, and Skill updates
/// immediately.
pub fn build_session_manager_with_registry(
    db: &sqlx::SqlitePool,
    data_dir: PathBuf,
    config_home: PathBuf,
    config_registry: Arc<TenantConfigRegistry>,
    compaction_hook_factory: Option<CompactionHookFactory>,
) -> Result<Arc<AgentSessionManager>> {
    let gitlab_manager = Arc::new(GitlabProjectManager::new(db.clone(), data_dir.clone()));

    let mut manager =
        AgentSessionManager::new(data_dir, config_registry, gitlab_manager, config_home);
    if let Some(factory) = compaction_hook_factory {
        manager = manager.with_compaction_hook_factory(factory);
    }

    Ok(Arc::new(manager))
}
