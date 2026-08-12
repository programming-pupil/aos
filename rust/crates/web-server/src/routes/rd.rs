//! AOS Code Studio routes — repository-aware engineering agent workspace.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, UNIX_EPOCH};

use axum::{
    body::Body,
    extract::{Extension, Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use tokio::{
    sync::mpsc,
    time::{timeout, Duration},
};
use walkdir::WalkDir;

use chrono::Utc;
use parking_lot::Mutex as ParkingMutex;

use crate::auth::Claims;
use crate::error::AppError;
use crate::routes::hooks::HookEventType;
use crate::state::AppState;
use rd_core::{
    collect_flat_tree, collect_repository_imports, collect_repository_symbols, contains_any,
    count_repository_files, detect_repository_profile, is_plausible_import_path, language_for_path,
    should_skip_path, LanguageStat as RdLanguageStatDto, RdContextBudget, RdContextProfile,
    RepositoryDetection as RdRepositoryDetection, MAX_FILE_BYTES,
};

mod agent_profiles;
mod auto_repair;
mod code_intel;
mod command_safety;
mod commands;
mod completion;
mod context;
mod diff_filters;
mod diff_validation;
pub(crate) mod embedding;
mod indexing;
mod integrations;
mod lifecycle_hooks;
mod market;
mod metrics;
mod patch;
mod patch_ownership;
mod preview_sessions;
mod prompts;
mod quality;
mod repositories;
mod review;
mod review_passes;
mod runtime_config;
mod runtime_events;
mod runtime_execution;
mod runtime_governance;
mod runtime_session;
mod runtime_tools;
mod specs;
mod stale_tasks;
mod steering;
mod task_actions;
mod task_events;
mod task_executor;
mod task_lifecycle;
mod task_store;
mod tasks;
mod text;
mod types;
mod utils;
mod workbench;
mod workflow;
mod worktree;

use self::agent_profiles::{
    create_agent_profile, delete_agent_profile, get_agent_profile_row, list_agent_profiles,
    load_enabled_agent_profile, update_agent_profile, RdAgentProfileDto,
};
use self::auto_repair::attempt_rd_auto_repair;
use self::code_intel::{code_intel_query, code_intel_restart, code_intel_status};
use self::command_safety::reject_dangerous_command;
use self::commands::{
    build_rd_candidate_fix_prompt, rd_test_command_timeout_secs, resolve_rd_test_command,
    run_command_in_dir, run_command_in_dir_with_agent_runtime, summarize_rd_test_output_for_prompt,
};
use self::completion::{
    extract_rd_allowed_tools, run_rd_completion, run_rd_completion_with_options, RdCompletionResult,
};
use self::context::{
    build_rd_context_plan_section, build_rd_context_policy_section,
    build_rd_llm_context_plan_section, build_rd_system_prompt, build_repository_context_for_prompt,
    build_repository_exact_evidence_context, build_repository_prescan_context,
    build_repository_runtime_context_hint, default_rd_context_depth,
    load_prompt_file_context_for_task, load_repository_instructions_for_task,
    maybe_run_rd_llm_context_planner, normalize_rd_context_depth, normalize_rd_profile_for_mode,
    rd_context_budget_json, rd_embed_texts_with_candidate_background,
    rd_normalize_repo_relative_path, record_rd_embedding_usage, resolve_rd_embedding_candidates,
    resolve_rd_task_context_strategy, route_rd_task_intent, should_run_rd_repository_prescan,
};
use self::diff_filters::{
    filter_rd_unified_diff_excluded_paths, infer_files_from_unified_diff,
    rd_file_change_is_applyable, sanitize_rd_parsed_diff_output,
};
use self::diff_validation::{maybe_repair_invalid_generated_diff, validate_generated_diff};
use self::embedding::{
    hash_text as rd_embedding_hash_text, repository_chunk_id, task_chunk_id, RdEmbeddingChunkType,
    RdEmbeddingChunkUpsert, RdEmbeddingSearchHit,
};
use self::indexing::{
    rebuild_repository_context_summary_index, rebuild_repository_file_summary_index,
    rebuild_repository_import_index, rebuild_repository_symbol_index, safe_join,
    safe_join_allow_missing, schedule_rd_repository_embedding_index,
    schedule_rd_repository_llm_summary_refinement, schedule_rd_task_embedding_index,
    stable_hash_hex,
};
use self::lifecycle_hooks::run_rd_hook;
use self::market::{install_agent_market_item, search_agent_market};
use self::metrics::record_quality_metric;
use self::patch::{
    build_patch_from_hunks, git_apply, git_apply_reverse, git_dirty_paths, split_unified_diff_hunks,
};
use self::patch_ownership::{
    enforce_rd_diff_output_policy, mark_rd_patch_ownership_applied, record_rd_patch_ownerships,
};
use self::preview_sessions::{
    authorize_preview_session, create_preview_session, get_preview_session, preview_proxy,
    preview_proxy_root, preview_screenshot, preview_session_logs, record_preview_console_event,
    stop_preview_session,
};
use self::quality::{
    accumulate_rd_quality_source_hit, get_quality_summary, load_rd_quality_embedding_model,
    load_rd_quality_index_cache_metrics, RdQualityIndexCacheMetrics, RdQualityObservabilityMetrics,
};
use self::repositories::{
    create_repository, delete_repository, ensure_repository_exists, list_repositories,
    load_repo_setting, repository_branches, repository_file, repository_file_suggestions,
    repository_imports, repository_root, repository_search, repository_symbols, repository_tree,
    repository_worktree_status, run_exact_repository_search, run_rg_repository_search,
    sync_repository, update_repository, RdRepositoryWorktreeStatusDto,
};
use self::review::{
    analyze_review_quality, record_rd_task_risk_map, record_review_quality_metrics,
};
pub fn start_periodic_repository_sync(
    state: AppState,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    repositories::start_periodic_repository_sync(state)
}

pub(crate) async fn recover_interrupted_plan_generations(db: &SqlitePool) -> Result<(), AppError> {
    let recovery_message =
        "AOS restarted while this plan stage was running. Retry the current stage to continue.";
    sqlx::query(
        "UPDATE rd_specs SET status = 'failed', last_error = ?, \
         stage_status_json = json_set(COALESCE(stage_status_json, JSON_OBJECT()), \
             '$.' || current_stage, 'failed'), updated_at = CURRENT_TIMESTAMP \
         WHERE status IN ('queued', 'running')",
    )
    .bind(recovery_message)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod interrupted_plan_recovery_tests {
    use super::recover_interrupted_plan_generations;

    #[tokio::test]
    async fn interrupted_plan_generation_becomes_retryable_after_restart() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite");
        sqlx::query(
            "CREATE TABLE rd_specs (id TEXT PRIMARY KEY, status TEXT, current_stage TEXT, \
             last_error TEXT, stage_status_json TEXT, updated_at TEXT)",
        )
        .execute(&db)
        .await
        .expect("plan schema");
        sqlx::query(
            "INSERT INTO rd_specs VALUES \
             ('plan-1', 'running', 'design', NULL, '{\"design\":\"running\"}', CURRENT_TIMESTAMP)",
        )
        .execute(&db)
        .await
        .expect("running plan fixture");

        recover_interrupted_plan_generations(&db)
            .await
            .expect("recover interrupted plan");
        let (status, error, stage_status): (String, String, String) = sqlx::query_as(
            "SELECT status, last_error, CAST(stage_status_json AS TEXT) FROM rd_specs WHERE id = 'plan-1'",
        )
        .fetch_one(&db)
        .await
        .expect("recovered plan");
        assert_eq!(status, "failed");
        assert!(error.contains("Retry the current stage"));
        assert!(stage_status.contains("\"design\":\"failed\""));
    }
}
use self::review_passes::{maybe_run_rd_architecture_pass, maybe_run_rd_reviewer_pass};
#[cfg(test)]
use self::runtime_config::{
    parse_rd_runtime_direct_fallback_enabled, parse_rd_runtime_timeout_secs,
};
use self::runtime_config::{
    rd_architecture_pass_enabled, rd_auto_repair_max_attempts, rd_candidate_max_fix_attempts,
    rd_candidate_runtime_bash_enabled, rd_candidate_verify_enabled, rd_candidate_worktree_enabled,
    rd_llm_context_planner_enabled, rd_llm_context_summary_enabled, rd_reviewer_pass_enabled,
    rd_runtime_bash_enabled, rd_runtime_direct_fallback_enabled, rd_runtime_executor_enabled,
    rd_runtime_timeout_secs, rd_runtime_write_tools_enabled,
};
#[cfg(test)]
use self::runtime_events::rd_runtime_event_to_task_event;
use self::runtime_events::{
    persist_rd_runtime_events, rd_runtime_soft_feedback_json, rd_runtime_tool_output_signal,
    summarize_rd_tool_text,
};
#[cfg(test)]
use self::runtime_execution::build_rd_candidate_context_message;
use self::runtime_execution::{run_rd_candidate_worktree_completion, run_rd_runtime_completion};
use self::runtime_governance::{
    build_rd_runtime_tool_governance, build_rd_runtime_tool_governance_plan,
    rd_runtime_tool_reason, rd_runtime_tool_target, summarize_rd_tool_calls,
};
#[cfg(test)]
use self::runtime_session::rd_runtime_error_is_auth_failure;
use self::runtime_session::{
    run_rd_runtime_turn_with_timeout_cleanup, start_rd_runtime_session, RdRuntimeSessionPolicy,
};
use self::runtime_tools::{ensure_rd_candidate_runtime_tools, resolve_rd_runtime_tool_policy};
#[cfg(test)]
use self::runtime_tools::{
    ensure_rd_candidate_runtime_tools_with_bash, rd_tool_is_bash,
    resolve_rd_runtime_tool_policy_with_switches,
};
use self::specs::{
    approve_design, approve_spec, approve_tasks, create_spec, create_task_from_spec, delete_spec,
    final_report_spec, generate_design, generate_spec, generate_tasks, get_spec, implement_all,
    implement_task, list_spec_events, list_specs, revise_spec_stage, update_spec,
};
use self::steering::{
    build_steering_context, create_steering_rule, delete_steering_rule, list_steering_rules,
    update_steering_rule,
};
use self::task_actions::{
    apply_task_changes, apply_task_hunks, rollback_task_changes, run_task_test,
};
use self::task_executor::execute_rd_task;
pub(crate) use self::task_lifecycle::retry_task_from_agent_ops;
use self::task_lifecycle::{cancel_task, create_task, retry_task, route_task_intent};
#[cfg(feature = "bot-agents")]
pub(crate) use self::task_lifecycle::{create_task_from_bot, RdBotTaskCreateInput};
use self::task_store::{
    complete_rd_task_if_no_pending_applyable_changes, ensure_task_access, get_agent_workflow_row,
    get_task_row, get_test_run, load_enabled_agent_workflow, record_event,
    reopen_rd_task_if_pending_applyable_changes, row_to_agent_workflow, row_to_change, row_to_task,
    row_to_test_run, update_rd_task_context_strategy,
};
use self::tasks::{get_task, list_tasks, task_changes, task_tests};
use self::text::{
    build_rd_runtime_user_prompt, derive_title, infer_first_file_from_diff, normalize_mode,
    parse_json_from_model_output, parse_rd_output, rd_system_prompt, truncate_text, ParsedRdOutput,
};
use self::types::{
    RdAgentMarketInstallRequest, RdAgentMarketInstallResponse, RdAgentMarketItemDto,
    RdAgentMarketQuery, RdAgentMarketSearchResponse, RdAgentWorkflowDto, RdAgentWorkflowRequest,
    RdFileChangeDto, RdIntentRouteDecision, RdIntentRouteRequest, RdIntentRouteResponse,
    RdLlmContextPlan, RdRepositoryInstructionContext, RdRuntimeToolGovernancePlan,
    RdTaskApplyHunksRequest, RdTaskApplyHunksResponse, RdTaskApplyRequest, RdTaskApplyResponse,
    RdTaskContextStrategy, RdTaskCreateRequest, RdTaskDto, RdTaskListQuery, RdTaskListResponse,
    RdTaskRollbackResponse, RdTaskTestRequest, RdTestRunDto, RdTokenUsageSnapshot,
    RdWorkflowStageKind, RdWorkflowStageSpec,
};

pub(crate) async fn approve_task_from_agent_ops(
    state: AppState,
    claims: Claims,
    task_id: &str,
) -> Result<Value, AppError> {
    let Json(result) = apply_task_changes(
        State(state),
        Extension(claims),
        AxumPath(task_id.to_string()),
        Json(RdTaskApplyRequest { change_ids: None }),
    )
    .await?;
    Ok(json!({
        "status": "approved",
        "applied": result.applied,
        "skipped": result.skipped,
    }))
}
use self::utils::{
    metric_count, nonnegative_i64_to_u64, normalize_optional, ratio, rd_error_is_cancelled,
    rd_task_cancelled_error, rd_task_error_detail_json, require_non_empty,
};
use self::workflow::{
    create_agent_workflow, delete_agent_workflow, list_agent_workflows,
    maybe_run_rd_workflow_postflight_stages, maybe_run_rd_workflow_preflight_stages,
    rd_workflow_stages, update_agent_workflow, workflow_definition_section,
};
#[cfg(test)]
use self::workflow::{workflow_stage_is_review_like, workflow_stage_kind};
use self::worktree::{
    capture_and_record_rd_task_git_baseline, cleanup_rd_candidate_worktree,
    create_rd_candidate_worktree, extract_rd_candidate_diff, read_rd_repository_worktree_status,
};
#[cfg(test)]
use self::worktree::{create_rd_candidate_worktree_from_root, git_worktree_remove_is_unsupported};

const RD_SCENARIO: &str = "rd";
const RD_INTERNAL_SOURCE: &str = "rd_internal";
const RD_CANDIDATE_SOURCE: &str = "rd_internal_cand";
const RD_THREAD_SOURCE: &str = "rd_thread";
const MAX_TREE_ITEMS: usize = 1200;
const MAX_CONTEXT_BYTES: usize = 80_000;
const RD_INLINE_CONTEXT_BUDGET_BYTES: usize = 52_000;
const RD_FILE_SUMMARY_INDEX_LIMIT: usize = 2_000;
const RD_EMBEDDING_BATCH_SIZE: usize = 48;
const RD_LOCAL_EMBEDDING_BACKGROUND_BATCH_SIZE: usize = 8;
const RD_EMBEDDING_QUERY_TIMEOUT_SECS: u64 = 20;
const RD_EMBEDDING_INDEX_BATCH_TIMEOUT_SECS: u64 = 60;
const RD_EMBEDDING_SYMBOL_INDEX_LIMIT: usize = 6_000;
const RD_EMBEDDING_IMPORT_INDEX_LIMIT: usize = 2_000;
const RD_LLM_CONTEXT_SUMMARY_TIMEOUT_SECS: u64 = 90;
const RD_LLM_CONTEXT_SUMMARY_MAX_SCOPES: usize = 8;
const RD_LLM_CONTEXT_PLANNER_TIMEOUT_SECS: u64 = 90;
const RD_LLM_CONTEXT_PLANNER_MAX_TOKENS: u32 = 1_800;
const MAX_REPOSITORY_INSTRUCTION_BYTES: usize = 32_000;
const MAX_EXPLICIT_FILE_CONTEXT_FILES: usize = 8;
const MAX_EXPLICIT_FILE_CONTEXT_BYTES: usize = 48_000;
const MAX_RD_WORKFLOW_STAGE_PASSES: usize = 2;
const DEFAULT_MAX_RD_WORKFLOW_POST_STAGE_PASSES: usize = 4;
const DEFAULT_RD_RUNTIME_TIMEOUT_SECS: u64 = 1_800;
const MIN_RD_RUNTIME_TIMEOUT_SECS: u64 = 60;
const MAX_RD_RUNTIME_TIMEOUT_SECS: u64 = 10_800;

static RD_EMBEDDING_INDEX_IN_FLIGHT: OnceLock<ParkingMutex<HashMap<String, bool>>> =
    OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneratedDiffCheckStatus {
    Passed,
    Failed,
    Skipped,
}

impl GeneratedDiffCheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug)]
struct GeneratedDiffCheckOutcome {
    status: GeneratedDiffCheckStatus,
    error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RdGitBaselinePolicy {
    CurrentWorktree,
    Head,
}

impl RdGitBaselinePolicy {
    fn from_option(value: Option<&str>) -> Self {
        match value.unwrap_or("current_worktree").trim() {
            "head" | "clean_head" | "HEAD" => Self::Head,
            _ => Self::CurrentWorktree,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::CurrentWorktree => "current_worktree",
            Self::Head => "head",
        }
    }
}

#[derive(Debug, Clone)]
struct RdTaskGitBaseline {
    baseline_policy: RdGitBaselinePolicy,
    head_sha: Option<String>,
    status_short: String,
    dirty_paths: Vec<String>,
    tracked_diff_patch: String,
    untracked_files: Vec<String>,
}

impl RdTaskGitBaseline {
    fn is_dirty(&self) -> bool {
        !self.status_short.trim().is_empty()
    }
}

#[derive(Debug, Default)]
struct RdExplicitFileContext {
    files: Vec<String>,
    skipped: Vec<String>,
    text: String,
}

#[derive(Debug)]
struct RdContextBuilder {
    budget_bytes: usize,
    used_bytes: usize,
    parts: Vec<String>,
    skipped: Vec<String>,
}

impl RdContextBuilder {
    fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            parts: Vec::new(),
            skipped: Vec::new(),
        }
    }

    fn push_section(&mut self, title: &str, body: &str, max_bytes: usize) {
        let remaining = self.budget_bytes.saturating_sub(self.used_bytes);
        if remaining == 0 {
            self.skipped.push(title.to_string());
            return;
        }
        let section_budget = remaining.min(max_bytes);
        let body = truncate_text(body.trim(), section_budget);
        if body.trim().is_empty() {
            return;
        }
        let section = format!("## {title}\n{body}");
        self.used_bytes = self.used_bytes.saturating_add(section.len());
        self.parts.push(section);
    }

    fn finish(mut self) -> String {
        if !self.skipped.is_empty() {
            self.parts.push(format!(
                "## 上下文预算提示\n以下 section 因预算耗尽未内联：{}。如需要，请使用 runtime 工具按需读取。",
                self.skipped.join(", ")
            ));
        }
        truncate_text(&self.parts.join("\n\n"), self.budget_bytes)
    }
}

