//! Agent route module assembly and exports.

mod agent_chat_adversarial;
mod agent_chat_adversarial_domain;
mod agent_chat_adversarial_support;
mod agent_chat_turn_engine;
mod agent_constants;
mod agent_context_memory_api;
mod agent_core_helpers;
mod agent_dtos;
mod agent_history_api;
mod agent_imports;
mod agent_internal_state;
mod agent_pm_alignment;
mod agent_pm_answer_finalize;
mod agent_pm_answer_postprocess;
mod agent_pm_contract;
mod agent_pm_history_utils;
mod agent_pm_live_retrieve;
mod agent_pm_llm_review;
mod agent_pm_memory;
mod agent_pm_ops_api;
mod agent_pm_orch_quality;
mod agent_pm_orchestrated_turn;
mod agent_pm_persist;
mod agent_pm_preflight;
mod agent_pm_probe_exec;
mod agent_pm_prompts;
mod agent_pm_quality;
mod agent_pm_route_planning;
mod agent_pm_runtime_governance;
mod agent_pm_runtime_helpers;
mod agent_pm_task_api;
mod agent_pm_task_manager;
mod agent_pm_task_runtime;
mod agent_pm_telemetry;
mod agent_router;
mod agent_session_api;
mod agent_stream_session;

#[cfg(feature = "bot-agents")]
pub(crate) use agent_chat_adversarial::default_chat_adversarial_models;
pub(crate) use agent_chat_adversarial::{
    mark_chat_adversarial_failed_from_parent, request_chat_adversarial_cancel_from_agent_ops,
    start_chat_adversarial_run_from_bot, ChatAdversarialBotRunInput,
};
pub(crate) use agent_core_helpers::maybe_dispatch_skill_command;
pub(crate) use agent_dtos::{PmTaskDocumentInput, PmTaskImageInput, PmTaskInputContext};
use agent_imports::*;
pub use agent_internal_state::run_pm_background_runtime_cycle;
pub(crate) use agent_pm_probe_exec::resolve_pm_native_search_runtime;
pub(crate) use agent_pm_task_api::{
    build_pm_task_image_summary, normalize_pm_task_input_context,
    request_pm_research_task_cancel_from_agent_ops, resume_pm_research_task_from_agent_ops,
};
#[cfg(feature = "bot-agents")]
pub(crate) use agent_pm_task_api::{start_pm_research_task_from_bot, PmResearchBotTaskInput};
pub(crate) use agent_pm_telemetry::{PmTelemetryEvent, PmTelemetrySink};
pub use agent_router::routes;

#[cfg(feature = "bot-agents")]
pub(crate) async fn load_pm_task_progress_events(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    after_event_id: u64,
    limit: i64,
) -> std::result::Result<Vec<(u64, serde_json::Value)>, crate::error::AppError> {
    let events = agent_pm_task_runtime::load_pm_task_events_from_db(
        db,
        task_id,
        tenant_id,
        user_id,
        session_id,
        after_event_id,
        limit,
    )
    .await?;
    Ok(events
        .into_iter()
        .map(|(id, event)| {
            (
                id,
                serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect())
}

#[cfg(feature = "bot-agents")]
pub(crate) async fn load_pm_task_answer_events(
    db: &sqlx::SqlitePool,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    after_event_id: u64,
    limit: i64,
) -> std::result::Result<Vec<(u64, serde_json::Value)>, crate::error::AppError> {
    let events = agent_pm_task_runtime::load_pm_task_stream_events_from_db(
        db,
        task_id,
        tenant_id,
        user_id,
        after_event_id,
        limit,
    )
    .await?;
    Ok(events
        .into_iter()
        .map(|(id, event)| {
            (
                id,
                serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect())
}
