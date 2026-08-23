//! Core runtime primitives for the `claw` CLI and supporting crates.
//!
//! This crate owns session persistence, permission evaluation, prompt assembly,
//! MCP plumbing, tool-facing file operations, and the core conversation loop
//! that drives interactive and one-shot turns.

mod approval_tokens;
mod bash;
pub mod bash_validation;
mod bootstrap;
pub mod branch_lock;
mod compact;
mod config;
pub mod config_validate;
mod conversation;
pub mod data_protection;
mod file_ops;
pub mod g004_conformance;
mod git_context;
pub mod green_contract;
mod hooks;
mod json;
mod lane_events;
pub mod lsp_client;
pub mod mcp;
mod mcp_client;
mod mcp_config_watcher;
pub mod mcp_lifecycle_hardened;
pub mod mcp_server;
mod mcp_stdio;
pub mod mcp_tool_bridge;
mod oauth;
pub mod permission_enforcer;
mod permissions;
pub mod plugin_lifecycle;
mod policy_engine;
mod prompt;
pub mod recovery_recipes;
mod remote;
mod report_schema;
pub mod sandbox;
mod sandbox_backend;
mod session;
pub mod session_control;
pub use session_control::SessionStore;
pub mod egress;
pub mod execution_kernel;
pub mod isolation;
#[cfg(feature = "local-embedding")]
pub mod local_embedding;
pub mod semantic_kernel;
mod sse;
pub mod stale_base;
pub mod stale_branch;
pub mod summary_compression;
pub mod task_packet;
pub mod task_registry;
pub mod team_cron_registry;
pub mod tenant_executor;
pub mod tenant_sandbox;
mod token_estimator;
pub mod trident;
#[cfg(test)]
mod trust_resolver;
mod usage;
pub mod worker_boot;

pub use approval_tokens::{
    ApprovalDelegationHop, ApprovalScope, ApprovalTokenAudit, ApprovalTokenError,
    ApprovalTokenGrant, ApprovalTokenLedger, ApprovalTokenStatus,
};
pub use bash::{execute_bash, execute_bash_with_cancellation, BashCommandInput, BashCommandOutput};

