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

pub(super) fn plan_revise_stage_prompt(
    stage: &str,
    output_schema: &str,
    requirement: &str,
    current_document: &str,
    feedback: &str,
    repo_context: &str,
) -> String {
    let (output_contract, schema_hint) = if stage.eq_ignore_ascii_case("design") {
        (
            "Return the complete revised design as Markdown only. Do not wrap it in JSON or a Markdown code fence.",
            "",
        )
    } else {
        ("Return JSON only using this schema:\n", output_schema)
    };
    format!(
        "You are the AOS Code Studio Spec Mode revision agent.\n\n\
Revise the current {stage} document according to the user's feedback. Preserve correct content, \
resolve every actionable feedback item, and keep the result grounded in real repository evidence. \
Do not implement code. {output_contract}{schema_hint}\n\n\
Original requirement:\n{requirement}\n\nCurrent document:\n{current_document}\n\n\
User feedback:\n{feedback}\n\nRepository context:\n{repo_context}"
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
        assert!(design.contains("Markdown"));

        let tasks = plan_generate_tasks_prompt("需求", "规格", "设计", "验收", "上下文");
        assert!(tasks.contains("\"tasksMd\""));
        assert!(tasks.contains("\"taskItems\""));

        let revision = plan_revise_stage_prompt(
            "design",
            r#"{"designMd": string}"#,
            "需求",
            "当前设计",
            "补充回滚方案",
            "仓库上下文",
        );
        assert!(revision.contains("补充回滚方案"));
        assert!(revision.contains("Markdown only"));

        let report = plan_final_report_prompt("标题", "规格", "设计", "任务", "{}");
        assert!(report.contains("\"finalReportMd\""));
    }
}
