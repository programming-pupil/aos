//! Diff validation and repair flow for RD generated patches.

use super::*;

pub(super) async fn validate_generated_diff(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: Option<&str>,
    change_id: &str,
    diff: &str,
    source_stage: &str,
) -> Result<GeneratedDiffCheckOutcome, AppError> {
    let Some(repo_id) = repository_id else {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff_check",
            "skipped",
            "任务未绑定仓库，已跳过 Diff 可应用性校验",
            json!({
                "changeId": change_id,
                "sourceStage": source_stage,
                "reason": "repository_missing",
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            None,
            Some(task_id),
            "diff_check_skipped",
            1.0,
            json!({
                "changeId": change_id,
                "sourceStage": source_stage,
                "reason": "repository_missing",
            }),
        )
        .await?;
        return Ok(GeneratedDiffCheckOutcome {
            status: GeneratedDiffCheckStatus::Skipped,
            error_message: None,
        });
    };

    let root = match repository_root(state, claims, repo_id).await {
        Ok(root) => root,
        Err(error) => {
            let error_message = error.to_string();
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_check",
                "failed",
                "仓库工作目录不可用，无法确认 Diff 是否可应用",
                json!({
                    "changeId": change_id,
                    "repositoryId": repo_id,
                    "sourceStage": source_stage,
                    "error": truncate_text(&error_message, 2_000),
                }),
            )
            .await?;
            record_quality_metric(
                &state.db,
                &claims.tenant_id,
                Some(repo_id),
                Some(task_id),
                "diff_check_failed",
                1.0,
                json!({
                    "changeId": change_id,
                    "sourceStage": source_stage,
                    "reason": "repository_unavailable",
                    "error": truncate_text(&error_message, 2_000),
                }),
            )
            .await?;
            append_task_review_note(
                &state.db,
                &claims.tenant_id,
                task_id,
                "Diff 校验警告",
                &format!(
                    "系统未能定位仓库工作目录，因此无法确认变更 `{change_id}` 是否可被 `git apply` 应用。建议先重新同步仓库，再重新生成或应用 Diff。错误：{}",
                    truncate_text(&error_message, 1_000)
                ),
            )
            .await?;
            return Ok(GeneratedDiffCheckOutcome {
                status: GeneratedDiffCheckStatus::Failed,
                error_message: Some(error_message),
            });
        }
    };

    match git_apply(&root, diff, true).await {
        Ok(()) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_check",
                "completed",
                "Diff 可应用性校验通过",
                json!({
                    "changeId": change_id,
                    "repositoryId": repo_id,
                    "sourceStage": source_stage,
                    "checkOnly": true,
                    "status": GeneratedDiffCheckStatus::Passed.as_str(),
                }),
            )
            .await?;
            record_quality_metric(
                &state.db,
                &claims.tenant_id,
                Some(repo_id),
                Some(task_id),
                "diff_check_passed",
                1.0,
                json!({
                    "changeId": change_id,
                    "sourceStage": source_stage,
                }),
            )
            .await?;
            Ok(GeneratedDiffCheckOutcome {
                status: GeneratedDiffCheckStatus::Passed,
                error_message: None,
            })
        }
        Err(error) => {
            let error_message = error.to_string();
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_check",
                "failed",
                "Diff 可应用性校验失败，补丁可能无法直接应用",
                json!({
                    "changeId": change_id,
                    "repositoryId": repo_id,
                    "sourceStage": source_stage,
                    "checkOnly": true,
                    "status": GeneratedDiffCheckStatus::Failed.as_str(),
                    "error": truncate_text(&error_message, 4_000),
                }),
            )
            .await?;
            record_quality_metric(
                &state.db,
                &claims.tenant_id,
                Some(repo_id),
                Some(task_id),
                "diff_check_failed",
                1.0,
                json!({
                    "changeId": change_id,
                    "sourceStage": source_stage,
                    "error": truncate_text(&error_message, 2_000),
                }),
            )
            .await?;
            append_task_review_note(
                &state.db,
                &claims.tenant_id,
                task_id,
                "Diff 校验警告",
                &format!(
                    "系统已对变更 `{change_id}` 执行 `git apply --check`，结果未通过。请优先让 Agent 重新生成 Diff 或人工检查补丁上下文，避免直接应用失败。错误：{}",
                    truncate_text(&error_message, 1_000)
                ),
            )
            .await?;
            Ok(GeneratedDiffCheckOutcome {
                status: GeneratedDiffCheckStatus::Failed,
                error_message: Some(error_message),
            })
        }
    }
}

