use super::*;
use pm_domain::prompts as pm_prompts;

pub(super) fn build_pm_retrieve_prompt(
    original_question: &str,
    plan: &serde_json::Value,
    preferred_variant: Option<&str>,
    preferred_route_id: Option<&str>,
    attempt: usize,
    runtime_budget: &PmTimeoutBudget,
    source_slot_budget_secs: u64,
    blocked_domains: &[String],
) -> String {
    pm_prompts::build_pm_retrieve_prompt(
        original_question,
        plan,
        preferred_variant,
        preferred_route_id,
        attempt,
        runtime_budget,
        source_slot_budget_secs,
        blocked_domains,
    )
}

pub(super) fn build_pm_understand_plan_prompt(
    original_question: &str,
    plan: &serde_json::Value,
    runtime_budget: &PmTimeoutBudget,
) -> String {
    pm_prompts::build_pm_understand_plan_prompt(original_question, plan, runtime_budget)
}

pub(super) fn build_pm_report_semantic_extract_prompt(
    original_question: &str,
    plan: &serde_json::Value,
) -> String {
    pm_prompts::build_pm_report_semantic_extract_prompt(original_question, plan)
}

pub(super) fn build_pm_contract_repair_prompt(
    contract_name: &str,
    user_question: &str,
    previous_output: &str,
    runtime_budget: &PmTimeoutBudget,
    planned_route_ids: &[String],
    validation_issue: &str,
    attempt: usize,
    max_attempts: usize,
) -> String {
    pm_prompts::build_pm_contract_repair_prompt(
        contract_name,
        user_question,
        previous_output,
        runtime_budget,
        planned_route_ids,
        validation_issue,
        attempt,
        max_attempts,
    )
}

pub(super) fn build_pm_task_graph_repair_prompt(
    user_question: &str,
    previous_output: &str,
    validation_issue: &str,
    attempt: usize,
    max_attempts: usize,
) -> String {
    pm_prompts::build_pm_task_graph_repair_prompt(
        user_question,
        previous_output,
        validation_issue,
        attempt,
        max_attempts,
    )
}

pub(super) fn extract_pm_preface_visible_text(preface_text: &str) -> String {
    pm_prompts::extract_pm_preface_visible_text(preface_text)
}

pub(super) fn build_pm_retry_prompt(
    original_question: &str,
    previous_answer: &str,
    quality: &PmAnswerQualityDto,
    strategy: PmRepairStrategy,
    next_attempt: usize,
    preferred_variant: Option<&str>,
    preferred_route: Option<&str>,
    preferred_route_channel: Option<&str>,
    preferred_execution_channel: Option<&str>,
    runtime_budget: &PmTimeoutBudget,
    source_slot_budget_secs: u64,
    blocked_domains: &[String],
) -> String {
    pm_prompts::build_pm_retry_prompt(
        original_question,
        previous_answer,
        pm_prompts::PmRetryPromptQuality {
            missing: &quality.missing,
            suggestions: &quality.suggestions,
        },
        strategy,
        next_attempt,
        preferred_variant,
        preferred_route,
        preferred_route_channel,
        preferred_execution_channel,
        runtime_budget,
        source_slot_budget_secs,
        blocked_domains,
    )
}

pub(super) fn build_pm_force_synthesize_prompt(
    original_question: &str,
    previous_answer: &str,
    attempt: usize,
) -> String {
    pm_prompts::build_pm_force_synthesize_prompt(original_question, previous_answer, attempt)
}

pub(super) fn build_pm_subtask_map_prompt(
    original_question: &str,
    subtask_title: &str,
    subtask_context: &str,
    attempt: usize,
    map_index: usize,
    map_total: usize,
) -> String {
    pm_prompts::build_pm_subtask_map_prompt(
        original_question,
        subtask_title,
        subtask_context,
        attempt,
        map_index,
        map_total,
    )
}

pub(super) fn build_pm_force_synthesize_reduce_prompt(
    original_question: &str,
    reduce_context: &str,
    attempt: usize,
) -> String {
    pm_prompts::build_pm_force_synthesize_reduce_prompt(original_question, reduce_context, attempt)
}

pub(super) fn build_pm_expert_only_final_prompt(
    original_question: &str,
    context: &str,
    failure_reason: &str,
    attempt: usize,
) -> String {
    pm_prompts::build_pm_expert_only_final_prompt(
        original_question,
        context,
        failure_reason,
        attempt,
    )
}
