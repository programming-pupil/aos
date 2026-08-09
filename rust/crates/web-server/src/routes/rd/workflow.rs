//! Agent workflow CRUD, stage classification, and execution orchestration.

use super::*;

pub(super) async fn list_agent_workflows(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<RdAgentWorkflowDto>>, AppError> {
    let rows = sqlx::query("SELECT id, name, description, definition_json, source, source_item_id, enabled, CAST(created_at AS TEXT) created_at, CAST(updated_at AS TEXT) updated_at FROM rd_agent_workflows WHERE tenant_id = ? ORDER BY enabled DESC, updated_at DESC")
        .bind(&claims.tenant_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows.iter().map(row_to_agent_workflow).collect()))
}

pub(super) async fn create_agent_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RdAgentWorkflowRequest>,
) -> Result<Json<RdAgentWorkflowDto>, AppError> {
    let name = require_non_empty(&req.name, "name")?;
    validate_rd_workflow_definition(&req.definition_json)?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO rd_agent_workflows (id, tenant_id, name, description, definition_json, source, source_item_id, enabled) VALUES (?, ?, ?, ?, ?, 'custom', NULL, ?)")
        .bind(&id)
        .bind(&claims.tenant_id)
        .bind(&name)
        .bind(normalize_optional(req.description.as_deref()))
        .bind(&req.definition_json)
        .bind(req.enabled.unwrap_or(true))
        .execute(&state.db)
        .await?;
    get_agent_workflow_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn update_agent_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<RdAgentWorkflowRequest>,
) -> Result<Json<RdAgentWorkflowDto>, AppError> {
    let name = require_non_empty(&req.name, "name")?;
    validate_rd_workflow_definition(&req.definition_json)?;
    let result = sqlx::query("UPDATE rd_agent_workflows SET name = ?, description = ?, definition_json = ?, enabled = ? WHERE id = ? AND tenant_id = ?")
        .bind(&name)
        .bind(normalize_optional(req.description.as_deref()))
        .bind(&req.definition_json)
        .bind(req.enabled.unwrap_or(true))
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "rd agent workflow not found".to_string(),
        ));
    }
    get_agent_workflow_row(&state.db, &claims.tenant_id, &id)
        .await
        .map(Json)
}

pub(super) async fn delete_agent_workflow(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM rd_agent_workflows WHERE id = ? AND tenant_id = ?")
        .bind(&id)
        .bind(&claims.tenant_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "deleted": result.rows_affected() > 0 })))
}

fn validate_rd_workflow_definition(definition: &Value) -> Result<(), AppError> {
    let stages = definition
        .get("stages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::ValidationError("definition_json.stages must be an array".to_string())
        })?;
    if stages.is_empty() {
        return Err(AppError::ValidationError(
            "definition_json.stages cannot be empty".to_string(),
        ));
    }
    for (index, stage) in stages.iter().enumerate() {
        let Some(object) = stage.as_object() else {
            return Err(AppError::ValidationError(format!(
                "definition_json.stages[{index}] must be an object"
            )));
        };
        let agent = object
            .get("agent")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if agent.is_empty() {
            return Err(AppError::ValidationError(format!(
                "definition_json.stages[{index}].agent is required"
            )));
        }
    }
    Ok(())
}

pub(super) fn rd_workflow_stages(definition: &Value) -> Vec<RdWorkflowStageSpec> {
    definition
        .get("stages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, stage)| {
            let object = stage.as_object()?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("stage_{}", index + 1));
            let agent = object
                .get("agent")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("Coding Agent")
                .to_string();
            let mode = normalize_mode(object.get("mode").and_then(Value::as_str));
            let goal = object
                .get("goal")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("完成当前阶段目标")
                .to_string();
            Some(RdWorkflowStageSpec {
                id,
                agent,
                mode,
                goal,
            })
        })
        .collect()
}

pub(super) fn workflow_stage_is_preflight(stage: &RdWorkflowStageSpec) -> bool {
    workflow_stage_kind(stage) == RdWorkflowStageKind::Preflight
}

pub(super) fn workflow_stage_kind(stage: &RdWorkflowStageSpec) -> RdWorkflowStageKind {
    let text =
        format!("{} {} {} {}", stage.id, stage.agent, stage.mode, stage.goal).to_ascii_lowercase();
    if workflow_stage_is_main_implementation(&text, stage) {
        return RdWorkflowStageKind::MainImplementation;
    }
    let positive = [
        "architecture",
        "analysis",
        "scan",
        "understand",
        "design",
        "planning",
        "failure",
        "架构",
        "分析",
        "扫描",
        "理解",
        "设计",
        "规划",
        "定位",
    ];
    let negative = [
        "implementation",
        "coding",
        "patch",
        "modify",
        "review",
        "test",
        "build",
        "rerun",
        "pr",
        "实现",
        "编码",
        "补丁",
        "修改",
        "审查",
        "测试",
        "构建",
        "复测",
    ];
    if positive.iter().any(|needle| text.contains(needle))
        && !negative.iter().any(|needle| text.contains(needle))
    {
        RdWorkflowStageKind::Preflight
    } else {
        RdWorkflowStageKind::Postflight
    }
}