#[derive(Debug, Clone)]
struct RdRepositoryFileSummary {
    file_path: String,
    language: Option<String>,
    size_bytes: u64,
    mtime_ms: Option<u64>,
    content_hash: String,
    git_blob_sha: Option<String>,
    summary_text: String,
    summary_hash: String,
    symbols: Vec<String>,
    imports: Vec<String>,
}

#[derive(Debug, Clone)]
struct RdRepositoryContextSummary {
    scope_type: String,
    scope_key: String,
    source_hash: String,
    summary_text: String,
    detail_json: Value,
}

#[derive(Debug, Clone, Default)]
struct RdExistingFileSummaryCache {
    file_path: String,
    language: Option<String>,
    size_bytes: u64,
    mtime_ms: Option<u64>,
    content_hash: String,
    git_blob_sha: Option<String>,
    summary_text: String,
    summary_hash: String,
    symbols: Vec<String>,
    imports: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct RdFileSummaryIndexOutput {
    summaries: Vec<RdRepositoryFileSummary>,
    reused_count: usize,
    regenerated_count: usize,
}

#[derive(Debug, Clone, Default)]
struct RdEmbeddingIndexStats {
    total_chunks: usize,
    reused_chunks: usize,
    regenerated_chunks: usize,
    pruned_chunks: usize,
    estimated_tokens_saved: u64,
}

#[derive(Debug, Clone)]
struct RdEmbeddingApiKey {
    id: Option<String>,
    provider: String,
    #[cfg(feature = "nl2sql")]
    base_url: Option<String>,
    model: String,
    vector_space_id: String,
    dimensions: Option<usize>,
    is_local: bool,
    #[cfg(feature = "nl2sql")]
    api_key: String,
}

#[derive(Debug, Clone)]
struct RdEmbeddingInputChunk {
    chunk_id: String,
    chunk_type: RdEmbeddingChunkType,
    file_path: Option<String>,
    symbol_name: Option<String>,
    line_number: Option<u64>,
    content_hash: String,
    text: String,
    metadata_json: Value,
    task_id: Option<String>,
}

#[derive(Debug)]
struct RdEmbeddingBatchOutput {
    vectors: Vec<Vec<f32>>,
    usage: Option<api::Usage>,
}

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(token) = token else {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    };

