use super::*;

pub(super) fn get_agent_manager(state: &AppState) -> &Arc<AgentSessionManager> {
    state.agent_manager()
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/sessions", routing_post(create_session))
        .route("/sessions", routing_get(list_sessions))
        .route("/sessions/{session_id}", routing_get(get_session))
        .route("/sessions/{session_id}", routing_patch(rename_session))
        .route("/sessions/{session_id}", routing_delete(delete_session))
        .route(
            "/sessions/{session_id}/pin",
            routing_post(toggle_pin_session),
        )
        .route(
            "/sessions/{session_id}/bookmark",
            routing_post(toggle_bookmark_session),
        )
        .route(
            "/sessions/{session_id}/branch",
            routing_post(branch_session),
        )
        .route("/sessions/{session_id}/turn", routing_post(run_turn))
        .route(
            "/sessions/{session_id}/pm-research-tasks",
            routing_post(start_pm_research_task),
        )
        .route(
            "/sessions/{session_id}/pm-memories",
            routing_get(list_pm_session_memories).post(create_pm_session_memory),
        )
        .route(
            "/sessions/{session_id}/pm-memories/pause",
            routing_get(get_pm_session_memory_pause).post(pause_pm_session_memory),
        )
        .route(
            "/sessions/{session_id}/pm-memories/{memory_id}",
            routing_patch(update_pm_session_memory).delete(delete_pm_session_memory),
        )
        .route(
            "/sessions/{session_id}/stream",
            routing_post(stream_session),
        )
        .route("/sessions/{session_id}/stream", routing_get(stream_session))
        .route(
            "/sessions/{session_id}/cancel-turn",
            routing_post(cancel_session_turn),
        )
        .route(
            "/sessions/{session_id}/context-status",
            routing_get(get_session_context_status),
        )
        .route(
            "/sessions/{session_id}/compact",
            routing_post(compact_session_context),
        )
        .route(
            "/sessions/{session_id}/compactions",
            routing_get(list_session_compactions),
        )
        .route(
            "/sessions/{session_id}/memory-mode",
            routing_patch(patch_session_memory_mode),
        )
        .route(
            "/sessions/{session_id}/memory-citations",
            routing_get(list_session_memory_citations),
        )
        .route(
            "/chat-adversarial-runs",
            routing_post(start_chat_adversarial_run),
        )
        .route(
            "/chat-adversarial-runs",
            routing_get(list_chat_adversarial_runs),
        )
        .route(
            "/chat-adversarial-runs/{run_id}",
            routing_get(get_chat_adversarial_run),
        )
        .route(
            "/chat-adversarial-runs/{run_id}/events",
            routing_get(stream_chat_adversarial_run_events),
        )
        .route(
            "/chat-adversarial-runs/{run_id}/cancel",
            routing_post(cancel_chat_adversarial_run),
        )
        .route(
            "/chat-adversarial-runs/{run_id}/thread",
            routing_get(get_chat_adversarial_run_thread)
                .patch(update_chat_adversarial_run_thread)
                .delete(delete_chat_adversarial_run_thread),
        )
        .route(
            "/pm-research-tasks/{task_id}",
            routing_get(get_pm_research_task_status),
        )
        .route(
            "/pm-research-tasks/{task_id}/subtasks",
            routing_get(get_pm_research_task_subtasks),
        )
        .route(
            "/pm-research-tasks/{task_id}/subtasks/{subtask_id}/attempts",
            routing_get(get_pm_research_task_subtask_attempts),
        )
        .route(
            "/pm-research-tasks/{task_id}/events",
            routing_get(stream_pm_research_task_events),
        )
        .route(
            "/pm-research-tasks/{task_id}/cancel",
            routing_post(cancel_pm_research_task),
        )
        .route(
            "/pm-research-tasks/{task_id}/resume",
            routing_post(resume_pm_research_task),
        )
        .route(
            "/pm-strategy-records",
            routing_post(record_pm_strategy_outcome),
        )
        .route(
            "/pm-strategy-leaderboard",
            routing_get(list_pm_strategy_leaderboard),
        )
        .route("/pm-budget-profiles", routing_get(list_pm_budget_profiles))
        .route(
            "/pm-budget-profiles/activate",
            routing_post(set_pm_budget_profile),
        )
        .route("/pm-research-runs", routing_get(list_pm_research_runs))
        .route(
            "/pm-research-runs/{run_id}/trace",
            routing_get(get_pm_research_run_trace),
        )
        .route("/pm-slo-summary", routing_get(list_pm_slo_summary))
        .route(
            "/pm-failure-taxonomy",
            routing_get(list_pm_failure_taxonomy),
        )
        .route(
            "/pm-quality-gate-summary",
            routing_get(list_pm_quality_gate_summary),
        )
        .route(
            "/pm-knowledge-coverage-warnings",
            routing_get(list_pm_knowledge_coverage_warnings),
        )
        .route("/pm-provider-health", routing_get(list_pm_provider_health))
        .route(
            "/pm-route-learning-features",
            routing_get(list_pm_route_learning_features),
        )
        .route(
            "/pm-runtime-insights",
            routing_get(list_pm_runtime_insights),
        )
        .route("/pm-prompt-registry", routing_get(list_pm_prompt_registry))
        .route("/pm-audit-trails", routing_get(list_pm_audit_trails))
        .route(
            "/sessions/{session_id}/state",
            routing_get(get_session_state),
        )
        .route(
            "/sessions/{session_id}/history",
            routing_get(get_session_history),
        )
        .route("/commands", routing_get(get_commands))
        .layer(axum::middleware::from_fn_with_state(state, auth_middleware))
}
