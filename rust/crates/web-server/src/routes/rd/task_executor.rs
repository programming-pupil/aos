//! Main RD task execution orchestrator.

use super::*;

pub(super) async fn execute_rd_task(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    prompt: &str,
    context_strategy: RdTaskContextStrategy,
    baseline_policy: RdGitBaselinePolicy,
    repository_id: Option<&str>,
    model: Option<&str>,
    agent_profile_id: Option<&str>,
    workflow_id: Option<&str>,
) -> Result<(), AppError> {
    let worker_id = format!("rd-worker:{task_id}");
    let _ = crate::routes::agent_ops::heartbeat_agent_task_for_linked_resource(
        state,
        &claims.tenant_id,
        "rd_task",
        task_id,
        &worker_id,
        600,
    )
    .await;
    let agent_profile = match agent_profile_id {
        Some(id) => Some(load_enabled_agent_profile(&state.db, &claims.tenant_id, id).await?),
        None => None,
    };
    let workflow = match workflow_id {
        Some(id) => Some(load_enabled_agent_workflow(&state.db, &claims.tenant_id, id).await?),
        None => None,
    };
    let steering = build_steering_context(&state.db, &claims.tenant_id, repository_id).await?;
    let context_profile = context_strategy.profile;
    let context_budget = context_profile.budget();
    let use_runtime = rd_runtime_executor_enabled();
    let task_git_baseline = if let Some(repo_id) = repository_id {
        Some(
            capture_and_record_rd_task_git_baseline(
                state,
                claims,
                task_id,
                repo_id,
                baseline_policy,
            )
            .await?,
        )
    } else {
        None
    };
    let repo_context = if let Some(repo_id) = repository_id {
        let _ = crate::routes::agent_ops::heartbeat_agent_task_for_linked_resource(
            state,
            &claims.tenant_id,
            "rd_task",
            task_id,
            &worker_id,
            600,
        )
        .await;
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "context",
            "running",
            if use_runtime {
                "正在准备 runtime 仓库工作目录"
            } else {
                "正在读取仓库上下文"
            },
            json!({
                "repositoryId": repo_id,
                "strategy": if use_runtime { "runtime_workspace" } else { "inline_context" },
                "contextProfile": context_profile.as_str(),
                "contextProfileName": context_profile.display_name(),
                "contextDepth": &context_strategy.depth,
                "shouldDeepScan": context_strategy.should_deep_scan,
                "budget": rd_context_budget_json(context_budget),
            }),
        )
        .await?;
        let context = if use_runtime {
            build_repository_runtime_context_hint(
                state,
                claims,
                repo_id,
                prompt,
                context_profile,
                Some(task_id),
            )
            .await?
        } else {
            build_repository_context_for_prompt(
                state,
                claims,
                repo_id,
                prompt,
                context_budget.inline_context_bytes,
                context_profile,
                Some(task_id),
            )
            .await?
        };
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "context",
            "completed",
            if use_runtime {
                "runtime 仓库工作目录已准备完成"
            } else {
                "仓库上下文读取完成"
            },
            json!({
                "chars": context.chars().count(),
                "strategy": if use_runtime { "runtime_workspace" } else { "inline_context" },
                "contextProfile": context_profile.as_str(),
                "contextProfileName": context_profile.display_name(),
                "contextDepth": &context_strategy.depth,
                "shouldDeepScan": context_strategy.should_deep_scan,
                "budget": rd_context_budget_json(context_budget),
            }),
        )
        .await?;
        context
    } else {
        String::new()
    };
    let repo_instructions =
        load_repository_instructions_for_task(state, claims, task_id, repository_id, "main")
            .await?;
    let explicit_file_context =
        load_prompt_file_context_for_task(state, claims, task_id, repository_id, prompt).await?;
    let runtime_allowed_tools = extract_rd_allowed_tools(
        agent_profile
            .as_ref()
            .and_then(|profile| profile.allowed_tools.as_ref()),
    )?;
    let workflow_stages = workflow
        .as_ref()
        .map(|workflow| rd_workflow_stages(&workflow.definition_json))
        .unwrap_or_default();
    if agent_profile.is_some() || steering.rule_count > 0 || workflow.is_some() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "steering",
            "completed",
            "已加载研发 Agent、团队规范与工作流",
            json!({
                "agentProfileId": agent_profile.as_ref().map(|profile| profile.id.clone()),
                "agentProfileName": agent_profile.as_ref().map(|profile| profile.name.clone()),
                "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
                "workflowName": workflow.as_ref().map(|workflow| workflow.name.clone()),
                "workflowStageCount": workflow_stages.len(),
                "runtimeAllowedTools": runtime_allowed_tools.clone().unwrap_or_default(),
                "steeringRuleCount": steering.rule_count,
                "steeringRuleNames": steering.rule_names.clone(),
                "contextProfile": context_profile.as_str(),
                "contextDepth": &context_strategy.depth,
                "shouldDeepScan": context_strategy.should_deep_scan,
            }),
        )
        .await?;
    }
    let workflow_preflight_context = if let Some(workflow) = workflow.as_ref() {
        maybe_run_rd_workflow_preflight_stages(
            state,
            claims,
            task_id,
            workflow,
            &workflow_stages,
            mode,
            prompt,
            repository_id,
            model,
            runtime_allowed_tools.clone(),
            &repo_context,
            &explicit_file_context,
        )
        .await?
    } else {
        String::new()
    };
    let workflow_section = workflow
        .as_ref()
        .map(|workflow| {
            workflow_definition_section(workflow, &workflow_stages, &workflow_preflight_context)
        })
        .unwrap_or_default();
    let architecture_context = maybe_run_rd_architecture_pass(
        state,
        claims,
        task_id,
        mode,
        prompt,
        repository_id,
        model,
        context_profile,
    )
    .await?
    .unwrap_or_default();
    let architecture_section = if architecture_context.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n多 Agent 架构预分析：\n{}", architecture_context.trim())
    };
    let preview_debug_section =
        build_preview_debug_evidence_section(state, claims, task_id, repository_id, prompt).await?;
    let prescan_context = if let Some(repo_id) =
        repository_id.filter(|_| should_run_rd_repository_prescan(context_profile, mode, prompt))
    {
        match build_repository_prescan_context(state, claims, repo_id).await {
            Ok(context) => {
                if !context.trim().is_empty() {
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "context_prescan",
                        "completed",
                        "已完成宽泛任务本地预扫描",
                        json!({"repositoryId": repo_id, "chars": context.chars().count()}),
                    )
                    .await?;
                }
                context
            }
            Err(error) => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "context_prescan",
                    "skipped",
                    "宽泛任务本地预扫描失败，已继续任务",
                    json!({"repositoryId": repo_id, "error": error.to_string()}),
                )
                .await?;
                String::new()
            }
        }
    } else {
        String::new()
    };
    let prescan_section = if prescan_context.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n本地预扫描候选入口（仅作路线图，关键判断必须读取真实文件）：\n{}",
            prescan_context.trim()
        )
    };
    let explicit_file_section = if explicit_file_context.text.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n用户显式 @ 文件上下文：\n{}\n\n请优先参考这些文件，但仍可使用 runtime 工具核对最新内容和相关调用链。",
            explicit_file_context.text.trim()
        )
    };

    let context_policy_section = build_rd_context_policy_section(context_profile);
    let (context_plan_section, context_plan_detail) = build_rd_context_plan_section(
        context_profile,
        repository_id.is_some(),
        !explicit_file_context.files.is_empty(),
        workflow.is_some(),
    );
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "context_plan",
        "completed",
        "已生成自适应上下文计划",
        context_plan_detail,
    )
    .await?;
    let llm_context_plan = maybe_run_rd_llm_context_planner(
        state,
        claims,
        task_id,
        mode,
        prompt,
        repository_id,
        model,
        context_profile,
        &context_plan_section,
        &repo_context,
        &explicit_file_context,
        workflow.is_some(),
        workflow_stages.len(),
    )
    .await;
    let llm_context_plan_section = llm_context_plan
        .as_ref()
        .map(|plan| format!("\n\n{}", build_rd_llm_context_plan_section(plan)))
        .unwrap_or_default();
    if let Some(plan) = llm_context_plan.as_ref() {
        let effective_strategy = RdTaskContextStrategy {
            profile: plan.profile,
            depth: plan.depth.clone(),
            should_deep_scan: plan.should_deep_scan || plan.depth == "deep",
        };
        update_rd_task_context_strategy(&state.db, &claims.tenant_id, task_id, &effective_strategy)
            .await?;
    }
    let runtime_context_strategy = llm_context_plan.as_ref().map_or_else(
        || context_strategy.clone(),
        |plan| RdTaskContextStrategy {
            profile: plan.profile,
            depth: plan.depth.clone(),
            should_deep_scan: plan.should_deep_scan || plan.depth == "deep",
        },
    );
    let system =
        build_rd_system_prompt(mode, agent_profile.as_ref(), &steering, &repo_instructions);
    let user = format!(
        "{system}\n\n{context_policy_section}\n\n{context_plan_section}{llm_context_plan_section}\n\n用户任务：\n{prompt}\n\n仓库上下文：\n{repo_context}{prescan_section}{explicit_file_section}{workflow_section}{architecture_section}{preview_debug_section}\n\n请严格输出 JSON，不要输出 Markdown fence。"
    );
    let _ = crate::routes::agent_ops::heartbeat_agent_task_for_linked_resource(
        state,
        &claims.tenant_id,
        "rd_task",
        task_id,
        &worker_id,
        600,
    )
    .await;
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "plan",
        "running",
        "正在生成计划和结果",
        json!({
            "mode": mode,
            "contextProfile": context_profile.as_str(),
            "contextProfileName": context_profile.display_name(),
            "contextDepth": &context_strategy.depth,
            "shouldDeepScan": context_strategy.should_deep_scan,
            "budget": rd_context_budget_json(context_budget),
            "llmContextPlanner": llm_context_plan.as_ref().map(|plan| json!({
                "profile": plan.profile.as_str(),
                "profileName": plan.profile.display_name(),
                "depth": &plan.depth,
                "shouldDeepScan": plan.should_deep_scan,
                "confidence": plan.confidence,
            })),
        }),
    )
    .await?;
    run_rd_hook(
        state,
        claims,
        HookEventType::BeforeModelCall,
        "rd.model.generate",
        json!({
            "taskId": task_id,
            "mode": mode,
            "contextProfile": context_profile.as_str(),
            "contextDepth": &context_strategy.depth,
            "shouldDeepScan": context_strategy.should_deep_scan,
            "repositoryId": repository_id,
            "promptChars": prompt.chars().count(),
            "contextChars": repo_context.chars().count(),
            "explicitFileContextChars": explicit_file_context.text.chars().count(),
            "explicitFileContextFiles": explicit_file_context.files.clone(),
            "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
            "workflowName": workflow.as_ref().map(|workflow| workflow.name.clone()),
        }),
        None,
        false,
        true,
    )
    .await?;
    let completion = if use_runtime
        && mode == "modify"
        && repository_id.is_some()
        && rd_candidate_worktree_enabled()
    {
        let Some(repo_id) = repository_id else {
            return Err(AppError::ValidationError(
                "repository is required for RD candidate worktree".to_string(),
            ));
        };
        match run_rd_candidate_worktree_completion(
            state,
            claims,
            task_id,
            mode,
            repo_id,
            task_git_baseline.as_ref(),
            model,
            runtime_allowed_tools.clone(),
            Some(runtime_context_strategy.clone()),
            user.clone(),
        )
        .await
        {
            Ok(completion) => completion,
            Err(error) => {
                if rd_error_is_cancelled(&error) {
                    return Err(error);
                }
                let error_text = truncate_text(&error.to_string(), 1_000);
                if task_git_baseline
                    .as_ref()
                    .is_some_and(RdTaskGitBaseline::is_dirty)
                {
                    record_event(
                        &state.db,
                        &claims.tenant_id,
                        task_id,
                        "diff_guard",
                        "failed",
                        "候选工作区失败且任务基线存在未提交变更，已禁止回退到主工作区生成 Diff，避免污染本次审批",
                        json!({
                            "error": error_text.clone(),
                            "baselinePolicy": task_git_baseline.as_ref().map(|value| value.baseline_policy.as_str()),
                            "dirtyPathCount": task_git_baseline.as_ref().map(|value| value.dirty_paths.len()).unwrap_or(0),
                            "fallback": false,
                        }),
                    )
                    .await?;
                    return Err(AppError::Internal(format!(
                        "RD candidate worktree failed and dirty baseline fallback is disabled: {error_text}"
                    )));
                }
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "candidate_worktree",
                    "failed",
                    &format!("候选工作区执行失败，已回退到安全 Diff-first runtime：{error_text}"),
                    json!({"error": error_text.clone(), "fallback": "diff_first_runtime"}),
                )
                .await?;
                record_quality_metric(
                    &state.db,
                    &claims.tenant_id,
                    repository_id,
                    Some(task_id),
                    "candidate_worktree_failed",
                    1.0,
                    json!({"mode": mode, "error": error_text}),
                )
                .await?;
                run_rd_runtime_completion(
                    state,
                    claims,
                    task_id,
                    mode,
                    repository_id,
                    model,
                    runtime_allowed_tools.clone(),
                    RdRuntimeSessionPolicy::ThreadPersistent,
                    Some(runtime_context_strategy.clone()),
                    user.clone(),
                )
                .await?
            }
        }
    } else if use_runtime {
        if mode == "modify"
            && repository_id.is_some()
            && task_git_baseline
                .as_ref()
                .is_some_and(RdTaskGitBaseline::is_dirty)
        {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_guard",
                "failed",
                "当前仓库存在未提交变更，但候选工作区未启用；已拒绝在主工作区生成修改 Diff，避免把历史改动归属到本任务",
                json!({
                    "baselinePolicy": task_git_baseline.as_ref().map(|value| value.baseline_policy.as_str()),
                    "dirtyPathCount": task_git_baseline.as_ref().map(|value| value.dirty_paths.len()).unwrap_or(0),
                    "candidateWorktreeEnabled": rd_candidate_worktree_enabled(),
                }),
            )
            .await?;
            return Err(AppError::ValidationError(
                "current worktree is dirty; enable RD candidate worktree to isolate task diff ownership"
                    .to_string(),
            ));
        }
        match run_rd_runtime_completion(
            state,
            claims,
            task_id,
            mode,
            repository_id,
            model,
            runtime_allowed_tools.clone(),
            RdRuntimeSessionPolicy::ThreadPersistent,
            Some(runtime_context_strategy.clone()),
            user.clone(),
        )
        .await
        {
            Ok(completion) => completion,
            Err(error) if rd_runtime_direct_fallback_enabled() => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "runtime",
                    "failed",
                    "研发 runtime 执行失败，已回退到直连模型生成",
                    json!({"error": error.to_string()}),
                )
                .await?;
                run_rd_completion(state, &claims.tenant_id, &claims.sub, model, user).await?
            }
            Err(error) => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "runtime",
                    "failed",
                    "研发 runtime 执行失败；默认不回退到直连模型，以避免代码能力退化",
                    json!({"error": error.to_string(), "fallback": false}),
                )
                .await?;
                return Err(error);
            }
        }
    } else {
        if mode == "modify"
            && repository_id.is_some()
            && task_git_baseline
                .as_ref()
                .is_some_and(RdTaskGitBaseline::is_dirty)
        {
            record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "diff_guard",
                "failed",
                "当前仓库存在未提交变更，但 runtime/candidate 未启用；已拒绝直连模型生成修改 Diff，避免把历史改动归属到本任务",
                json!({
                    "baselinePolicy": task_git_baseline.as_ref().map(|value| value.baseline_policy.as_str()),
                    "dirtyPathCount": task_git_baseline.as_ref().map(|value| value.dirty_paths.len()).unwrap_or(0),
                    "runtimeEnabled": use_runtime,
                    "candidateWorktreeEnabled": rd_candidate_worktree_enabled(),
                }),
            )
            .await?;
            return Err(AppError::ValidationError(
                "current worktree is dirty; RD runtime candidate worktree is required for safe diff ownership"
                    .to_string(),
            ));
        }
        run_rd_completion(state, &claims.tenant_id, &claims.sub, model, user).await?
    };
    let _ = crate::routes::agent_ops::heartbeat_agent_task_for_linked_resource(
        state,
        &claims.tenant_id,
        "rd_task",
        task_id,
        &worker_id,
        600,
    )
    .await;
    let mut parsed = parse_rd_output(&completion.text, mode);
    sanitize_rd_parsed_diff_output(&mut parsed);
    run_rd_hook(
        state,
        claims,
        HookEventType::AfterModelCall,
        "rd.model.generate",
        json!({
            "taskId": task_id,
            "mode": mode,
            "repositoryId": repository_id,
            "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
        }),
        Some(json!({
            "model": completion.model.clone(),
            "provider": completion.provider.clone(),
            "outputChars": completion.text.chars().count(),
            "hasDiff": parsed.unified_diff.as_deref().is_some_and(|v| !v.trim().is_empty()),
        })),
        false,
        false,
    )
    .await?;
    maybe_run_rd_reviewer_pass(
        state,
        claims,
        task_id,
        mode,
        prompt,
        repository_id,
        Some(completion.model.as_str()),
        &mut parsed,
    )
    .await?;
    if let Some(workflow) = workflow.as_ref() {
        maybe_run_rd_workflow_postflight_stages(
            state,
            claims,
            task_id,
            workflow,
            &workflow_stages,
            mode,
            prompt,
            repository_id,
            Some(completion.model.as_str()),
            runtime_allowed_tools.clone(),
            &repo_context,
            &explicit_file_context,
            &mut parsed,
        )
        .await?;
    }
    enforce_rd_diff_output_policy(state, claims, task_id, mode, &mut parsed).await?;
    let status = if parsed
        .unified_diff
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
    {
        "waiting_approval"
    } else {
        "completed"
    };
    sqlx::query("UPDATE rd_tasks SET status = ?, model = ?, plan_md = ?, answer_md = ?, review_md = ?, pr_title = ?, pr_description = ?, completed_at = IIF(? = 'completed', CURRENT_TIMESTAMP, NULL) WHERE id = ? AND tenant_id = ?")
        .bind(status)
        .bind(&completion.model)
        .bind(&parsed.plan_md)
        .bind(&parsed.answer_md)
        .bind(&parsed.review_md)
        .bind(&parsed.pr_title)
        .bind(&parsed.pr_description)
        .bind(status)
        .bind(task_id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if let Some(diff) = parsed
        .unified_diff
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let touched_files = if parsed.touched_files.is_empty() {
            infer_files_from_unified_diff(diff)
        } else {
            parsed.touched_files.clone()
        };
        let file_path = touched_files.first().cloned().unwrap_or_else(|| {
            infer_first_file_from_diff(&diff).unwrap_or_else(|| "changes.diff".to_string())
        });
        let change_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO rd_file_changes (id, task_id, tenant_id, repository_id, file_path, change_type, diff_patch) VALUES (?, ?, ?, ?, ?, 'modify', ?)")
            .bind(&change_id)
            .bind(task_id)
            .bind(&claims.tenant_id)
            .bind(repository_id)
            .bind(file_path)
            .bind(&diff)
            .execute(&state.db)
            .await?;
        record_rd_patch_ownerships(
            &state.db,
            &claims.tenant_id,
            repository_id,
            task_id,
            &change_id,
            &touched_files,
            diff,
        )
        .await?;
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "diff",
            "waiting_approval",
            "已生成待确认 Diff",
            json!({}),
        )
        .await?;
        let diff_check = validate_generated_diff(
            state,
            claims,
            task_id,
            repository_id,
            &change_id,
            &diff,
            "main",
        )
        .await?;
        if diff_check.status == GeneratedDiffCheckStatus::Failed {
            maybe_repair_invalid_generated_diff(
                state,
                claims,
                task_id,
                mode,
                prompt,
                repository_id,
                Some(completion.model.as_str()),
                &change_id,
                &diff,
                diff_check.error_message.as_deref(),
                &parsed.plan_md,
                &parsed.answer_md,
                "main",
            )
            .await?;
        }
        record_rd_task_risk_map(
            &state.db,
            &claims.tenant_id,
            task_id,
            mode,
            status,
            &parsed,
            Some(diff),
            Some(&diff_check),
            "main",
        )
        .await?;
    } else {
        record_rd_task_risk_map(
            &state.db,
            &claims.tenant_id,
            task_id,
            mode,
            status,
            &parsed,
            None,
            None,
            "main",
        )
        .await?;
    }
    record_quality_metric(
        &state.db,
        &claims.tenant_id,
        repository_id,
        Some(task_id),
        if status == "completed" {
            "task_completed"
        } else {
            "task_waiting_approval"
        },
        1.0,
        json!({
            "mode": mode,
            "model": completion.model.clone(),
            "provider": completion.provider.clone(),
            "apiKeyId": completion.api_key_id.clone(),
            "status": status,
            "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
        }),
    )
    .await?;
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "summary",
        status,
        "研发任务已生成结果",
        json!({
            "model": completion.model.clone(),
            "provider": completion.provider.clone(),
            "apiKeyId": completion.api_key_id.clone(),
            "usage": completion.usage.clone(),
        }),
    )
    .await?;
    run_rd_hook(
        state,
        claims,
        HookEventType::TaskCompleted,
        "rd.task.completed",
        json!({
            "taskId": task_id,
            "mode": mode,
            "repositoryId": repository_id,
            "workflowId": workflow.as_ref().map(|workflow| workflow.id.clone()),
        }),
        Some(json!({"status": status})),
        false,
        false,
    )
    .await?;
    schedule_rd_task_embedding_index(
        state.clone(),
        claims.tenant_id.clone(),
        claims.sub.clone(),
        task_id.to_string(),
    );
    Ok(())
}