    match crate::auth::verify_token(&state, token).await {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(_) => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/repositories", routing_get(list_repositories))
        .route("/repositories", routing_post(create_repository))
        .route("/repositories/{id}", routing_patch(update_repository))
        .route("/repositories/{id}", routing_delete(delete_repository))
        .route("/repositories/{id}/sync", routing_post(sync_repository))
        .route("/repositories/{id}/tree", routing_get(repository_tree))
        .route(
            "/repositories/{id}/file-suggestions",
            routing_get(repository_file_suggestions),
        )
        .route("/repositories/{id}/search", routing_get(repository_search))
        .route(
            "/repositories/{id}/symbols",
            routing_get(repository_symbols),
        )
        .route(
            "/repositories/{id}/imports",
            routing_get(repository_imports),
        )
        .route("/repositories/{id}/file", routing_get(repository_file))
        .route(
            "/repositories/{id}/branches",
            routing_get(repository_branches),
        )
        .route(
            "/repositories/{id}/worktree-status",
            routing_get(repository_worktree_status),
        )
        .route(
            "/repositories/{id}/code-intel/status",
            routing_get(code_intel_status),
        )
        .route(
            "/repositories/{id}/code-intel/query",
            routing_post(code_intel_query),
        )
        .route(
            "/repositories/{id}/code-intel/restart",
            routing_post(code_intel_restart),
        )
        .route(
            "/repositories/{id}/preview-sessions",
            routing_post(create_preview_session),
        )
        .route("/preview-sessions/{id}", routing_get(get_preview_session))
        .route(
            "/preview-sessions/{id}/authorize",
            routing_post(authorize_preview_session),
        )
        .route(
            "/preview-sessions/{id}/proxy",
            routing_get(preview_proxy_root),
        )
        .route(
            "/preview-sessions/{id}/proxy/{*path}",
            routing_get(preview_proxy),
        )
        .route(
            "/preview-sessions/{id}/stop",
            routing_post(stop_preview_session),
        )
        .route(
            "/preview-sessions/{id}/logs",
            routing_get(preview_session_logs),
        )
        .route(
            "/preview-sessions/{id}/screenshot",
            routing_post(preview_screenshot),
        )
        .route(
            "/preview-sessions/{id}/console-event",
            routing_post(record_preview_console_event),
        )
        .route("/quality", routing_get(get_quality_summary))
        .route("/intent-route", routing_post(route_task_intent))
        .route("/tasks", routing_get(list_tasks))
        .route("/tasks", routing_post(create_task))
        .route("/tasks/{id}", routing_get(get_task))
        .route(
            "/tasks/{id}/workbench",
            routing_get(workbench::get_task_workbench),
        )
        .route(
            "/tasks/{id}/events",
            routing_get(task_events::list_task_events),
        )
        .route(
            "/tasks/{id}/token-diagnostics",
            routing_get(task_events::task_token_diagnostics),
        )
        .route("/tasks/{id}/changes", routing_get(task_changes))
        .route("/tasks/{id}/tests", routing_get(task_tests))
        .route("/tasks/{id}/apply", routing_post(apply_task_changes))
        .route("/tasks/{id}/rollback", routing_post(rollback_task_changes))
        .route("/tasks/{id}/apply-hunks", routing_post(apply_task_hunks))
        .route("/tasks/{id}/test", routing_post(run_task_test))
        .route("/tasks/{id}/cancel", routing_post(cancel_task))
        .route("/tasks/{id}/retry", routing_post(retry_task))
        .route("/specs", routing_get(list_specs))
        .route("/specs", routing_post(create_spec))
        .route("/specs/{id}", routing_get(get_spec))
        .route("/specs/{id}", routing_patch(update_spec))
        .route("/specs/{id}", routing_delete(delete_spec))
        .route("/specs/{id}/events", routing_get(list_spec_events))
        .route("/specs/{id}/generate-spec", routing_post(generate_spec))
        .route("/specs/{id}/approve-spec", routing_post(approve_spec))
        .route("/specs/{id}/generate-design", routing_post(generate_design))
        .route("/specs/{id}/revise", routing_post(revise_spec_stage))
        .route("/specs/{id}/approve-design", routing_post(approve_design))
        .route("/specs/{id}/generate-tasks", routing_post(generate_tasks))
        .route("/specs/{id}/approve-tasks", routing_post(approve_tasks))
        .route("/specs/{id}/implement-task", routing_post(implement_task))
        .route("/specs/{id}/implement-all", routing_post(implement_all))
        .route("/specs/{id}/final-report", routing_post(final_report_spec))
        .route(
            "/specs/{id}/create-task",
            routing_post(create_task_from_spec),
        )
        .route("/agent-profiles", routing_get(list_agent_profiles))
        .route("/agent-profiles", routing_post(create_agent_profile))
        .route("/agent-profiles/{id}", routing_patch(update_agent_profile))
        .route("/agent-profiles/{id}", routing_delete(delete_agent_profile))
        .route("/agent-market/search", routing_get(search_agent_market))
        .route(
            "/agent-market/{id}/install",
            routing_post(install_agent_market_item),
        )
        .route("/agent-workflows", routing_get(list_agent_workflows))
        .route("/agent-workflows", routing_post(create_agent_workflow))
        .route(
            "/agent-workflows/{id}",
            routing_patch(update_agent_workflow),
        )
        .route(
            "/agent-workflows/{id}",
            routing_delete(delete_agent_workflow),
        )
        .route("/steering-rules", routing_get(list_steering_rules))
        .route("/steering-rules", routing_post(create_steering_rule))
        .route("/steering-rules/{id}", routing_patch(update_steering_rule))
        .route("/steering-rules/{id}", routing_delete(delete_steering_rule))
        .route(
            "/integrations",
            routing_get(integrations::list_integrations),
        )
        .route(
            "/integrations",
            routing_post(integrations::create_integration),
        )
        .route(
            "/integrations/{id}",
            routing_patch(integrations::update_integration),
        )
        .route(
            "/integrations/{id}",
            routing_delete(integrations::delete_integration),
        )
        .route(
            "/integrations/{id}/test",
            routing_post(integrations::test_integration),
        )
        .route(
            "/tasks/{id}/pr-draft",
            routing_get(integrations::task_pr_draft),
        )
        .route(
            "/tasks/{id}/pr-draft/publish",
            routing_post(integrations::publish_task_pr_draft),
        )
        .layer(axum::middleware::from_fn_with_state(state, auth_middleware))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parsed_output() -> ParsedRdOutput {
        ParsedRdOutput {
            plan_md: "读取入口文件并调整返回文案".to_string(),
            answer_md: "已在候选工作区完成修改，等待审批。".to_string(),
            review_md: None,
            pr_title: None,
            pr_description: None,
            unified_diff: None,
            touched_files: Vec::new(),
        }
    }

    #[test]
    fn candidate_context_message_preserves_pending_diff_semantics() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let files = infer_files_from_unified_diff(diff);
        let message = build_rd_candidate_context_message(
            "task-1",
            "modify",
            "repo-1",
            "candidate-session-1",
            diff,
            &files,
            3,
            &parsed_output(),
        );

        assert!(message.contains("<system-reminder>"));
        assert!(message.contains("Task ID: task-1"));
        assert!(message.contains("Candidate runtime session: candidate-session-1"));
        assert!(message.contains("src/main.rs"));
        assert!(message.contains("主仓库尚未修改"));
        assert!(message.contains("Diff artifact"));
        assert!(message.contains("diff_hash="));
        assert!(!message.contains("+new"));
        assert!(
            message.contains("previous proposed change"),
            "follow-up guidance must tell later turns how to use candidate context"
        );
    }