pub(crate) fn behavior_trace(case_id: &str) {
    if std::env::var("AOS_BEHAVIOR_TRACE_CASE").as_deref() == Ok(case_id) {
        static EMITTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if EMITTED.set(()).is_ok() {
            eprintln!("AOS_PRODUCTION_TRACE\t{case_id}");
        }
    }
}
pub use bootstrap::{BootstrapPhase, BootstrapPlan};
pub use branch_lock::{detect_branch_lock_collisions, BranchLockCollision, BranchLockIntent};
pub use compact::{
    compact_session, compact_session_with_summary, format_compact_summary,
    get_compact_continuation_message, replace_compaction_summary, should_compact, CompactionConfig,
    CompactionResult,
};
pub use config::{
    ConfigEntry, ConfigError, ConfigLoader, ConfigSource, McpConfigCollection,
    McpManagedProxyServerConfig, McpOAuthConfig, McpRemoteServerConfig, McpSdkServerConfig,
    McpServerConfig, McpStdioServerConfig, McpTransport, McpWebSocketServerConfig, OAuthConfig,
    ProviderFallbackConfig, ResolvedPermissionMode, RulesImportConfig, RuntimeConfig,
    RuntimeContextManagementConfig, RuntimeFeatureConfig, RuntimeHookConfig, RuntimeHookEntry,
    RuntimePermissionRuleConfig, RuntimePluginConfig, ScopedMcpServerConfig,
    AOS_SETTINGS_SCHEMA_NAME,
};
pub use config_validate::{
    check_unsupported_format, format_diagnostics, validate_config_file, ConfigDiagnostic,
    DiagnosticKind, ValidationResult,
};
pub use conversation::{
    auto_compaction_threshold_from_env, trident_compaction_enabled_from_env, ApiClient, ApiRequest,
    AssistantEvent, AutoCompactionEvent, CompactionHook, ContextManagementReport,
    ConversationRuntime, DeferredApprovalDecision, DeferredToolResult, DeferredToolUse,
    PreparedCompaction, PromptCacheEvent, ProviderRequestTrace, ResumableTurnOutcome,
    RuntimeCancellationToken, RuntimeError, RuntimeEventReporter, StaticToolExecutor,
    SuspendedTurn, ToolError, ToolExecutionOutcome, ToolExecutionRequest, ToolExecutor,
    ToolInvocationContext, TurnSummary,
};
pub use data_protection::{
    configured_data_protection_mode, explicit_env_opt_in_enabled, explicit_opt_in_value,
    inspect_sensitive_text, protect_sensitive_json, protect_sensitive_text, DataProtectionMode,
    DataProtectionReport, ProtectedText, SensitiveDataCategory,
};
pub use egress::{
    evaluate_egress, EgressDecision, EgressPolicy, EgressRule, HostMatcher, NetTarget,
};
pub use execution_kernel::{
    reduce_runtime_artifact, AgentExecutionKernel, RuntimeApprovalDecision, RuntimeApprovalRequest,
    RuntimeApprovalResolution, RuntimeArtifactKind, RuntimeArtifactPreview,
    RuntimeContextManifestInput, RuntimeContextSupplement, RuntimeContextSupplementRequest,
    RuntimeInteractionRequest, RuntimeInteractionResolution, RuntimeManifestLineage,
    RuntimeModelBudgetStage, RuntimeToolCancellationContract, RuntimeToolContract,
    RuntimeToolIntent, RuntimeToolOutcome, RuntimeToolOutcomeKind, RuntimeToolProjection,
    RuntimeToolRetryPolicy, RuntimeToolRiskLevel, RuntimeToolSideEffectClass, RuntimeTurnStart,
    RuntimeTurnTerminalStatus,
};
pub use file_ops::{
    edit_file, edit_file_in_workspace, edit_file_with_cancellation, glob_search,
    glob_search_in_workspace, grep_search, grep_search_in_workspace, read_file,
    read_file_in_workspace, write_file, write_file_in_workspace, write_file_with_cancellation,
    EditFileOutput, GlobSearchOutput, GrepSearchInput, GrepSearchOutput, ReadFileOutput,
    StructuredPatchHunk, TextFilePayload, WriteFileOutput,
};
pub use git_context::{GitCommitEntry, GitContext};
pub use hooks::{
    HookAbortSignal, HookEvent, HookProgressEvent, HookProgressReporter, HookRunResult, HookRunner,
};
pub use isolation::{
    evaluate_path_access, normalize_lexical, tenant_roots_disjoint, IsolationDecision, PathAccess,
};
pub use lane_events::{
    dedupe_superseded_commit_events, LaneCommitProvenance, LaneEvent, LaneEventBlocker,
    LaneEventName, LaneEventStatus, LaneFailureClass,
};
pub use mcp::{
    mcp_server_signature, mcp_tool_name, mcp_tool_prefix, normalize_name_for_mcp,
    scoped_mcp_config_hash, unwrap_ccr_proxy_url, McpServerSessionManager,
};
pub use mcp_client::{
    McpClientAuth, McpClientBootstrap, McpClientTransport, McpManagedProxyTransport,
    McpRemoteTransport, McpSdkTransport, McpStdioTransport,
};
pub use mcp_config_watcher::{
    diff_mcp_config, ConfigWatcher, McpConfigDiff, McpReloadableManager,
    ReloadableMcpServerManager, ReloadableMcpServerSessionManager,
};
pub use mcp_lifecycle_hardened::{
    McpDegradedReport, McpErrorSurface, McpFailedServer, McpLifecyclePhase, McpLifecycleState,
    McpLifecycleValidator, McpPhaseResult,
};
pub use mcp_server::{McpServer, McpServerSpec, ToolCallHandler, MCP_SERVER_PROTOCOL_VERSION};
pub use mcp_stdio::{
    spawn_mcp_stdio_process, JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    ManagedMcpTool, McpDiscoveryFailure, McpInitializeClientInfo, McpInitializeParams,
    McpInitializeResult, McpInitializeServerInfo, McpListResourcesParams, McpListResourcesResult,
    McpListToolsParams, McpListToolsResult, McpReadResourceParams, McpReadResourceResult,
    McpResource, McpResourceContents, McpServerManager, McpServerManagerError, McpStdioProcess,
    McpTool, McpToolCallContent, McpToolCallParams, McpToolCallResult, McpToolDiscoveryReport,
    UnsupportedMcpServer,
};
pub use oauth::{
    clear_oauth_credentials, code_challenge_s256, credentials_path, generate_pkce_pair,
    generate_state, load_oauth_credentials, loopback_redirect_uri, parse_oauth_callback_query,
    parse_oauth_callback_request_target, save_oauth_credentials, OAuthAuthorizationRequest,
    OAuthCallbackParams, OAuthRefreshRequest, OAuthTokenExchangeRequest, OAuthTokenSet,
    PkceChallengeMethod, PkceCodePair,
};
pub use permissions::{
    PermissionContext, PermissionMode, PermissionOutcome, PermissionOverride, PermissionPolicy,
    PermissionPromptDecision, PermissionPrompter, PermissionRequest,
};
pub use plugin_lifecycle::{
    DegradedMode, DiscoveryResult, PluginHealthcheck, PluginLifecycle, PluginLifecycleEvent,
    PluginState, ResourceInfo, ServerHealth, ServerStatus, ToolInfo,
};
pub use policy_engine::{
    evaluate, DiffScope, GreenLevel, LaneBlocker, LaneContext, PolicyAction, PolicyCondition,
    PolicyEngine, PolicyRule, ReconcileReason, ReviewStatus,
};
pub use prompt::{
    load_system_prompt, load_system_prompt_with_context,
    load_system_prompt_with_context_and_profile, load_system_prompt_with_profile, prepend_bullets,
    ContextFile, ModelFamilyIdentity, ProjectContext, PromptBuildError, SystemPromptBuilder,
    SystemPromptProfile, FRONTIER_MODEL_NAME, SYSTEM_PROMPT_DYNAMIC_BOUNDARY,
};
pub use recovery_recipes::{
    attempt_recovery, recipe_for, EscalationPolicy, FailureScenario, RecoveryContext,
    RecoveryEvent, RecoveryRecipe, RecoveryResult, RecoveryStep,
};
pub use remote::{
    inherited_upstream_proxy_env, no_proxy_list, read_token, upstream_proxy_ws_url,
    RemoteSessionContext, UpstreamProxyBootstrap, UpstreamProxyState, DEFAULT_REMOTE_BASE_URL,
    DEFAULT_SESSION_TOKEN_PATH, DEFAULT_SYSTEM_CA_BUNDLE, NO_PROXY_HOSTS, UPSTREAM_PROXY_ENV_KEYS,
};
pub use report_schema::{
    canonicalize_report, project_report, report_content_hash, report_schema_v1_registry,
    CanonicalReportV1, ClaimKind, ConsumerCapabilities, FieldDelta, FieldDeltaState,
    NegativeEvidence, NegativeFindingStatus, ProjectionProvenance, RedactionProvenance,
    ReportClaim, ReportConfidence, ReportIdentity, ReportProjectionV1, ReportSchemaField,
    ReportSchemaRegistry, SensitivityClass, DEFAULT_PROJECTION_POLICY_V1, REPORT_SCHEMA_V1,
};
pub use sandbox::{
    build_linux_sandbox_command, detect_container_environment, detect_container_environment_from,
    execute_confined_command, resolve_sandbox_status, resolve_sandbox_status_for_request,
    sandbox_backend_capability, ConfinedOutput, ContainerEnvironment, EnforcementCapability,
    FilesystemIsolationMode, LinuxSandboxCommand, SandboxConfig, SandboxDetectionInputs,
    SandboxRequest, SandboxStatus,
};
pub use session::{
    ContentBlock, ConversationMessage, MessageRole, Session, SessionCompaction,
    SessionContextBaseline, SessionError, SessionFork, SessionHeartbeat, SessionLiveness,
    SessionPromptEntry, SessionRuntimeContext, SessionTurnStatus,
};
pub use sse::{IncrementalSseParser, SseEvent};
pub use stale_base::{
    check_base_commit, format_stale_base_warning, read_aos_base_file, resolve_expected_base,
    BaseCommitSource, BaseCommitState,
};
pub use stale_branch::{
    apply_policy, check_freshness, BranchFreshness, StaleBranchAction, StaleBranchEvent,
    StaleBranchPolicy,
};
pub use task_packet::{validate_packet, TaskPacket, TaskPacketValidationError, ValidatedPacket};
pub use telemetry::{
    AnalyticsEvent, JsonlTelemetrySink, MemoryTelemetrySink, SessionTraceRecord, SessionTracer,
    TelemetryEvent, TelemetrySink,
};
pub use tenant_executor::{evaluate_tenant_command, execute_in_tenant_sandbox, TenantCommand};
pub use tenant_sandbox::{
    stable_tenant_dir_name, tenant_root_from_base, ExecOutcome, ExecutionAudit, ResourceQuota,
    ResourceUsage, TenantSandbox,
};
pub use token_estimator::{
    estimate_message_tokens, estimate_message_tokens_with_options, estimate_session_tokens,
    estimate_session_tokens_with_options, estimate_text_tokens, estimate_text_tokens_with_options,
    TokenEstimateOptions, TokenizerKind,
};
pub use trident::{trident_compact_session, TridentConfig, TridentStats};
#[cfg(test)]
pub use trust_resolver::{TrustConfig, TrustDecision, TrustEvent, TrustPolicy, TrustResolver};
pub use usage::{
    format_usd, pricing_for_model, pricing_with_custom, pricing_with_provenance, ModelPricing,
    PricingSource, TokenUsage, UsageCostEstimate, UsageTracker,
};
pub use worker_boot::{
    Worker, WorkerEvent, WorkerEventKind, WorkerEventPayload, WorkerFailure, WorkerFailureKind,
    WorkerPromptTarget, WorkerReadySnapshot, WorkerRegistry, WorkerStatus, WorkerTrustResolution,
};

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