fn workflow_stage_is_main_implementation(text: &str, stage: &RdWorkflowStageSpec) -> bool {
    stage.mode == "modify"
        || [
            "implementation",
            "coding",
            "patch",
            "modify",
            "fix",
            "实现",
            "编码",
            "补丁",
            "修改",
            "修复",
        ]
        .iter()
        .any(|needle| text.contains(needle))
}

pub(super) fn workflow_stage_is_review_like(stage: &RdWorkflowStageSpec) -> bool {
    let text =
        format!("{} {} {} {}", stage.id, stage.agent, stage.mode, stage.goal).to_ascii_lowercase();
    stage.mode == "review"
        || ["review", "audit", "security", "审查", "审核", "安全"]
            .iter()
            .any(|needle| text.contains(needle))
}

fn rd_workflow_post_stage_limit() -> usize {
    std::env::var("AOS_RD_WORKFLOW_POST_STAGE_PASSES")
        .or_else(|_| std::env::var("RD_CODE_WORKFLOW_POST_STAGE_PASSES"))
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value <= 8)
        .unwrap_or(DEFAULT_MAX_RD_WORKFLOW_POST_STAGE_PASSES)
}

pub(super) fn workflow_definition_section(
    workflow: &RdAgentWorkflowDto,
    stages: &[RdWorkflowStageSpec],
    stage_context: &str,
) -> String {
    if stages.is_empty() {
        return format!(
            "\n\n多 Agent 工作流：{}\n该工作流暂无有效阶段定义，请按普通 Coding Agent 任务执行。",
            workflow.name
        );
    }
    let stage_lines = stages
        .iter()
        .enumerate()
        .map(|(index, stage)| {
            format!(
                "{}. {} [{}] - {}",
                index + 1,
                stage.agent,
                stage.mode,
                stage.goal
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let context_section = if stage_context.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n前置工作流阶段输出：\n{}", stage_context.trim())
    };
    format!(
        "\n\n多 Agent 工作流：{}\n说明：{}\n阶段：\n{}{}\n\n请把以上工作流当作任务编排约束：前置阶段用于理解与计划，implementation/patch/modify 阶段由主 Coding Agent 承担，review/test/pr 等后置阶段会在主结果后继续执行；不要绕过 Diff-first 审批策略。",
        workflow.name,
        workflow.description.as_deref().unwrap_or(""),
        stage_lines,
        context_section
    )
}

pub(super) async fn maybe_run_rd_workflow_preflight_stages(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    workflow: &RdAgentWorkflowDto,
    stages: &[RdWorkflowStageSpec],
    mode: &str,
    original_prompt: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    repo_context: &str,
    explicit_file_context: &RdExplicitFileContext,
) -> Result<String, AppError> {
    let preflight_stages = stages
        .iter()
        .filter(|stage| workflow_stage_is_preflight(stage))
        .take(MAX_RD_WORKFLOW_STAGE_PASSES)
        .cloned()
        .collect::<Vec<_>>();

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "workflow",
        if preflight_stages.is_empty() {
            "completed"
        } else {
            "running"
        },
        "已加载多 Agent 工作流",
        json!({
            "workflowId": workflow.id,
            "workflowName": workflow.name,
            "stageCount": stages.len(),
            "preflightStageCount": preflight_stages.len(),
            "preflightLimit": MAX_RD_WORKFLOW_STAGE_PASSES,
            "stages": stages,
        }),
    )
    .await?;

    if preflight_stages.is_empty() {
        return Ok(String::new());
    }

    let explicit_file_section = if explicit_file_context.text.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n用户显式 @ 文件上下文：\n{}",
            explicit_file_context.text.trim()
        )
    };
    let mut outputs = Vec::new();
    for (index, stage) in preflight_stages.iter().enumerate() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "workflow_stage",
            "running",
            &format!("Workflow 阶段 {} 正在执行：{}", index + 1, stage.agent),
            json!({
                "workflowId": workflow.id,
                "stage": stage,
                "stageIndex": index + 1,
            }),
        )
        .await?;

        let stage_prompt = format!(
            "你是 AOS Code Studio 多 Agent 工作流中的一个前置阶段 Agent。\n\n工作流：{}\n当前阶段：{} [{}]\n阶段目标：{}\n总任务模式：{}\n用户需求：\n{}\n\n仓库上下文：\n{}{}\n\n约束：\n- 本阶段只做分析、检索、规划或风险识别，不要改代码，不要生成 Diff。\n- 如果 runtime 工具可用，优先读取真实仓库文件，不要只凭摘要猜。\n- 输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":null,\"prDescription\":null,\"unifiedDiff\":null,\"touchedFiles\":array}}。",
            workflow.name,
            stage.agent,
            stage.mode,
            stage.goal,
            mode,
            original_prompt,
            repo_context,
            explicit_file_section,
        );

        let completion = if rd_runtime_executor_enabled() && repository_id.is_some() {
            run_rd_runtime_completion(
                state,
                claims,
                task_id,
                &stage.mode,
                repository_id,
                model,
                allowed_tools.clone(),
                RdRuntimeSessionPolicy::Transient,
                None,
                stage_prompt,
            )
            .await
        } else {
            run_rd_completion(state, &claims.tenant_id, &claims.sub, model, stage_prompt).await
        };

        match completion {
            Ok(completion) => {
                let parsed = parse_rd_output(&completion.text, &stage.mode);
                let output = format!(
                    "## {}. {} [{}]\n\n### 计划\n{}\n\n### 结论\n{}",
                    index + 1,
                    stage.agent,
                    stage.mode,
                    parsed.plan_md.trim(),
                    parsed.answer_md.trim()
                );
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "workflow_stage",
                    "completed",
                    &format!("Workflow 阶段 {} 已完成：{}", index + 1, stage.agent),
                    json!({
                        "workflowId": workflow.id,
                        "stage": stage,
                        "stageIndex": index + 1,
                        "model": completion.model,
                        "provider": completion.provider,
                        "outputChars": output.chars().count(),
                        "touchedFiles": parsed.touched_files,
                    }),
                )
                .await?;
                outputs.push(output);
            }
            Err(error) => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "workflow_stage",
                    "failed",
                    &format!(
                        "Workflow 阶段 {} 执行失败，已继续主任务：{}",
                        index + 1,
                        stage.agent
                    ),
                    json!({
                        "workflowId": workflow.id,
                        "stage": stage,
                        "stageIndex": index + 1,
                        "error": error.to_string(),
                        "nonBlocking": true,
                    }),
                )
                .await?;
            }
        }
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "workflow",
        "completed",
        "多 Agent 工作流前置阶段已完成",
        json!({
            "workflowId": workflow.id,
            "workflowName": workflow.name,
            "completedPreflightStages": outputs.len(),
        }),
    )
    .await?;
    Ok(outputs.join("\n\n"))
}
pub(super) async fn maybe_run_rd_workflow_postflight_stages(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    workflow: &RdAgentWorkflowDto,
    stages: &[RdWorkflowStageSpec],
    mode: &str,
    original_prompt: &str,
    repository_id: Option<&str>,
    model: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    repo_context: &str,
    explicit_file_context: &RdExplicitFileContext,
    parsed: &mut ParsedRdOutput,
) -> Result<(), AppError> {
    let stage_limit = rd_workflow_post_stage_limit();
    let postflight_stages = stages
        .iter()
        .filter(|stage| workflow_stage_kind(stage) == RdWorkflowStageKind::Postflight)
        .take(stage_limit)
        .cloned()
        .collect::<Vec<_>>();

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "workflow_postflight",
        if postflight_stages.is_empty() {
            "completed"
        } else {
            "running"
        },
        "多 Agent 工作流后置阶段已准备",
        json!({
            "workflowId": workflow.id,
            "workflowName": workflow.name,
            "stageCount": stages.len(),
            "postflightStageCount": postflight_stages.len(),
            "postflightLimit": stage_limit,
        }),
    )
    .await?;

    if postflight_stages.is_empty() {
        return Ok(());
    }

    let explicit_file_section = if explicit_file_context.text.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\n用户显式 @ 文件上下文：\n{}",
            explicit_file_context.text.trim()
        )
    };

    for (index, stage) in postflight_stages.iter().enumerate() {
        record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "workflow_post_stage",
            "running",
            &format!("Workflow 后置阶段 {} 正在执行：{}", index + 1, stage.agent),
            json!({
                "workflowId": workflow.id,
                "stage": stage,
                "stageIndex": index + 1,
            }),
        )
        .await?;

        let diff_section = parsed
            .unified_diff
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|diff| {
                format!(
                    "\n\n主 Agent 生成的待审批 Diff：\n```diff\n{}\n```",
                    truncate_text(diff, 40_000)
                )
            })
            .unwrap_or_else(|| "\n\n主 Agent 未生成 Diff。".to_string());
        let existing_review = parsed
            .review_md
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|review| format!("\n\n已有 Review 内容：\n{}", truncate_text(review, 16_000)))
            .unwrap_or_default();
        let stage_prompt = format!(
            "你是 AOS Code Studio 多 Agent 工作流中的后置阶段 Agent。\n\n工作流：{}\n当前阶段：{} [{}]\n阶段目标：{}\n总任务模式：{}\n用户原始需求：\n{}\n\n仓库上下文：\n{}{}\n\n主 Agent 计划：\n{}\n\n主 Agent 结果：\n{}{}{}\n\n约束：\n- 本阶段用于审查、测试解释、PR 草稿、风险补充或发布前检查，不要直接修改文件，不要提交代码。\n- 不要生成新的 unifiedDiff；如发现必须改动的问题，请在 reviewMd/answerMd 中说明阻塞原因和建议，不要和主 Diff 混在一起。\n- 如果 runtime 工具可用，优先读取真实仓库文件或搜索相关调用链，不要只凭摘要猜。\n- 输出 JSON：{{\"planMd\":string,\"answerMd\":string,\"reviewMd\":string|null,\"prTitle\":string|null,\"prDescription\":string|null,\"unifiedDiff\":null,\"touchedFiles\":array}}。",
            workflow.name,
            stage.agent,
            stage.mode,
            stage.goal,
            mode,
            original_prompt,
            repo_context,
            explicit_file_section,
            truncate_text(&parsed.plan_md, 12_000),
            truncate_text(&parsed.answer_md, 16_000),
            diff_section,
            existing_review,
        );

        let completion = if rd_runtime_executor_enabled() && repository_id.is_some() {
            run_rd_runtime_completion(
                state,
                claims,
                task_id,
                &stage.mode,
                repository_id,
                model,
                allowed_tools.clone(),
                RdRuntimeSessionPolicy::Transient,
                None,
                stage_prompt,
            )
            .await
        } else {
            run_rd_completion(state, &claims.tenant_id, &claims.sub, model, stage_prompt).await
        };

        match completion {
            Ok(completion) => {
                let stage_output = parse_rd_output(&completion.text, &stage.mode);
                let ignored_diff = stage_output
                    .unified_diff
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                let stage_note = format!(
                    "## Workflow 后置阶段 {}. {} [{}]\n\n### 计划\n{}\n\n### 结果\n{}",
                    index + 1,
                    stage.agent,
                    stage.mode,
                    stage_output.plan_md.trim(),
                    stage_output.answer_md.trim()
                );

                if workflow_stage_is_review_like(stage) {
                    let review_text = stage_output
                        .review_md
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(stage_output.answer_md.trim());
                    let review_section = format!(
                        "## Workflow 后置阶段 {}. {} [{}]\n\n{}",
                        index + 1,
                        stage.agent,
                        stage.mode,
                        review_text
                    );
                    parsed.review_md = Some(match parsed.review_md.take() {
                        Some(existing) if !existing.trim().is_empty() => {
                            format!("{}\n\n{}", existing.trim_end(), review_section)
                        }
                        _ => review_section,
                    });
                } else if !stage_note.trim().is_empty() {
                    parsed.answer_md = if parsed.answer_md.trim().is_empty() {
                        stage_note
                    } else {
                        format!("{}\n\n{}", parsed.answer_md.trim_end(), stage_note)
                    };
                }

                if parsed.pr_title.is_none() {
                    parsed.pr_title = stage_output.pr_title;
                }
                if parsed.pr_description.is_none() {
                    parsed.pr_description = stage_output.pr_description;
                }

                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "workflow_post_stage",
                    "completed",
                    &format!("Workflow 后置阶段 {} 已完成：{}", index + 1, stage.agent),
                    json!({
                        "workflowId": workflow.id,
                        "stage": stage,
                        "stageIndex": index + 1,
                        "model": completion.model,
                        "provider": completion.provider,
                        "outputChars": completion.text.chars().count(),
                        "touchedFiles": stage_output.touched_files,
                        "ignoredDiff": ignored_diff,
                    }),
                )
                .await?;
            }
            Err(error) => {
                record_event(
                    &state.db,
                    &claims.tenant_id,
                    task_id,
                    "workflow_post_stage",
                    "failed",
                    &format!(
                        "Workflow 后置阶段 {} 执行失败，已保留主任务结果：{}",
                        index + 1,
                        stage.agent
                    ),
                    json!({
                        "workflowId": workflow.id,
                        "stage": stage,
                        "stageIndex": index + 1,
                        "error": error.to_string(),
                        "nonBlocking": true,
                    }),
                )
                .await?;
            }
        }
    }

    record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "workflow_postflight",
        "completed",
        "多 Agent 工作流后置阶段已完成",
        json!({
            "workflowId": workflow.id,
            "workflowName": workflow.name,
            "completedPostflightStages": postflight_stages.len(),
        }),
    )
    .await?;
    Ok(())
}