    #[test]
    fn candidate_context_message_handles_no_diff_without_fake_changes() {
        let message = build_rd_candidate_context_message(
            "task-2",
            "modify",
            "repo-2",
            "candidate-session-2",
            "",
            &[],
            1,
            &parsed_output(),
        );

        assert!(message.contains("无文件变更"));
        assert!(message.contains("没有产生 Git Diff"));
        assert!(!message.contains("```diff"));
    }

    #[test]
    fn candidate_runtime_tools_include_write_tools_and_diff_validator() {
        let tools = ensure_rd_candidate_runtime_tools(Some(vec![
            "read_file".to_string(),
            "read_file".to_string(),
            "grep_search".to_string(),
        ]));

        for required in [
            "read_file",
            "grep_search",
            "glob_search",
            "write_file",
            "edit_file",
            "rd_validate_diff",
        ] {
            assert!(
                tools.iter().any(|tool| tool == required),
                "{required} should be available in candidate worktree runtime"
            );
        }
        let mut deduped = tools.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(tools, deduped, "candidate tool allowlist must be stable");
    }

    #[test]
    fn rd_runtime_default_tools_exclude_bash_and_write_tools() {
        let policy = resolve_rd_runtime_tool_policy_with_switches("ask", None, false, false);

        assert!(policy.allowed_tools.iter().any(|tool| tool == "read_file"));
        assert!(policy
            .allowed_tools
            .iter()
            .any(|tool| tool == "rd_validate_diff"));
        for blocked in ["bash", "write_file", "edit_file"] {
            assert!(
                !policy.allowed_tools.iter().any(|tool| tool == blocked),
                "{blocked} must not be exposed to default RD Q&A runtime"
            );
        }
    }

