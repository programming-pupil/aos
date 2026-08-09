//! Context planning, retrieval, and prompt assembly for RD code tasks.

use super::steering::RdSteeringContext;
use super::*;

mod intent;
pub(super) use intent::route_rd_task_intent;
pub(super) use rd_core::contains_any;
mod instructions;
pub(super) use instructions::{
    load_prompt_file_context_for_task, load_repository_instructions_for_task,
};
mod planner;
pub(super) use planner::{
    build_rd_context_plan_section, build_rd_context_policy_section,
    build_rd_llm_context_plan_section, default_rd_context_depth, maybe_run_rd_llm_context_planner,
    normalize_rd_context_depth, normalize_rd_profile_for_mode, rd_context_budget_json,
    resolve_rd_task_context_strategy,
};
mod repository;
pub(super) use repository::{
    build_repository_context, build_repository_context_for_prompt,
    build_repository_runtime_context_hint, rd_normalize_repo_relative_path,
};
mod semantic;
pub(super) use semantic::{
    rd_embed_texts_with_candidate, record_rd_embedding_usage, resolve_rd_embedding_candidates,
};
use semantic::{rd_semantic_hit_metadata_hint, rd_semantic_repository_search};
pub(super) fn build_rd_system_prompt(
    mode: &str,
    agent_profile: Option<&RdAgentProfileDto>,
    steering: &RdSteeringContext,
    repo_instructions: &RdRepositoryInstructionContext,
) -> String {
    let mut parts = vec![rd_system_prompt(mode).to_string()];
    if let Some(profile) = agent_profile {
        parts.push(format!(
            "## 当前 Coding Agent\n名称：{}\n角色要求：\n{}",
            profile.name,
            profile.role_prompt.trim()
        ));
        if let Some(tools) = &profile.allowed_tools {
            parts.push(format!(
                "## Agent 工具边界\nallowedTools 配置如下；如果当前执行器尚未暴露对应工具，也必须在计划中说明限制，不得假装已经执行：\n{}",
                tools
            ));
        }
    }
    if !steering.text.trim().is_empty() {
        parts.push(format!(
            "## 团队 Steering 规范\n这些规范优先级高于普通建议，除非用户明确要求且不违反安全约束：\n{}",
            steering.text.trim()
        ));
    }
    if !repo_instructions.is_empty() {
        parts.push(format!(
            "## 仓库内开发规范\n以下内容自动发现自仓库文件（{}）。它们代表项目本身的编码、测试、架构和协作约定；除非与安全边界冲突，否则必须优先遵守：\n{}",
            repo_instructions.files.join(", "),
            repo_instructions.text.trim()
        ));
    }
    parts.push(
        "## 安全边界\n默认只生成计划、解释、审查意见和 unified diff；不要声称已经写入文件、运行命令、提交代码或创建 PR。".to_string(),
    );
    parts.join("\n\n")
}
fn extract_repository_retrieval_terms(prompt: &str, limit: usize) -> Vec<String> {
    let stop_words = [
        "the", "and", "for", "with", "this", "that", "from", "into", "检查", "项目", "代码",
        "问题", "修复", "实现", "功能", "这个", "那个", "一下", "所有", "全部",
    ];
    let mut terms = BTreeSet::new();
    for raw in prompt.split(|ch: char| {
        !(ch.is_ascii_alphanumeric()
            || ch == '_'
            || ch == '-'
            || ('\u{4e00}'..='\u{9fff}').contains(&ch))
    }) {
        let term = raw.trim().trim_matches('-').trim_matches('_');
        if term.chars().count() < 2 || term.chars().count() > 48 {
            continue;
        }
        if stop_words
            .iter()
            .any(|stop| term.eq_ignore_ascii_case(stop))
        {
            continue;
        }
        terms.insert(term.to_string());
        if terms.len() >= limit {
            break;
        }
    }
    terms.into_iter().collect()
}

pub(super) fn should_run_rd_repository_prescan(
    context_profile: RdContextProfile,
    mode: &str,
    prompt: &str,
) -> bool {
    if !matches!(
        context_profile,
        RdContextProfile::Review | RdContextProfile::DeepReview
    ) {
        return false;
    }
    let lower = prompt.to_ascii_lowercase();
    context_profile == RdContextProfile::DeepReview
        || mode == "review"
        || [
            "audit",
            "security",
            "review",
            "全量",
            "全部",
            "所有",
            "整个项目",
            "检查项目",
            "找出问题",
            "安全",
            "审查",
            "风险",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

pub(super) async fn build_repository_prescan_context(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
) -> Result<String, AppError> {
    let root = repository_root(state, claims, repository_id).await?;
    let mut lines =
        vec!["本地预扫描不会代替 LLM 审查，只提供高价值入口，减少盲目全仓扫描。".to_string()];

    let mut candidate_paths = BTreeSet::new();
    for term in [
        "auth",
        "security",
        "login",
        "permission",
        "config",
        "route",
        "controller",
        "service",
        "sql",
        "migration",
        "test",
    ] {
        let like = format!("%{term}%");
        let rows = sqlx::query(
            "SELECT file_path, summary_text \
             FROM rd_repository_file_summaries \
             WHERE tenant_id = ? AND repository_id = ? \
               AND (file_path LIKE ? OR summary_text LIKE ?) \
             ORDER BY updated_at DESC LIMIT 5",
        )
        .bind(&claims.tenant_id)
        .bind(repository_id)
        .bind(&like)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;
        for row in rows {
            let path: String = row.get("file_path");
            let summary: String = row.get("summary_text");
            if candidate_paths.insert(path.clone()) {
                lines.push(format!(
                    "- 候选文件 `{path}`：{}",
                    truncate_text(&summary, 220)
                ));
            }
        }
    }

    let risk_pattern = r"TODO|FIXME|panic!|unwrap\(|expect\(|eval\(|innerHTML|password|secret|api[_-]?key|SELECT \*|unsafe";
    if let Some(hits) = run_rg_repository_search(&root, risk_pattern, 40).await? {
        if !hits.is_empty() {
            lines.push("潜在风险信号（rg 采样，需读取真实文件确认）：".to_string());
            for hit in hits.into_iter().take(30) {
                lines.push(format!(
                    "- `{}`:{} {}",
                    hit.path,
                    hit.line_number,
                    redact_sensitive_snippet(&hit.snippet)
                ));
            }
        }
    }

    Ok(truncate_text(&lines.join("\n"), 7_000))
}

fn redact_sensitive_snippet(snippet: &str) -> String {
    let lower = snippet.to_ascii_lowercase();
    if [
        "password",
        "secret",
        "api_key",
        "apikey",
        "token",
        "access_key",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        truncate_text(
            &snippet
                .split_whitespace()
                .map(|part| {
                    let lower = part.to_ascii_lowercase();
                    if lower.contains("password")
                        || lower.contains("secret")
                        || lower.contains("token")
                        || lower.contains("key")
                    {
                        "[REDACTED]"
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            240,
        )
    } else {
        truncate_text(snippet, 240)
    }
}
