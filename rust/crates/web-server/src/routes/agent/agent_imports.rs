pub(super) use std::{
    any::Any,
    collections::{HashMap, HashSet, VecDeque},
    env,
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) use async_stream::try_stream;
pub(super) use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
    },
    Json, Router,
};
pub(super) use futures_util::{FutureExt, StreamExt, TryStreamExt};
pub(super) use serde::{Deserialize, Serialize};
pub(super) use sqlx::Row;
pub(super) use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit, Semaphore};
pub(super) use tokio::time::{sleep, timeout};

pub(super) use crate::auth::Claims;
pub(super) use crate::error::AppError;
pub(super) use crate::state::AppState;
pub(super) use pm_domain::budget::{PmBudgetProfile, PmTimeoutBudget};
pub(super) use pm_domain::deep_research_loop::{
    pm_deep_loop_max_wall_secs, pm_deep_loop_min_synthesis_window_secs,
    pm_deep_loop_no_new_evidence_limit, PmDeepResearchAction, PmDeepResearchLoop,
    PmDeepResearchLoopInput,
};
pub(super) use pm_domain::planner::PmExecConstraints;
pub(super) use pm_domain::probe_plan::{
    build_pm_probe_candidates, collect_enabled_pm_routes, is_pm_route_blocked,
    is_pm_route_over_quota, pick_pm_attempt_preferences,
    pick_pm_attempt_preferences_with_source_quota_and_blocked, pick_pm_subtask_focus_for_repair,
    pick_pm_subtask_gap_retry_variant_for_attempt, pm_route_usage_key,
    pm_should_consume_source_quota, prioritize_pm_probe_candidates_for_subtasks,
    record_pm_route_failure_and_maybe_block, record_pm_route_success, PmEnabledRoute,
    PmProbeCandidate,
};
pub(super) use pm_domain::repair::PmRepairStrategy;
pub(super) use pm_domain::report_strategy::{
    apply_pm_report_semantic_extraction, apply_pm_report_strategy_plan,
    detect_pm_report_strategy_signal, extract_pm_first_party_evidence, pm_is_report_strategy_mode,
    PmReportSemanticExtraction,
};
pub(super) use pm_domain::route_rank::{
    contains_cjk, pm_route_health_key, rank_pm_plan_routes_with_scores, PmRouteHealthSignal,
};
pub(super) use pm_domain::subtask_runtime::{
    collect_pm_subtask_runtime_metas, resolve_subtask_runtime_key, PmSubtaskRuntimeMeta,
};
pub(super) use pm_domain::task_graph::{
    apply_pm_exec_constraints_to_plan, apply_pm_task_graph_to_plan, build_pm_fallback_task_graph,
    build_pm_stage_plan, detect_pm_task_graph_issue, extract_pm_task_graph,
};
pub(super) use pm_domain::turn_router::{
    apply_pm_turn_route_to_plan, build_pm_fallback_turn_route, extract_pm_turn_route,
    pm_plan_turn_route, pm_turn_route_allows_deep_strategy, PmReasoningDepth, PmRouteEngine,
    PmSearchNeed, PmSearchPolicy, PmTurnClass, PmTurnRoute,
};
pub(super) use pm_orchestrator::events::pm_stage_user_message as pm_stage_user_message_v2;
pub(super) use pm_orchestrator::persistence::{
    get_pm_budget_profile, get_pm_budget_profile_config,
    list_pm_subtask_attempts_by_task_and_subtask, list_pm_subtask_runs_by_task,
    load_open_pm_domain_circuit_keys, load_pm_retry_not_before_ms, load_pm_route_circuit_state,
    persist_pm_run_finish, persist_pm_run_start, record_pm_audit_event, record_pm_prompt_usage,
    report_pm_domain_circuit_failure, report_pm_domain_circuit_success,
    report_pm_route_circuit_failure, report_pm_route_circuit_success,
    upsert_pm_claim_verdict_batch, upsert_pm_conflict_case_batch, upsert_pm_provider_health,
    upsert_pm_quality_gate_metrics, upsert_pm_retry_not_before_ms, upsert_pm_route_bandit_state,
    upsert_pm_route_learning_feature, upsert_pm_subtask_attempt, upsert_pm_subtask_run,
    PmClaimVerdictRow, PmConflictCaseRow, PmRunConfigSnapshot, PmRunFinishPayload,
    PmSourceSlotUpsertPayload, PmSubtaskAttemptUpsertPayload, PmSubtaskRunUpsertPayload,
    PmToolCallLedgerRow,
};
pub(super) use pm_report::{
    build_pm_query_variants, build_pm_report_artifact, extract_first_json_object,
    extract_http_urls, extract_named_json_object, extract_pm_visible_answer_text,
    extract_url_domain, first_non_empty_line, is_pm_high_signal_source_url,
    is_pm_visible_output_noise, normalize_claim_key, normalize_http_url_candidate,
    parse_json_object_relaxed, sha256_hex, strip_pm_list_prefix, tokenize_for_match,
    truncate_for_log, PmAnswerQualityDto, PmClaimEvidenceDto, PmConflictEdgeDto,
    PmConflictGraphDto, PmConflictRowDto, PmEvidenceLeafDto, PmEvidenceTreeNodeDto,
    PmReportArtifactDto,
};

