//! Architecture and review side-pass agents for RD coding tasks.

use super::*;

pub(super) async fn maybe_run_rd_architecture_pass(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    original_prompt: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    context_profile: RdContextProfile,
) -> Result<Option<String>, AppError> {
    let Some(repo_id) = repository_id else {
        return Ok(None);
    };
    if !rd_runtime_executor_enabled() || !rd_architecture_pass_enabled() {
        return Ok(None);
    }
    if !context_profile.should_run_architecture_pass() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "architecture_agent",
            "skipped",
            "当前任务采用轻量/定向上下文策略，跳过额外 Architecture Agent 预分析",
            json!({
                "repositoryId": repo_id,
                "mode": mode,
                "contextProfile": context_profile.as_str(),
            }),
        )
        .await?;
        return Ok(None);
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "architecture_agent",
        "running",
        "Architecture Agent 正在理解仓库结构与相关文件",
        json!({
            "repositoryId": repo_id,
            "mode": mode,
            "contextProfile": context_profile.as_str(),
        }),
    )
    .await?;
    let repo_instructions = load_repository_instructions_for_task(
        state,
        claims,
        task_id,
        Some(repo_id),
        "architecture_agent",
    )
    .await?;
    let repo_instruction_section = if repo_instructions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n仓库内开发规范（必须优先遵守）：\n{}",
            repo_instructions.text.trim()
        )
    };

    let architecture_prompt = format!(
        "你是 AOS Code Studio 的 Architecture Agent。你的任务是先理解真实仓库，不要改代码，不要生成 Diff。\n\n当前任务模式：{}\n\n用户需求：\n{}{}\n\n请使用仓库搜索/读取工具找出：相关文件、模块边界、数据流/调用链、潜在风险、建议验证命令和主 Coding Agent 应重点注意的约束。\n\n输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":null,\"prDescription\":null,\"unifiedDiff\":null,\"touchedFiles\":array}}。",
        mode, original_prompt, repo_instruction_section,
    );

    match run_rd_runtime_completion(
        state,
        claims,
        task_id,
        "ask",
        Some(repo_id),
        model,
        None,
        RdRuntimeSessionPolicy::Transient,
        None,
        architecture_prompt,
    )
    .await
    {
        Ok(completion) => {
            let parsed = parse_rd_output(&completion.text, "ask");
            let context = format!(
                "## Architecture Agent\n\n### 计划\n{}\n\n### 仓库理解\n{}",
                parsed.plan_md, parsed.answer_md
            );
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "architecture_agent",
                "completed",
                "Architecture Agent 仓库理解完成",
                json!({
                    "model": completion.model,
                    "provider": completion.provider,
                    "contextChars": context.chars().count(),
                    "touchedFiles": parsed.touched_files,
                }),
            )
            .await?;
            Ok(Some(context))
        }
        Err(error) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "architecture_agent",
                "failed",
                "Architecture Agent 预分析失败，已继续主 Coding Agent",
                json!({"error": error.to_string(), "nonBlocking": true}),
            )
            .await?;
            Ok(None)
        }
    }
}

pub(super) async fn maybe_run_rd_reviewer_pass(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    original_prompt: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    parsed: &mut ParsedRdOutput,
) -> Result<(), AppError> {
    let Some(diff) = parsed
        .unified_diff
        .clone()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    if mode != "modify" || !rd_runtime_executor_enabled() || !rd_reviewer_pass_enabled() {
        return Ok(());
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "review_agent",
        "running",
        "独立 Review Agent 正在审查 Diff",
        json!({
            "repositoryId": repository_id,
            "diffChars": diff.chars().count(),
        }),
    )
    .await?;
    let repo_instruction_context = load_repository_instructions_for_task(
        state,
        claims,
        task_id,
        repository_id,
        "review_agent",
    )
    .await?;
    let repo_instruction_section = if repo_instruction_context.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n仓库内开发规范（审查时必须纳入判断）：\n{}",
            repo_instruction_context.text.trim()
        )
    };

    let review_prompt = format!(
        "你是 AOS Code Studio 的独立 Review Agent。请基于真实仓库文件、用户原始需求和主 Agent 生成的 Diff 做审查，实事求是，不要为了反驳而反驳。\n\n用户原始需求：\n{}{}\n\n主 Agent 计划：\n{}\n\n主 Agent 总结：\n{}\n\n待审查 Diff：\n{}\n\n请输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":null,\"touchedFiles\":array}}。\nreviewMd 必须使用清晰 Markdown 分段，禁止输出一整段密集文字，禁止使用顶级 # 标题。推荐结构：### 总体判断、### Findings、### 风险与缺失测试、### 建议。必须优先列出发现的问题、严重级别、文件路径、风险、缺失测试和是否建议应用；如果没有明显问题，也要说明 residual risk。",
        original_prompt,
        repo_instruction_section,
        parsed.plan_md,
        parsed.answer_md,
        diff,
    );

    match run_rd_runtime_completion(
        state,
        claims,
        task_id,
        "review",
        repository_id,
        model,
        None,
        RdRuntimeSessionPolicy::Transient,
        None,
        review_prompt,
    )
    .await
    {
        Ok(completion) => {
            let review = parse_rd_output(&completion.text, "review");
            let review_text = review.review_md.clone().unwrap_or_else(|| {
                if review.answer_md.trim().is_empty() {
                    "Review Agent 未返回审查正文。".to_string()
                } else {
                    review.answer_md.clone()
                }
            });
            let review_quality = analyze_review_quality(
                &review_text,
                &diff,
                &parsed.touched_files,
                &review.touched_files,
            );
            parsed.review_md = Some(match parsed.review_md.take() {
                Some(existing) if !existing.trim().is_empty() => {
                    format!("{existing}\n\n## 独立 Review Agent\n\n{review_text}")
                }
                _ => format!("## 独立 Review Agent\n\n{review_text}"),
            });
            if parsed.pr_title.is_none() {
                parsed.pr_title = review.pr_title;
            }
            if parsed.pr_description.is_none() {
                parsed.pr_description = review.pr_description;
            }
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "review_agent",
                "completed",
                "独立 Review Agent 审查完成",
                json!({
                    "model": completion.model,
                    "provider": completion.provider,
                    "reviewChars": review_text.chars().count(),
                    "findingsCount": review_quality.findings_count,
                    "fileRefCount": review_quality.file_ref_count,
                    "lineRefCount": review_quality.line_ref_count,
                }),
            )
            .await?;
            if let Some(repo_id) = repository_id {
                record_review_quality_metrics(
                    &state.db,
                    &claims.tenant_id,
                    repo_id,
                    task_id,
                    &review_quality,
                )
                .await?;
            }
        }
        Err(error) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "review_agent",
                "failed",
                "独立 Review Agent 审查失败，已保留主 Agent 结果",
                json!({"error": error.to_string(), "nonBlocking": true}),
            )
            .await?;
        }
    }

    Ok(())
}