async fn append_task_review_note(
    db: &SqlitePool,
    tenant_id: &str,
    task_id: &str,
    heading: &str,
    body: &str,
) -> Result<(), AppError> {
    let note = format!("\n\n## {heading}\n\n{body}");
    sqlx::query("UPDATE rd_tasks SET review_md = (COALESCE(review_md, '') || ?) WHERE id = ? AND tenant_id = ?")
        .bind(note)
        .bind(task_id)
        .bind(tenant_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn mark_rd_file_change_type(
    db: &SqlitePool,
    tenant_id: &str,
    change_id: &str,
    change_type: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE rd_file_changes SET change_type = ? WHERE id = ? AND tenant_id = ?")
        .bind(change_type)
        .bind(change_id)
        .bind(tenant_id)
        .execute(db)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn maybe_repair_invalid_generated_diff(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    original_prompt: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    failed_change_id: &str,
    failed_diff: &str,
    failed_error: Option<&str>,
    previous_plan: &str,
    previous_answer: &str,
    source_stage: &str,
) -> Result<Option<String>, AppError> {
    let Some(repo_id) = repository_id else {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff_repair",
            "skipped",
            "任务未绑定仓库，已跳过 Diff 自动修复",
            json!({
                "failedChangeId": failed_change_id,
                "sourceStage": source_stage,
                "reason": "repository_missing",
            }),
        )
        .await?;
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            failed_change_id,
            "invalid_diff",
        )
        .await?;
        return Ok(None);
    };
    if !rd_runtime_executor_enabled() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff_repair",
            "skipped",
            "研发 runtime 未启用，已跳过 Diff 自动修复",
            json!({
                "failedChangeId": failed_change_id,
                "repositoryId": repo_id,
                "sourceStage": source_stage,
                "reason": "runtime_disabled",
            }),
        )
        .await?;
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            failed_change_id,
            "invalid_diff",
        )
        .await?;
        return Ok(None);
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "diff_repair",
        "running",
        "Patch Repair Agent 正在根据 git apply 校验错误重新生成 Diff",
        json!({
            "failedChangeId": failed_change_id,
            "repositoryId": repo_id,
            "sourceStage": source_stage,
            "mode": mode,
        }),
    )
    .await?;
    run_rd_hook(
        state,
        claims,
        HookEventType::BeforeModelCall,
        "rd.diff_repair.model",
        json!({
            "taskId": task_id,
            "repositoryId": repo_id,
            "failedChangeId": failed_change_id,
            "sourceStage": source_stage,
            "failedDiffChars": failed_diff.chars().count(),
        }),
        None,
        false,
        true,
    )
    .await?;
    let repo_instruction_context =
        load_repository_instructions_for_task(state, claims, task_id, Some(repo_id), "diff_repair")
            .await?;
    let repo_instruction_section = if repo_instruction_context.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n# 仓库内开发规范\n{}",
            repo_instruction_context.text.trim()
        )
    };

    let repair_prompt = format!(
        "你是 AOS Code Studio 的 Patch Repair Agent。上一版 Diff 没有通过 `git apply --check`，请使用真实仓库工具重新读取相关文件并生成一版新的、完整的、可 `git apply` 的 unified diff。\n\n约束：\n- 不要直接修改文件，不要运行写入命令，不要提交代码。\n- 不要只解释原因，必须尽力输出新的 unifiedDiff。\n- 如果原需求本身无法生成安全 Diff，请明确说明原因，并让 unifiedDiff 为 null。\n- 输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":string|null,\"touchedFiles\":array}}。\n\n# 原始任务\n{}{}\n\n# 上一版计划\n{}\n\n# 上一版总结\n{}\n\n# 失败的 change_id\n{}\n\n# git apply --check 错误\n{}\n\n# 失败 Diff\n{}",
        truncate_text(original_prompt, 8_000),
        repo_instruction_section,
        truncate_text(previous_plan, 6_000),
        truncate_text(previous_answer, 6_000),
        failed_change_id,
        truncate_text(failed_error.unwrap_or("unknown git apply error"), 4_000),
        truncate_text(failed_diff, 24_000),
    );

    let completion = match run_rd_runtime_completion(
        state,
        claims,
        task_id,
        "modify",
        Some(repo_id),
        model,
        None,
        RdRuntimeSessionPolicy::Transient,
        None,
        repair_prompt,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_repair",
                "failed",
                "Patch Repair Agent 执行失败，已保留原始 Diff 和校验错误",
                json!({
                    "failedChangeId": failed_change_id,
                    "repositoryId": repo_id,
                    "sourceStage": source_stage,
                    "error": error.to_string(),
                }),
            )
            .await?;
            return Ok(None);
        }
    };

    let mut parsed = parse_rd_output(&completion.text, "modify");
    if parsed
        .unified_diff
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        maybe_run_rd_reviewer_pass(
            state,
            claims,
            task_id,
            "modify",
            original_prompt,
            Some(repo_id),
            Some(completion.model.as_str()),
            &mut parsed,
        )
        .await?;
    }
    let Some(repaired_diff) = parsed
        .unified_diff
        .clone()
        .filter(|value| !value.trim().is_empty())
    else {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff_repair",
            "completed",
            "Patch Repair Agent 未生成新的 Diff，已保留原始校验警告",
            json!({
                "failedChangeId": failed_change_id,
                "repositoryId": repo_id,
                "sourceStage": source_stage,
                "model": completion.model,
                "provider": completion.provider,
            }),
        )
        .await?;
        run_rd_hook(
            state,
            claims,
            HookEventType::AfterModelCall,
            "rd.diff_repair.model",
            json!({
                "taskId": task_id,
                "repositoryId": repo_id,
                "failedChangeId": failed_change_id,
                "sourceStage": source_stage,
            }),
            Some(json!({"status": "no_diff"})),
            false,
            false,
        )
        .await?;
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            failed_change_id,
            "invalid_diff",
        )
        .await?;
        return Ok(None);
    };

    let file_path = parsed.touched_files.first().cloned().unwrap_or_else(|| {
        infer_first_file_from_diff(&repaired_diff).unwrap_or_else(|| "repair.diff".to_string())
    });
    let repaired_change_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO rd_file_changes (id, task_id, tenant_id, repository_id, file_path, change_type, diff_patch) VALUES (?, ?, ?, ?, ?, 'repair_diff', ?)")
        .bind(&repaired_change_id)
        .bind(task_id)
        .bind(&claims.tenant_id)
        .bind(repo_id)
        .bind(file_path)
        .bind(&repaired_diff)
        .execute(&state.db)
        .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "diff",
        "waiting_approval",
        "Patch Repair Agent 已生成新的待确认 Diff",
        json!({
            "failedChangeId": failed_change_id,
            "repairedChangeId": repaired_change_id,
            "repositoryId": repo_id,
            "sourceStage": source_stage,
        }),
    )
    .await?;
    let repaired_check = validate_generated_diff(
        state,
        claims,
        task_id,
        Some(repo_id),
        &repaired_change_id,
        &repaired_diff,
        "diff_repair",
    )
    .await?;
    if repaired_check.status == GeneratedDiffCheckStatus::Passed {
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            failed_change_id,
            "superseded_invalid",
        )
        .await?;
    } else {
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            failed_change_id,
            "invalid_diff",
        )
        .await?;
        mark_rd_file_change_type(
            &state.db,
            &claims.tenant_id,
            &repaired_change_id,
            "invalid_repair_diff",
        )
        .await?;
    }

    let repair_summary = format!(
        "\n\n## Patch Repair Agent\n\n### 计划\n{}\n\n### 结果\n{}\n\n- 原 Diff: `{}`\n- 修复 Diff: `{}`\n- 修复 Diff 校验状态: `{}`",
        parsed.plan_md,
        parsed.answer_md,
        failed_change_id,
        repaired_change_id,
        repaired_check.status.as_str(),
    );
    sqlx::query("UPDATE rd_tasks SET status = 'waiting_approval', answer_md = (COALESCE(answer_md, '') || ?), model = ? WHERE id = ? AND tenant_id = ?")
        .bind(repair_summary)
        .bind(&completion.model)
        .bind(task_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(review) = parsed
        .review_md
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        append_task_review_note(
            &state.db,
            &claims.tenant_id,
            task_id,
            "Patch Repair Review Agent",
            review,
        )
        .await?;
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "diff_repair",
        if repaired_check.status == GeneratedDiffCheckStatus::Passed {
            "completed"
        } else {
            "failed"
        },
        if repaired_check.status == GeneratedDiffCheckStatus::Passed {
            "Patch Repair Agent 已生成并校验通过新的 Diff"
        } else {
            "Patch Repair Agent 已生成新的 Diff，但仍未通过可应用性校验"
        },
        json!({
            "failedChangeId": failed_change_id,
            "repairedChangeId": repaired_change_id,
            "repositoryId": repo_id,
            "sourceStage": source_stage,
            "checkStatus": repaired_check.status.as_str(),
            "model": completion.model,
            "provider": completion.provider,
        }),
    )
    .await?;
    record_rd_task_risk_map(
        &state.db,
        &claims.tenant_id,
        task_id,
        mode,
        "waiting_approval",
        &parsed,
        Some(&repaired_diff),
        Some(&repaired_check),
        "diff_repair",
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repo_id),
        Some(task_id),
        if repaired_check.status == GeneratedDiffCheckStatus::Passed {
            "diff_repair_passed"
        } else {
            "diff_repair_failed"
        },
        1.0,
        json!({
            "failedChangeId": failed_change_id,
            "repairedChangeId": repaired_change_id,
            "sourceStage": source_stage,
            "checkStatus": repaired_check.status.as_str(),
        }),
    )
    .await?;
    run_rd_hook(
        state,
        claims,
        HookEventType::AfterModelCall,
        "rd.diff_repair.model",
        json!({
            "taskId": task_id,
            "repositoryId": repo_id,
            "failedChangeId": failed_change_id,
            "sourceStage": source_stage,
        }),
        Some(json!({
            "status": "waiting_approval",
            "repairedChangeId": repaired_change_id,
            "checkStatus": repaired_check.status.as_str(),
        })),
        false,
        false,
    )
    .await?;

    Ok(Some(repaired_change_id))
}