    #[test]
    fn rd_runtime_filters_explicit_bash_unless_enabled() {
        let policy = resolve_rd_runtime_tool_policy_with_switches(
            "review",
            Some(vec!["read_file".to_string(), "Bash".to_string()]),
            false,
            false,
        );

        assert!(policy.filtered_bash);
        assert!(policy.allowed_tools.iter().any(|tool| tool == "read_file"));
        assert!(
            !policy
                .allowed_tools
                .iter()
                .any(|tool| rd_tool_is_bash(tool)),
            "bash must be filtered from non-candidate runtime by default"
        );
    }

    #[test]
    fn rd_runtime_filters_explicit_write_tools_unless_enabled() {
        let policy = resolve_rd_runtime_tool_policy_with_switches(
            "modify",
            Some(vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "EditFile".to_string(),
            ]),
            false,
            false,
        );

        assert_eq!(
            policy.filtered_write_tools,
            vec!["EditFile".to_string(), "write_file".to_string()]
        );
        for blocked in ["write_file", "EditFile"] {
            assert!(
                !policy.allowed_tools.iter().any(|tool| tool == blocked),
                "{blocked} must be filtered from non-candidate runtime by default"
            );
        }
    }

    #[test]
    fn rd_runtime_failed_tool_event_uses_failed_message() {
        let event = agent_gateway::AgentEvent::ToolResult {
            index: 1,
            tool_name: "glob_search".to_string(),
            input: r#"{"pattern":"["}"#.to_string(),
            output: "invalid glob pattern".to_string(),
            is_error: true,
        };

        let (stage, status, message, detail) =
            rd_runtime_event_to_task_event(event).expect("tool result should map to task event");

        assert_eq!(stage, "runtime_tool");
        assert_eq!(status, "failed");
        assert_eq!(message, "runtime 工具 glob_search 执行失败");
        assert_eq!(detail["toolCalls"][0]["isError"], true);
    }

    #[test]
    fn detects_git_worktree_remove_unsupported_usage() {
        let stderr = "usage: git worktree add [<options>] <path> [<branch>]\n   or: git worktree list [<options>]\n   or: git worktree lock [<options>] <path>\n   or: git worktree prune [<options>]\n   or: git worktree unlock <path>\n";

        assert!(git_worktree_remove_is_unsupported(stderr));
        assert!(!git_worktree_remove_is_unsupported(
            "fatal: '/tmp/aos' is not a working tree"
        ));
    }

    #[test]
    fn candidate_runtime_default_tools_keep_editing_but_exclude_bash() {
        let tools = ensure_rd_candidate_runtime_tools_with_bash(None, false);

        for required in [
            "read_file",
            "grep_search",
            "glob_search",
            "write_file",
            "edit_file",
        ] {
            assert!(
                tools.iter().any(|tool| tool == required),
                "{required} should be available in candidate worktree runtime"
            );
        }
        assert!(
            !tools.iter().any(|tool| rd_tool_is_bash(tool)),
            "candidate runtime should use AOS-managed test commands instead of default bash"
        );
    }

    #[test]
    fn workflow_stage_kind_routes_template_stages_to_real_execution_slots() {
        let architecture = RdWorkflowStageSpec {
            id: "architecture".to_string(),
            agent: "Architecture Agent".to_string(),
            mode: "ask".to_string(),
            goal: "理解仓库结构、相关文件、风险和验证命令".to_string(),
        };
        let implementation = RdWorkflowStageSpec {
            id: "implementation".to_string(),
            agent: "Coding Agent".to_string(),
            mode: "modify".to_string(),
            goal: "生成可审查 Diff".to_string(),
        };
        let review = RdWorkflowStageSpec {
            id: "review".to_string(),
            agent: "Review Agent".to_string(),
            mode: "review".to_string(),
            goal: "输出 findings-first 审查和风险".to_string(),
        };

        assert_eq!(
            workflow_stage_kind(&architecture),
            RdWorkflowStageKind::Preflight
        );
        assert_eq!(
            workflow_stage_kind(&implementation),
            RdWorkflowStageKind::MainImplementation
        );
        assert_eq!(
            workflow_stage_kind(&review),
            RdWorkflowStageKind::Postflight
        );
        assert!(workflow_stage_is_review_like(&review));
    }

    #[tokio::test]
    async fn candidate_worktree_extracts_real_git_diff_end_to_end() {
        let root =
            std::env::temp_dir().join(format!("aos-rd-candidate-e2e-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("src"))
            .await
            .expect("create temp repository");
        tokio::fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }\n")
            .await
            .expect("write initial file");
        run_git_for_test(&root, &["init"]).await;
        run_git_for_test(&root, &["add", "src/lib.rs"]).await;
        run_git_for_test(
            &root,
            &[
                "-c",
                "user.email=aos@example.invalid",
                "-c",
                "user.name=AOS Test",
                "commit",
                "-m",
                "init",
            ],
        )
        .await;

        let task_id = format!("task-{}", uuid::Uuid::new_v4());
        let candidate = create_rd_candidate_worktree_from_root(&root, &task_id)
            .await
            .expect("create candidate worktree");
        tokio::fs::write(
            candidate.path.join("src/lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .await
        .expect("edit candidate file");

        let diff = extract_rd_candidate_diff(&candidate.path)
            .await
            .expect("extract candidate diff");

        assert!(diff.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(diff.contains("-pub fn answer() -> i32 { 1 }"));
        assert!(diff.contains("+pub fn answer() -> i32 { 42 }"));
        assert_eq!(
            tokio::fs::read_to_string(root.join("src/lib.rs"))
                .await
                .expect("read main worktree"),
            "pub fn answer() -> i32 { 1 }\n",
            "candidate edits must not mutate the main repository"
        );

        cleanup_rd_candidate_worktree(&candidate).await;
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    async fn run_git_for_test(root: &Path, args: &[&str]) {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .await
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn metric_count_rounds_and_clamps_metric_totals() {
        let mut totals = HashMap::new();
        totals.insert("ok".to_string(), 2.49);
        totals.insert("negative".to_string(), -4.0);

        assert_eq!(metric_count(&totals, "ok"), 2);
        assert_eq!(metric_count(&totals, "missing"), 0);
        assert_eq!(metric_count(&totals, "negative"), 0);
    }

    #[test]
    fn review_quality_counts_file_and_line_level_refs() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10 +10 @@\n-old\n+new\n";
        let quality = analyze_review_quality(
            "- 严重: src/main.rs:10 这里会导致回归，缺少测试。\n- low: README.md only wording.",
            diff,
            &["src/main.rs".to_string()],
            &[],
        );

        assert_eq!(quality.findings_count, 2);
        assert_eq!(quality.file_ref_count, 1);
        assert_eq!(quality.line_ref_count, 1);
    }

    #[test]
    fn review_quality_treats_explicit_no_findings_as_zero() {
        let quality = analyze_review_quality(
            "没有明显问题。Residual risk: 未运行完整集成测试。",
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            &["src/lib.rs".to_string()],
            &[],
        );

        assert_eq!(quality.findings_count, 0);
        assert_eq!(quality.file_ref_count, 0);
        assert_eq!(quality.line_ref_count, 0);
    }

    #[test]
    fn direct_fallback_is_disabled_by_default_and_requires_explicit_opt_in() {
        assert!(!parse_rd_runtime_direct_fallback_enabled(None));
        assert!(!parse_rd_runtime_direct_fallback_enabled(Some("false")));
        assert!(!parse_rd_runtime_direct_fallback_enabled(Some("off")));
        assert!(parse_rd_runtime_direct_fallback_enabled(Some("true")));
        assert!(parse_rd_runtime_direct_fallback_enabled(Some("direct")));
        assert!(parse_rd_runtime_direct_fallback_enabled(Some("completion")));
    }

    #[test]
    fn rd_runtime_timeout_defaults_to_long_code_task_window() {
        assert_eq!(
            parse_rd_runtime_timeout_secs(None),
            DEFAULT_RD_RUNTIME_TIMEOUT_SECS
        );
        assert_eq!(parse_rd_runtime_timeout_secs(Some("5")), 60);
        assert_eq!(parse_rd_runtime_timeout_secs(Some("900")), 900);
        assert_eq!(parse_rd_runtime_timeout_secs(Some("999999")), 10_800);
        assert_eq!(
            parse_rd_runtime_timeout_secs(Some("not-a-number")),
            DEFAULT_RD_RUNTIME_TIMEOUT_SECS
        );
    }

    #[test]
    fn rd_candidate_source_fits_legacy_agent_session_column() {
        assert!(
            RD_CANDIDATE_SOURCE.len() <= 20,
            "legacy agent_sessions.source was varchar(20); keep source short even though newer migrations expand it"
        );
    }

    #[test]
    fn rd_runtime_error_detects_model_auth_failure() {
        assert!(rd_runtime_error_is_auth_failure(
            r#"runtime error: all API keys failed: API error (401 Unauthorized): {"error":{"message":"用户信息验证失败","type":"authentication_error"}}"#
        ));
        assert!(rd_runtime_error_is_auth_failure(
            "all API keys failed: Invalid API key. Please check your configuration."
        ));
        assert!(!rd_runtime_error_is_auth_failure(
            "RD runtime turn timed out after 1800s"
        ));
    }
}
