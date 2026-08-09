//! Prompt builders for Code Studio Vibe and Plan modes.

use crate::routes::builtin_skills::{PromptId, PromptRegistry};

pub(super) fn plan_generate_spec_prompt(requirement: &str, repo_context: &str) -> String {
    PromptRegistry::render(
        PromptId::CodeStudioSpecGenerateSpec,
        &[("requirement", requirement), ("repo_context", repo_context)],
    )
}

pub(super) fn plan_generate_design_prompt(
    requirement: &str,
    requirements_md: &str,
    acceptance_md: &str,
    repo_context: &str,
) -> String {
    PromptRegistry::render(
        PromptId::CodeStudioSpecGenerateDesign,
        &[
            ("requirement", requirement),
            ("requirements_md", requirements_md),
            ("acceptance_md", acceptance_md),
            ("repo_context", repo_context),
        ],
    )
}

pub(super) fn plan_generate_tasks_prompt(
    requirement: &str,
    requirements_md: &str,
    design_md: &str,
    acceptance_md: &str,
    repo_context: &str,
) -> String {
    PromptRegistry::render(
        PromptId::CodeStudioSpecGenerateTasks,
        &[
            ("requirement", requirement),
            ("requirements_md", requirements_md),
            ("design_md", design_md),
            ("acceptance_md", acceptance_md),
            ("repo_context", repo_context),
        ],
    )
}

pub(super) fn plan_implement_task_prompt(
    title: &str,
    requirements_md: &str,
    design_md: &str,
    task_item_json: &str,
) -> String {
    PromptRegistry::render(
        PromptId::CodeStudioSpecImplementTask,
        &[
            ("title", title),
            ("requirements_md", requirements_md),
            ("design_md", design_md),
            ("task_item_json", task_item_json),
        ],
    )
}

pub(super) fn plan_final_report_prompt(
    title: &str,
    requirements_md: &str,
    design_md: &str,
    tasks_md: &str,
    implementation_summary_json: &str,
) -> String {
    PromptRegistry::render(
        PromptId::CodeStudioSpecFinalReport,
        &[
            ("title", title),
            ("requirements_md", requirements_md),
            ("design_md", design_md),
            ("tasks_md", tasks_md),
            ("implementation_summary_json", implementation_summary_json),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_prompts_preserve_json_schemas() {
        let spec = plan_generate_spec_prompt("需求", "上下文");
        assert!(spec.contains("\"requirementsMd\""));
        assert!(spec.contains("\"acceptanceMd\""));
        assert!(spec.contains("你是 AOS Code Studio 的 Plan Mode"));

        let design = plan_generate_design_prompt("需求", "规格", "验收", "上下文");
        assert!(design.contains("\"designMd\""));

        let tasks = plan_generate_tasks_prompt("需求", "规格", "设计", "验收", "上下文");
        assert!(tasks.contains("\"tasksMd\""));
        assert!(tasks.contains("\"taskItems\""));

        let report = plan_final_report_prompt("标题", "规格", "设计", "任务", "{}");
        assert!(report.contains("\"finalReportMd\""));
    }
}