async fn build_preview_debug_evidence_section(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    repository_id: Option<&str>,
    prompt: &str,
) -> Result<String, AppError> {
    if !rd_prompt_mentions_preview_debug(prompt) {
        return Ok(String::new());
    }
    let Some(repository_id) = repository_id else {
        return Ok(String::new());
    };
    let rows = sqlx::query(
        r"
        SELECT ps.id, ps.status, ps.command, ps.port, ps.url, ps.logs_preview, ps.last_error,
               pe.event_type, pe.severity, pe.message, CAST(pe.metadata_json AS TEXT) AS metadata_json,
               CAST(pe.created_at AS TEXT) AS event_created_at
        FROM rd_preview_sessions ps
        LEFT JOIN (
          SELECT id, tenant_id, session_id, event_type, severity, message, metadata_json, created_at
          FROM (
            SELECT pe2.*,
                   ROW_NUMBER() OVER (
                     PARTITION BY pe2.tenant_id, pe2.session_id
                     ORDER BY pe2.created_at DESC
                   ) AS rn
            FROM rd_preview_events pe2
          ) ranked_events
          WHERE rn <= 5
        ) pe
          ON pe.tenant_id = ps.tenant_id
         AND pe.session_id = ps.id
        WHERE ps.tenant_id = ?
          AND ps.user_id = ?
          AND ps.repository_id = ?
          AND (ps.task_id = ? OR ps.task_id IS NULL)
        ORDER BY ps.updated_at DESC, pe.created_at DESC
        LIMIT 12
        ",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(repository_id)
    .bind(task_id)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "preview_debug",
            "skipped",
            "用户提到预览/浏览器问题，但未找到可用 Preview Debug 证据",
            json!({"repositoryId": repository_id}),
        )
        .await?;
        return Ok(String::new());
    }
    let mut lines = vec![
        "\n\nPreview Debug 证据（由 Code Studio 自动采集；必须读取真实文件后再修改，不能只凭证据猜测）：".to_string(),
    ];
    let mut seen_sessions = HashSet::<String>::new();
    for row in rows {
        let session_id: String = row.get("id");
        if seen_sessions.insert(session_id.clone()) {
            let status: String = row.get("status");
            let command: String = row.get("command");
            let port = row
                .try_get::<Option<u64>, _>("port")
                .ok()
                .flatten()
                .map(|value| format!(":{value}"))
                .unwrap_or_default();
            let url: Option<String> = row.get("url");
            let last_error: Option<String> = row.get("last_error");
            lines.push(format!(
                "- Session {session_id} · {status}{port} · {}",
                truncate_text(&command, 140)
            ));
            if let Some(url) = url.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("  url: {url}"));
            }
            if let Some(error) = last_error.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("  lastError: {}", truncate_text(&error, 300)));
            }
            let logs_preview: Option<String> = row.get("logs_preview");
            if let Some(logs) = logs_preview.filter(|value| !value.trim().is_empty()) {
                lines.push(format!("  runtimeLogs: {}", truncate_text(&logs, 800)));
            }
        }
        let event_type: Option<String> = row.get("event_type");
        let event_message: Option<String> = row.get("message");
        if let (Some(event_type), Some(message)) = (event_type, event_message) {
            let severity: Option<String> = row.get("severity");
            lines.push(format!(
                "  event: {} {} · {}",
                severity.unwrap_or_else(|| "info".to_string()),
                event_type,
                truncate_text(&message, 500)
            ));
        }
    }
    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "preview_debug",
        "completed",
        "已注入 Preview Debug 证据到研发 Agent prompt",
        json!({
            "repositoryId": repository_id,
            "evidenceChars": lines.join("\n").chars().count()
        }),
    )
    .await?;
    Ok(lines.join("\n"))
}

fn rd_prompt_mentions_preview_debug(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    prompt.contains("预览")
        || prompt.contains("页面")
        || prompt.contains("浏览器")
        || prompt.contains("控制台")
        || prompt.contains("截图")
        || lower.contains("preview")
        || lower.contains("console")
        || lower.contains("network")
        || lower.contains("browser")
        || lower.contains("dev server")
        || lower.contains("screenshot")
}