pub(super) use agent_gateway::{
    AgentSessionManager, GatewayError, SessionHandle, SessionInfo, SessionState, TokenUsageRecord,
    TurnResult,
};

pub(super) use super::agent_chat_adversarial::{
    cancel_chat_adversarial_run, delete_chat_adversarial_run_thread, get_chat_adversarial_run,
    get_chat_adversarial_run_thread, list_chat_adversarial_runs, start_chat_adversarial_run,
    stream_chat_adversarial_run_events, update_chat_adversarial_run_thread,
};
pub(super) use super::agent_chat_turn_engine::{plan_chat_turn, ChatTurnEngineInput};
pub(super) use super::agent_constants::*;
pub(super) use super::agent_context_memory_api::{
    compact_session_context, get_session_context_status, list_session_compactions,
    list_session_memory_citations, patch_session_memory_mode,
};
pub(super) use super::agent_core_helpers::{auth_middleware, turn_to_run_turn_response};
pub(super) use super::agent_dtos::*;
pub(super) use super::agent_history_api::{
    get_session_history, sanitize_pm_user_message, wrap_pm_research_prompt,
};
pub(super) use super::agent_internal_state::{
    pm_background_worker_runtime, PmAnswerDeltaCallback, PmProbeOutcome, PmResearchRunPermit,
    PmResearchTaskConfig, PmResearchTaskManager, PmResearchTaskRecord,
};
pub(super) use super::agent_pm_alignment::{
    build_pm_conflict_graph, extract_claim_alignment, extract_conflict_matrix,
};
pub(super) use super::agent_pm_answer_finalize::finalize_pm_answer_text_with_repair_flag;
pub(super) use super::agent_pm_answer_postprocess::{
    apply_hard_alignment_from_tool_results, build_pm_emergency_conclusion_text,
    build_pm_preface_fallback, build_pm_tool_evidence_hits, build_pm_websearch_content_chars_map,
    pm_is_citable_url_by_content_chars, pm_is_tool_diagnostic_excerpt, push_pm_emergency_url,
    PmToolEvidenceHit,
};
pub(super) use super::agent_pm_contract::{
    apply_pm_contract_gate, detect_exec_constraints_issue, extract_pm_exec_constraints,
    validate_exec_constraints_contract,
};
pub(super) use super::agent_pm_history_utils::{
    flush_pending_pm_internal_history, merge_pending_pm_assistant, paginate_history_messages,
    push_history_message_dedup,
};
pub(super) use super::agent_pm_live_retrieve::run_pm_retrieve_turn_with_live_events;
pub(super) use super::agent_pm_llm_review::{
    run_pm_llm_expert_review, run_pm_llm_final_editor_if_needed, PmLlmExpertReview,
};
pub(super) use super::agent_pm_memory::{
    build_pm_recent_history_context_prompt, build_pm_session_memory_prompt,
    create_pm_session_memory, delete_pm_session_memory, get_pm_session_memory_pause,
    list_pm_session_memories, pause_pm_session_memory, persist_pm_session_memory_candidate,
    persist_pm_session_summary_after_turn, update_pm_session_memory,
};
pub(super) use super::agent_pm_ops_api::{
    get_pm_research_run_trace, list_pm_audit_trails, list_pm_budget_profiles,
    list_pm_failure_taxonomy, list_pm_knowledge_coverage_warnings, list_pm_prompt_registry,
    list_pm_provider_health, list_pm_quality_gate_summary, list_pm_research_runs,
    list_pm_route_learning_features, list_pm_runtime_insights, list_pm_slo_summary,
    list_pm_strategy_leaderboard, record_pm_strategy_outcome, set_pm_budget_profile,
};
pub(super) use super::agent_pm_orch_quality::{
    admit_pm_external_evidence, apply_pm_conflict_gate, apply_pm_depth_coverage_gate,
    apply_pm_evidence_admission_gate, apply_pm_report_strategy_quality_gate,
    build_pm_local_strategy_synthesis_turn, build_pm_preserved_partial_turn,
    build_pm_synthesis_continuation_prompt, build_runtime_error_quality,
    degrade_pm_quality_with_reason, evaluate_pm_answer_quality, finalize_pm_orchestration_result,
    merge_pm_streamed_answer_parts, pm_attach_force_synth_diag, pm_retry_strategy,
    pm_source_slot_timeout_for_strategy, run_pm_force_synthesize_fallback_turn_with_observed_tools,
    PmDepthCoverageGateResult,
};
pub(super) use super::agent_pm_orchestrated_turn::run_pm_orchestrated_turn;
pub(super) use super::agent_pm_persist::{
    build_pm_tool_summary_value, classify_pm_runtime_error_code, classify_pm_tool_error_code,
    persist_pm_claim_and_conflict_records, persist_pm_evidence_graph,
    persist_pm_source_slot_and_tool_ledger, pick_primary_tool_url, score_pm_probe_quality,
};
pub(super) use super::agent_pm_preflight::{run_pm_startup_preflight, PmStartupPreflightOutcome};
pub(super) use super::agent_pm_probe_exec::{
    build_pm_observed_tool_context, build_pm_probe_repair_context,
    collect_pm_disallowed_research_tools, merge_pm_probe_turns, merge_pm_tool_calls_unique,
    merge_pm_turn_with_observed_tool_calls, pm_blocked_non_search_research_tools,
    run_pm_probe_turn, should_fast_fail_after_tool_errors,
};
pub(super) use super::agent_pm_prompts::{
    build_pm_contract_repair_prompt, build_pm_expert_only_final_prompt,
    build_pm_force_synthesize_prompt, build_pm_force_synthesize_reduce_prompt,
    build_pm_report_semantic_extract_prompt, build_pm_retrieve_prompt, build_pm_retry_prompt,
    build_pm_subtask_map_prompt, build_pm_task_graph_repair_prompt,
    build_pm_understand_plan_prompt, extract_pm_preface_visible_text,
};
pub(super) use super::agent_pm_quality::{
    apply_pm_first_party_quality_policy, build_pm_direct_answer_quality,
    build_pm_direct_answer_timeout_fallback, pick_preferred_pm_result, pm_is_deliverable_quality,
    pm_is_soft_deliverable_quality, pm_quality_delivery_score, pm_synthesize_stage_status,
    update_best_pm_turn_quality,
};
pub(super) use super::agent_pm_route_planning::{
    blocked_domains_from_usage, collect_pm_domain_tool_outcomes, collect_pm_turn_domains,
    load_pm_historical_evidence_hints, load_pm_route_health_scores, load_pm_route_scores,
    merge_blocked_domains, pick_pm_attempt_preferences_for_strategy, rank_pm_plan_routes,
};
pub(super) use super::agent_pm_runtime_governance::{
    format_panic_payload, pm_apply_retry_governance_delay, pm_background_task_deadline_secs,
    pm_contract_repair_max_retries, pm_contract_repair_turn_timeout_secs,
    pm_direct_answer_turn_timeout_secs, pm_domain_circuit_report, pm_env_u64, pm_env_usize,
    pm_flag_enabled, pm_force_synth_turn_timeout_secs, pm_preface_turn_timeout_secs,
    pm_preflight_circuit_breakers, pm_report_semantic_extract_timeout_secs,
    pm_retrieve_circuit_allow, pm_retrieve_circuit_report, pm_retrieve_circuit_route_key,
    pm_timeout_recovery_wait_secs, resolve_pm_budget_snapshot,
    run_pm_background_runtime_cycle_impl, PmEndpointCircuitState,
};
pub(super) use super::agent_pm_runtime_helpers::{
    repair_exec_constraints_with_retries, repair_task_graph_with_retries,
    run_pm_internal_turn_with_timeout_cleanup_and_options,
    run_pm_turn_streaming_with_timeout_cleanup_and_options,
    run_pm_turn_with_timeout_cleanup_and_options, run_pm_user_visible_answer_streaming_turn,
    run_pm_user_visible_answer_streaming_turn_preserving_partial, PmTransientSessionGuard,
};
pub(super) use super::agent_pm_task_api::{
    build_pm_effective_user_message_for_task, cancel_pm_research_task, get_pm_research_task_status,
    get_pm_research_task_subtask_attempts, get_pm_research_task_subtasks, resume_pm_research_task,
    spawn_pm_research_task, start_pm_research_task, stream_pm_research_task_events,
};
pub(super) use super::agent_pm_task_manager::pm_research_task_manager;
pub(super) use super::agent_pm_task_runtime::{
    assign_pm_task_bindings_to_history_messages, build_pm_task_record_from_runtime_row,
    complete_pm_task_with_local_recovery, force_finish_elapsed_pm_task_deadline,
    has_running_pm_task_for_session, load_claimable_pm_task_runtime_rows,
    load_latest_completed_pm_task_answer_from_db, load_pm_session_history_replay_from_db,
    load_pm_session_task_bindings_from_db, load_pm_task_events_from_db,
    load_pm_task_resume_context_from_db, load_pm_task_runtime_row_from_db,
    load_pm_task_snapshot_from_db, load_pm_task_stream_events_from_db,
    persist_pm_task_record_and_event, pm_task_deadline_elapsed, pm_task_event_is_terminal,
    pm_task_is_terminal_status, pm_task_research_config, pm_task_worker_id,
    reconcile_pm_task_history_turns, release_pm_task_lease, seed_pm_task_resume_checkpoint,
    touch_pm_task_lease, try_claim_pm_task_lease, PmResumeCheckpoint, PmTaskRuntimeRow,
};
pub(super) use super::agent_router::get_agent_manager;
pub(super) use super::agent_session_api::{
    branch_session, cancel_session_turn, create_session, delete_session, get_commands, get_session,
    get_session_state, list_sessions, rename_session, run_turn, toggle_bookmark_session,
    toggle_pin_session,
};
pub(super) use super::agent_stream_session::stream_session;
