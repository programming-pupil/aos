//! Runtime completion and candidate worktree execution for RD tasks.

use super::*;

pub(super) async fn run_rd_runtime_completion(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    session_policy: RdRuntimeSessionPolicy,
    context_strategy: Option<RdTaskContextStrategy>,
    prompt: String,
) -> Result<RdCompletionResult, AppError> {
    let manager = state.agent_manager().clone();
    let tool_policy = resolve_rd_runtime_tool_policy(mode, allowed_tools);
    let governance_plan = build_rd_runtime_tool_governance_plan(mode, context_strategy.as_ref());
    let governance_plan_json = governance_plan.to_json();
    let governance_prompt_section = governance_plan.to_prompt_section();
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "runtime",
        "running",
        "正在使用研发 runtime 读取代码与生成结果",
        json!({
            "executor": "agent_gateway",
            "source": session_policy.source(),
            "sessionPolicy": if session_policy.is_persistent() { "thread_persistent" } else { "transient" },
            "repositoryId": repository_id,
            "mode": mode,
            "toolGovernancePlan": governance_plan_json.clone(),
            "allowedTools": tool_policy.allowed_tools.clone(),
            "toolPolicySource": tool_policy.source,
            "bashEnabled": tool_policy.bash_enabled,
            "filteredBash": tool_policy.filtered_bash,
            "writeToolsEnabled": tool_policy.write_tools_enabled,
            "filteredWriteTools": tool_policy.filtered_write_tools.clone(),
        }),
    )
    .await?;

    let session_start = start_rd_runtime_session(
        state,
        claims,
        task_id,
        repository_id,
        model,
        Some(tool_policy.allowed_tools.clone()),
        session_policy,
    )
    .await?;
    let session_id = session_start.handle.session_id.clone();
    let runtime_prompt = build_rd_runtime_user_prompt(
        mode,
        repository_id,
        &prompt,
        Some(&governance_prompt_section),
    );
    let turn_result = run_rd_runtime_turn_with_timeout_cleanup(
        state,
        manager.clone(),
        session_id.clone(),
        &claims.tenant_id,
        task_id,
        runtime_prompt,
        true,
        Some(governance_plan.clone()),
        json!({
            "mode": mode,
            "repositoryId": repository_id,
            "sessionPolicy": if session_policy.is_persistent() { "thread_persistent" } else { "transient" },
            "source": session_policy.source(),
            "toolGovernancePlan": governance_plan_json.clone(),
            "allowedTools": tool_policy.allowed_tools.clone(),
            "toolPolicySource": tool_policy.source,
            "bashEnabled": tool_policy.bash_enabled,
            "filteredBash": tool_policy.filtered_bash,
            "writeToolsEnabled": tool_policy.write_tools_enabled,
            "filteredWriteTools": tool_policy.filtered_write_tools.clone(),
        }),
    )
    .await;
    if !session_policy.is_persistent() {
        let destroy_result = manager.destroy_session(&session_id).await;
        if let Err(error) = destroy_result {
            tracing::warn!(
                session_id = %session_id,
                task_id = %task_id,
                "failed to destroy RD transient runtime session: {}",
                error
            );
        }
    }
    let turn = turn_result?;
    if session_policy.is_persistent() {
        sqlx::query("UPDATE rd_tasks SET runtime_session_id = ? WHERE id = ? AND tenant_id = ?")
            .bind(&turn.session_id)
            .bind(task_id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;
    }
    let tool_governance = build_rd_runtime_tool_governance(&governance_plan, &turn.tool_calls);
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "runtime",
        "completed",
        "研发 runtime 执行完成",
        json!({
            "sessionId": turn.session_id.clone(),
            "sessionPolicy": if session_policy.is_persistent() { "thread_persistent" } else { "transient" },
            "sessionReused": session_start.reused,
            "sourceTaskId": session_start.source_task_id,
            "toolGovernancePlan": governance_plan_json.clone(),
            "toolCallCount": turn.tool_calls.len(),
            "toolCalls": summarize_rd_tool_calls(&turn.tool_calls),
            "toolGovernance": tool_governance.clone(),
            "iterations": turn.iterations,
            "inputTokens": turn.usage.input_tokens,
            "outputTokens": turn.usage.output_tokens,
            "totalTokens": turn.usage.total_tokens,
            "model": turn.usage.model.clone(),
            "mcpServers": turn.metadata.as_ref().map(|meta| meta.mcp_servers.clone()).unwrap_or_default(),
            "skills": turn.metadata.as_ref().map(|meta| meta.skills.clone()).unwrap_or_default(),
            "permissionMode": turn.metadata.as_ref().map(|meta| meta.permission_mode.clone()),
        }),
    )
    .await?;
    if !turn.tool_calls.is_empty() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "runtime_tool_governance",
            "completed",
            "runtime 工具治理摘要已生成",
            tool_governance,
        )
        .await?;
    }
    Ok(RdCompletionResult {
        text: turn.text,
        model: turn.usage.model.clone(),
        provider: "agent-gateway".to_string(),
        api_key_id: None,
        usage: Some(RdTokenUsageSnapshot::from_gateway(&turn.usage)),
        stop_reason: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_rd_candidate_worktree_completion(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    repository_id: &str,
    baseline: Option<&RdTaskGitBaseline>,
    model: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    context_strategy: Option<RdTaskContextStrategy>,
    prompt: String,
) -> Result<RdCompletionResult, AppError> {
    let agent_runtime_session = crate::routes::agent_ops::find_runtime_session_for_linked_resource(
        state,
        &claims.tenant_id,
        "rd_task",
        task_id,
    )
    .await
    .ok()
    .flatten();
    let agent_runtime_workspace = match agent_runtime_session.as_deref() {
        Some(session_id) => sqlx::query(
            "SELECT workspace_root FROM agent_runtime_sessions WHERE tenant_id = ? AND id = ?",
        )
        .bind(&claims.tenant_id)
        .bind(session_id)
        .fetch_optional(&state.db)
        .await?
        .map(|row| PathBuf::from(row.get::<String, _>("workspace_root")).join("workspace/repo")),
        None => None,
    };
    let candidate = match create_rd_candidate_worktree(
        state,
        claims,
        task_id,
        repository_id,
        baseline,
        agent_runtime_workspace.as_deref(),
    )
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            let error_text = truncate_text(&error.to_string(), 1_500);
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "candidate_worktree",
                "failed",
                "候选工作区创建或基线复制失败，已停止以保护主仓库和 Diff 归属",
                json!({
                    "repositoryId": repository_id,
                    "baselinePolicy": baseline.map(|value| value.baseline_policy.as_str()),
                    "baselineDirty": baseline.is_some_and(RdTaskGitBaseline::is_dirty),
                    "dirtyPathCount": baseline.map(|value| value.dirty_paths.len()).unwrap_or(0),
                    "untrackedFileCount": baseline.map(|value| value.untracked_files.len()).unwrap_or(0),
                    "error": error_text,
                    "errorDebug": truncate_text(&format!("{error:?}"), 4_000),
                }),
            )
            .await?;
            return Err(error);
        }
    };
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "candidate_worktree",
        "running",
        "候选工作区已创建，Coding Agent 将在隔离 worktree 中生成真实文件变更",
        json!({
            "repositoryId": repository_id,
            "candidatePath": candidate.path.to_string_lossy(),
            "agentRuntimeSessionId": agent_runtime_session,
            "baselinePolicy": baseline.map(|value| value.baseline_policy.as_str()),
            "baselineDirty": baseline.is_some_and(RdTaskGitBaseline::is_dirty),
            "baselineCommitCreated": candidate.baseline_commit_created,
        }),
    )
    .await?;
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        Some(repository_id),
        Some(task_id),
        "candidate_worktree_started",
        1.0,
        json!({
            "mode": mode,
            "candidatePath": candidate.path.to_string_lossy(),
            "agentRuntimeSessionId": agent_runtime_session,
            "baselinePolicy": baseline.map(|value| value.baseline_policy.as_str()),
            "baselineCommitCreated": candidate.baseline_commit_created,
        }),
    )
    .await?;

    let manager = state.agent_manager().clone();
    let allowed_tools = ensure_rd_candidate_runtime_tools(allowed_tools);
    let governance_plan = build_rd_runtime_tool_governance_plan(mode, context_strategy.as_ref());
    let governance_plan_json = governance_plan.to_json();
    let governance_prompt_section = governance_plan.to_prompt_section();
    let handle = match manager
        .create_internal_session_in_workspace(
            &claims.sub,
            &claims.tenant_id,
            candidate.path.clone(),
            model,
            RD_CANDIDATE_SOURCE,
            Some(RD_SCENARIO),
            Some(allowed_tools.clone()),
        )
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            cleanup_rd_candidate_worktree(&candidate).await;
            return Err(AppError::Internal(format!(
                "RD candidate runtime create_session failed: {error}"
            )));
        }
    };
    let session_id = handle.session_id.clone();
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "runtime",
        "running",
        "正在使用候选工作区 runtime 真实编辑代码",
        json!({
            "executor": "agent_gateway",
            "source": RD_CANDIDATE_SOURCE,
            "sessionPolicy": "candidate_worktree",
            "sessionId": session_id,
            "repositoryId": repository_id,
            "mode": mode,
            "toolGovernancePlan": governance_plan_json.clone(),
            "allowedTools": allowed_tools.clone(),
            "bashEnabled": rd_candidate_runtime_bash_enabled(),
        }),
    )
    .await?;

    let runtime_prompt = build_rd_candidate_runtime_prompt(
        mode,
        repository_id,
        &prompt,
        Some(&governance_prompt_section),
    );
    let turn_result = run_rd_runtime_turn_with_timeout_cleanup(
        state,
        manager.clone(),
        session_id.clone(),
        &claims.tenant_id,
        task_id,
        runtime_prompt,
        true,
        Some(governance_plan.clone()),
        json!({
            "mode": mode,
            "repositoryId": repository_id,
            "sessionPolicy": "candidate_worktree",
            "source": RD_CANDIDATE_SOURCE,
            "allowedTools": allowed_tools.clone(),
            "bashEnabled": rd_candidate_runtime_bash_enabled(),
            "toolGovernancePlan": governance_plan_json.clone(),
        }),
    )
    .await;
    let mut turn = match turn_result {
        Ok(turn) => turn,
        Err(error) => {
            let _ = manager.destroy_session(&session_id).await;
            cleanup_rd_candidate_worktree(&candidate).await;
            return Err(error);
        }
    };

    // CLI-grade iterative loop: run the repository's test command *inside* the
    // candidate worktree and, on failure, feed the real stderr/stdout back to
    // the same runtime session so the agent can fix its own changes — exactly
    // how a developer iterates in Claude Code / Cursor. The main repo is never
    // touched; only the isolated worktree is edited and re-tested. Gated by env
    // so deployments that prefer single-shot generation keep prior behaviour.
    if mode == "modify" && rd_candidate_verify_enabled() {
        if let Some(test_command) = resolve_rd_test_command(state, claims, repository_id).await {
            let max_attempts = rd_candidate_max_fix_attempts();
            for attempt in 1..=max_attempts {
                let verify = if let Some(runtime_session_id) = agent_runtime_session.as_deref() {
                    run_command_in_dir_with_agent_runtime(
                        state,
                        &claims.tenant_id,
                        runtime_session_id,
                        &candidate.path,
                        &test_command,
                    )
                    .await
                } else {
                    run_command_in_dir(&candidate.path, &test_command).await
                };
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "candidate_verify",
                    &verify.status,
                    "已在候选工作区运行测试命令验证本轮修改",
                    json!({
                        "repositoryId": repository_id,
                        "sessionId": session_id,
                        "command": test_command,
                        "attempt": attempt,
                        "maxAttempts": max_attempts,
                        "exitCode": verify.exit_code,
                    }),
                )
                .await?;
                record_quality_metric(
                    &state.db,
                    &claims.tenant_id,
                    Some(repository_id),
                    Some(task_id),
                    "candidate_verify_run",
                    1.0,
                    json!({
                        "status": verify.status,
                        "attempt": attempt,
                        "exitCode": verify.exit_code,
                    }),
                )
                .await?;

                if verify.status == "passed" {
                    record_quality_metric(
                        &state.db,
                        &claims.tenant_id,
                        Some(repository_id),
                        Some(task_id),
                        "candidate_verify_passed",
                        1.0,
                        json!({ "attempt": attempt }),
                    )
                    .await?;
                    break;
                }
                if verify.status == "cancelled" {
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "candidate_verify",
                        "cancelled",
                        "候选工作区验证已取消，停止后续自动修复",
                        json!({
                            "repositoryId": repository_id,
                            "sessionId": session_id,
                            "attempt": attempt,
                            "maxAttempts": max_attempts,
                        }),
                    )
                    .await?;
                    record_quality_metric(
                        &state.db,
                        &claims.tenant_id,
                        Some(repository_id),
                        Some(task_id),
                        "candidate_verify_cancelled",
                        1.0,
                        json!({ "attempt": attempt }),
                    )
                    .await?;
                    let _ = manager.destroy_session(&session_id).await;
                    cleanup_rd_candidate_worktree(&candidate).await;
                    return Err(rd_task_cancelled_error());
                }

                // Don't spend another model turn if we've exhausted the budget.
                if attempt == max_attempts {
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "candidate_verify",
                        "exhausted",
                        "已达到候选工作区自动修复轮次上限，提交当前修改供人工审查",
                        json!({
                            "repositoryId": repository_id,
                            "sessionId": session_id,
                            "attempt": attempt,
                            "maxAttempts": max_attempts,
                        }),
                    )
                    .await?;
                    record_quality_metric(
                        &state.db,
                        &claims.tenant_id,
                        Some(repository_id),
                        Some(task_id),
                        "candidate_verify_exhausted",
                        1.0,
                        json!({ "attempt": attempt }),
                    )
                    .await?;
                    break;
                }

                let fix_prompt = build_rd_candidate_fix_prompt(&test_command, &verify);
                let fix_result = run_rd_runtime_turn_with_timeout_cleanup(
                    state,
                    manager.clone(),
                    session_id.clone(),
                    &claims.tenant_id,
                    task_id,
                    fix_prompt,
                    true,
                    Some(governance_plan.clone()),
                    json!({
                        "mode": mode,
                        "repositoryId": repository_id,
                        "sessionPolicy": "candidate_worktree",
                        "source": RD_CANDIDATE_SOURCE,
                        "allowedTools": allowed_tools.clone(),
                        "bashEnabled": rd_candidate_runtime_bash_enabled(),
                        "repairAttempt": attempt,
                        "toolGovernancePlan": governance_plan_json.clone(),
                    }),
                )
                .await;
                match fix_result {
                    Ok(fix_turn) => {
                        turn = fix_turn;
                    }
                    Err(error) => {
                        // A failed fix turn shouldn't discard the edits already
                        // made; log and submit the current worktree state.
                        record_event(
                            &state.db,
                            &claims.tenant_id,
                            task_id,
                            "candidate_verify",
                            "failed",
                            "候选工作区修复轮执行失败，提交当前修改供人工审查",
                            json!({
                                "repositoryId": repository_id,
                                "sessionId": session_id,
                                "attempt": attempt,
                                "error": error.to_string(),
                            }),
                        )
                        .await?;
                        break;
                    }
                }
            }
        }
    }

    let tool_governance = build_rd_runtime_tool_governance(&governance_plan, &turn.tool_calls);
    if !turn.tool_calls.is_empty() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "runtime_tool_governance",
            "completed",
            "候选工作区 runtime 工具治理摘要已生成",
            tool_governance.clone(),
        )
        .await?;
    }

    let _ = manager.destroy_session(&session_id).await;

    let candidate_diff = match extract_rd_candidate_diff(&candidate.path).await {
        Ok(diff) => diff,
        Err(error) => {
            cleanup_rd_candidate_worktree(&candidate).await;
            return Err(error);
        }
    };
    cleanup_rd_candidate_worktree(&candidate).await;

    let parsed = parse_rd_output(&turn.text, mode);
    let text = if candidate_diff.trim().is_empty() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "candidate_worktree",
            "completed",
            "候选工作区执行完成，但未产生文件变更",
            json!({
                "repositoryId": repository_id,
                "sessionId": session_id,
                "toolGovernancePlan": governance_plan_json.clone(),
                "toolGovernance": tool_governance.clone(),
                "toolCallCount": turn.tool_calls.len(),
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            Some(repository_id),
            Some(task_id),
            "candidate_worktree_no_diff",
            1.0,
            json!({
                "mode": mode,
                "sessionId": session_id,
                "toolCallCount": turn.tool_calls.len(),
            }),
        )
        .await?;
        sync_rd_candidate_context_to_thread(
            state,
            claims,
            task_id,
            mode,
            repository_id,
            model,
            &turn,
            &candidate_diff,
            &parsed,
        )
        .await;
        turn.text.clone()
    } else {
        let touched_files = infer_files_from_unified_diff(&candidate_diff);
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "candidate_worktree",
            "completed",
            "候选工作区已生成真实 Git Diff",
            json!({
                "repositoryId": repository_id,
                "sessionId": session_id,
                "diffBytes": candidate_diff.len(),
                "touchedFiles": touched_files.clone(),
                "toolGovernancePlan": governance_plan_json.clone(),
                "toolGovernance": tool_governance.clone(),
                "toolCallCount": turn.tool_calls.len(),
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            Some(repository_id),
            Some(task_id),
            "candidate_worktree_diff_generated",
            1.0,
            json!({
                "mode": mode,
                "sessionId": session_id,
                "diffBytes": candidate_diff.len(),
                "touchedFileCount": touched_files.len(),
                "toolCallCount": turn.tool_calls.len(),
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            Some(repository_id),
            Some(task_id),
            "candidate_worktree_diff_bytes",
            candidate_diff.len() as f64,
            json!({
                "mode": mode,
                "sessionId": session_id,
                "touchedFiles": touched_files.clone(),
            }),
        )
        .await?;
        sync_rd_candidate_context_to_thread(
            state,
            claims,
            task_id,
            mode,
            repository_id,
            model,
            &turn,
            &candidate_diff,
            &parsed,
        )
        .await;
        serde_json::to_string(&json!({
            "planMd": parsed.plan_md,
            "answerMd": format!(
                "{}\n\n## Candidate Worktree\n\nAgent 已在隔离候选工作区完成文件编辑，AOS 已提取真实 `git diff` 并进入审批流。",
                parsed.answer_md
            ),
            "reviewMd": parsed.review_md,
            "prTitle": parsed.pr_title,
            "prDescription": parsed.pr_description,
            "unifiedDiff": candidate_diff,
            "touchedFiles": infer_files_from_unified_diff(&candidate_diff),
        }))
        .map_err(|error| AppError::Internal(format!("serialize candidate output: {error}")))?
    };

    Ok(RdCompletionResult {
        text,
        model: turn.usage.model.clone(),
        provider: "agent-gateway".to_string(),
        api_key_id: None,
        usage: Some(RdTokenUsageSnapshot::from_gateway(&turn.usage)),
        stop_reason: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn sync_rd_candidate_context_to_thread(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    repository_id: &str,
    model: Option<&str>,
    turn: &agent_gateway::TurnResult,
    candidate_diff: &str,
    parsed: &ParsedRdOutput,
) {
    let sync_result = async {
        let session_start = start_rd_runtime_session(
            state,
            claims,
            task_id,
            Some(repository_id),
            model,
            None,
            RdRuntimeSessionPolicy::ThreadPersistent,
        )
        .await?;
        let session_id = session_start.handle.session_id.clone();
        sqlx::query("UPDATE rd_tasks SET runtime_session_id = ? WHERE id = ? AND tenant_id = ?")
            .bind(&session_id)
            .bind(task_id)
            .bind(&claims.tenant_id)
            .execute(&state.db)
            .await?;

        let touched_files = infer_files_from_unified_diff(candidate_diff);
        let context = build_rd_candidate_context_message(
            task_id,
            mode,
            repository_id,
            &turn.session_id,
            candidate_diff,
            &touched_files,
            turn.tool_calls.len(),
            parsed,
        );
        state
            .agent_manager()
            .append_internal_context_message(&session_id, &claims.tenant_id, &claims.sub, context)
            .await
            .map_err(|error| {
                AppError::Internal(format!("RD candidate context sync failed: {error}"))
            })?;
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "candidate_context",
            "completed",
            "候选工作区上下文已同步到研发长期会话",
            json!({
                "repositoryId": repository_id,
                "threadSessionId": session_id.clone(),
                "candidateSessionId": turn.session_id.clone(),
                "sessionReused": session_start.reused,
                "sourceTaskId": session_start.source_task_id,
                "diffBytes": candidate_diff.len(),
                "touchedFiles": touched_files.clone(),
            }),
        )
        .await?;
        record_quality_metric(
            &state.db,
            &claims.tenant_id,
            Some(repository_id),
            Some(task_id),
            "candidate_context_synced",
            1.0,
            json!({
                "threadSessionId": session_id,
                "candidateSessionId": turn.session_id.clone(),
                "diffBytes": candidate_diff.len(),
                "touchedFileCount": touched_files.len(),
            }),
        )
        .await?;
        Ok::<(), AppError>(())
    }
    .await;

    if let Err(error) = sync_result {
        let error_message = error.to_string();
        let _ = record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "candidate_context",
            "failed",
            "候选工作区上下文同步失败，后续追问可能需要重新读取相关文件",
            json!({
                "repositoryId": repository_id,
                "candidateSessionId": turn.session_id.clone(),
                "error": truncate_text(&error_message, 2_000),
            }),
        )
        .await;
        let _ = record_quality_metric(
            &state.db,
            &claims.tenant_id,
            Some(repository_id),
            Some(task_id),
            "candidate_context_sync_failed",
            1.0,
            json!({
                "candidateSessionId": turn.session_id.clone(),
                "error": truncate_text(&error_message, 2_000),
            }),
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_rd_candidate_context_message(
    task_id: &str,
    mode: &str,
    repository_id: &str,
    candidate_session_id: &str,
    candidate_diff: &str,
    touched_files: &[String],
    tool_call_count: usize,
    parsed: &ParsedRdOutput,
) -> String {
    let touched = if touched_files.is_empty() {
        "无文件变更".to_string()
    } else {
        touched_files
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n- ")
    };
    let touched_section = if touched_files.is_empty() {
        touched
    } else {
        format!("- {touched}")
    };
    let diff_section = if candidate_diff.trim().is_empty() {
        "本轮候选工作区没有产生 Git Diff。".to_string()
    } else {
        format!(
            "本轮候选工作区产生了 {} bytes 的 Git Diff，主仓库尚未修改，Diff 已进入 AOS 审批流。\nDiff artifact: task_id={task_id}; source=rd_file_changes; diff_hash={}; touched_file_count={}。\n为节省长期会话 token，这里不内联完整 diff。后续如需继续修改，请先读取当前真实文件，并参考任务详情中的 pending diff/touched files。",
            candidate_diff.len(),
            stable_hash_hex(candidate_diff),
            touched_files.len()
        )
    };

    format!(
        "<system-reminder>\nAOS Code Studio candidate worktree context.\n\
         This is an internal continuity note for future turns; do not present it as a new user request.\n\n\
         Task ID: {task_id}\n\
         Mode: {mode}\n\
         Repository ID: {repository_id}\n\
         Candidate runtime session: {candidate_session_id} (destroyed after diff extraction)\n\
         Tool calls: {tool_call_count}\n\n\
         Touched files:\n{touched_section}\n\n\
         Candidate plan summary:\n{}\n\n\
         Candidate answer summary:\n{}\n\n\
         {diff_section}\n\n\
         Follow-up guidance: treat this as the previous proposed change. If the user asks to continue, inspect current repository files and pending task diffs before producing another patch.\n\
         </system-reminder>",
        truncate_text(&parsed.plan_md, 4_000),
        truncate_text(&parsed.answer_md, 4_000),
    )
}

fn build_rd_candidate_runtime_prompt(
    mode: &str,
    repository_id: &str,
    prompt: &str,
    governance_section: Option<&str>,
) -> String {
    let governance_section = governance_section
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(|section| format!("\n\n{section}"))
        .unwrap_or_default();
    format!(
        "{prompt}\n\n## Candidate Worktree 执行约束\n- 当前模式：{mode}\n- 当前 runtime 工作目录是 repositoryId={repository_id} 的隔离候选 worktree，不是主仓库。\n- 如果主仓库在任务开始前已有未提交变更，AOS 已把它们作为候选工作区 baseline；它们是当前代码事实，但不是本任务产物。\n- 你可以像 claw-code / Claude Code CLI 一样使用 glob_search、grep_search、read_file、edit_file、write_file 真实修改候选工作区文件。\n- 不要提交代码、推送代码、删除仓库、安装依赖或执行具有写入副作用的 bash 命令；测试命令仍由 AOS 的测试接口执行。\n- 修改完成后输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":null,\"touchedFiles\":array}}。\n- 你不需要手写最终 unifiedDiff；AOS 会从候选 worktree 提取真实 git diff 并交给用户审批。\n- 如果无法安全修改，明确说明原因，不要假装已经修改。{governance_section}"
    )
}
