//! Repository context construction, retrieval merging, and cache observability.

use super::*;

mod dependency_graph;
mod evidence;
mod observability;
mod retrieval;
mod task_memory;

pub(in crate::routes::rd) use dependency_graph::rd_normalize_repo_relative_path;
use observability::maybe_record_rd_retrieval_evidence;
use retrieval::build_repository_retrieval_context;

pub(in crate::routes::rd) async fn build_repository_context_for_prompt(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    prompt: &str,
    budget_bytes: usize,
    context_profile: RdContextProfile,
    task_id: Option<&str>,
) -> Result<String, AppError> {
    let root = repository_root(state, claims, repository_id).await?;
    let context_budget = context_profile.budget();
    let mut builder = RdContextBuilder::new(budget_bytes.min(MAX_CONTEXT_BYTES));
    for name in [
        "README.md",
        "README",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
    ] {
        let path = root.join(name);
        if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                builder.push_section(name, &text, context_budget.manifest_section_bytes);
            }
        }
    }

    let cached_context =
        load_repository_cached_context_summaries(state, claims, repository_id, context_profile)
            .await?;
    if !cached_context.trim().is_empty() {
        builder.push_section(
            "缓存仓库/目录摘要",
            &cached_context,
            match context_profile {
                RdContextProfile::Overview => 12_000,
                RdContextProfile::FocusedAsk | RdContextProfile::Explain => 8_000,
                _ => 6_000,
            },
        );
    }

    let retrieval_context =
        build_repository_retrieval_context(state, claims, repository_id, prompt, context_profile)
            .await?;
    maybe_record_rd_retrieval_evidence(
        state,
        claims,
        task_id,
        repository_id,
        context_profile,
        &retrieval_context,
    )
    .await;
    if !retrieval_context.text.trim().is_empty() {
        builder.push_section(
            "任务相关候选文件（索引召回）",
            &retrieval_context.text,
            context_budget.retrieval_bytes,
        );
    }

    let tree = collect_flat_tree(&root, context_budget.tree_item_limit).join("\n");
    builder.push_section("文件概览", &tree, 10_000);
    Ok(builder.finish())
}

pub(in crate::routes::rd) async fn build_repository_runtime_context_hint(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    prompt: &str,
    context_profile: RdContextProfile,
    task_id: Option<&str>,
) -> Result<String, AppError> {
    let context_budget = context_profile.budget();
    let retrieval_context =
        build_repository_retrieval_context(state, claims, repository_id, prompt, context_profile)
            .await?;
    maybe_record_rd_retrieval_evidence(
        state,
        claims,
        task_id,
        repository_id,
        context_profile,
        &retrieval_context,
    )
    .await;
    let mut builder = RdContextBuilder::new(context_budget.runtime_hint_bytes);
    builder.push_section(
        "Runtime 仓库工作区",
        &format!(
            "仓库已同步并作为 runtime 工作目录。当前上下文 profile：{}。请像 CLI 编程助手一样使用 glob_search/grep_search/read_file 等工具按需读取真实文件；优先从下面的索引召回候选开始，但不要把摘要当作代码事实，关键判断必须读取真实文件核对。",
            context_profile.display_name()
        ),
        2_000,
    );
    let cached_context =
        load_repository_cached_context_summaries(state, claims, repository_id, context_profile)
            .await?;
    if !cached_context.trim().is_empty() {
        builder.push_section(
            "缓存仓库/目录摘要",
            &cached_context,
            match context_profile {
                RdContextProfile::Overview => 8_000,
                RdContextProfile::FocusedAsk | RdContextProfile::Explain => 5_000,
                _ => 4_000,
            },
        );
    }
    if context_profile == RdContextProfile::Overview {
        let root = repository_root(state, claims, repository_id).await?;
        let overview_context = build_repository_overview_bootstrap_context(&root, context_budget)?;
        if !overview_context.trim().is_empty() {
            builder.push_section("概览优先上下文", &overview_context, 7_000);
        }
    }
    if !retrieval_context.text.trim().is_empty() {
        builder.push_section(
            "任务相关候选文件（索引召回）",
            &retrieval_context.text,
            context_budget.retrieval_bytes,
        );
    }
    Ok(builder.finish())
}

async fn load_repository_cached_context_summaries(
    state: &AppState,
    claims: &Claims,
    repository_id: &str,
    context_profile: RdContextProfile,
) -> Result<String, AppError> {
    let scope_limit = match context_profile {
        RdContextProfile::Overview => 10,
        RdContextProfile::FocusedAsk | RdContextProfile::Explain => 7,
        RdContextProfile::Modify => 6,
        RdContextProfile::Review | RdContextProfile::DeepReview => 8,
    };
    let rows = match sqlx::query(
        "SELECT scope_type, scope_key,
                COALESCE(NULLIF(llm_summary_text, ''), summary_text) AS effective_summary_text,
                llm_model,
                CAST(detail_json AS TEXT) detail_json
         FROM rd_repository_context_summaries
         WHERE tenant_id = ? AND repository_id = ?
         ORDER BY
           CASE scope_type
             WHEN 'repository' THEN 0
             WHEN 'entrypoints' THEN 1
             WHEN 'directory' THEN 2
             ELSE 9
           END,
           updated_at DESC
         LIMIT ?",
    )
    .bind(&claims.tenant_id)
    .bind(repository_id)
    .bind(i64::try_from(scope_limit).unwrap_or(i64::MAX))
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                repository_id = %repository_id,
                error = %error,
                "failed to load RD repository context summaries; continuing without cache"
            );
            return Ok(String::new());
        }
    };

    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut lines = vec![
        "以下为同步仓库时生成的确定性上下文摘要，用于减少盲目扫描；它不是代码事实，关键判断仍需读取真实文件。".to_string(),
    ];
    for row in rows {
        let scope_type: String = row.get("scope_type");
        let scope_key: String = row.get("scope_key");
        let summary_text: String = row.get("effective_summary_text");
        let llm_model: Option<String> = row.get("llm_model");
        lines.push(format!(
            "## {} `{}`{}\n{}",
            scope_type,
            scope_key,
            llm_model
                .as_deref()
                .map(|model| format!("（LLM 精炼：{model}）"))
                .unwrap_or_default(),
            truncate_text(&summary_text, 2_600)
        ));
    }
    Ok(truncate_text(&lines.join("\n\n"), 14_000))
}

fn build_repository_overview_bootstrap_context(
    root: &Path,
    budget: RdContextBudget,
) -> Result<String, AppError> {
    let mut builder = RdContextBuilder::new(budget.manifest_section_bytes);
    for name in [
        "README.md",
        "README",
        "package.json",
        "pnpm-workspace.yaml",
        "yarn.lock",
        "Cargo.toml",
        "Cargo.lock",
        "docker-compose.yml",
        "compose.yml",
        "Dockerfile",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
    ] {
        let path = root.join(name);
        if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                builder.push_section(name, &text, 3_000);
            }
        }
    }
    let tree = collect_flat_tree(root, budget.tree_item_limit).join("\n");
    builder.push_section("轻量文件树", &tree, 4_000);
    Ok(builder.finish())
}
