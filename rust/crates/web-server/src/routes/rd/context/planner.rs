//! Context profile, deterministic plans, and LLM context planner.

use super::*;

pub(in crate::routes::rd) use rd_core::context_planner::{
    build_rd_context_plan_section, build_rd_context_policy_section, default_rd_context_depth,
    normalize_rd_context_depth, normalize_rd_profile_for_mode, rd_context_budget_json,
};

pub(in crate::routes::rd) fn resolve_rd_task_context_strategy(
    mode: &str,
    prompt: &str,
    requested_profile: Option<&str>,
    requested_depth: Option<&str>,
    requested_should_deep_scan: Option<bool>,
    parent_task: Option<&RdTaskDto>,
) -> RdTaskContextStrategy {
    let parent_profile = parent_task
        .and_then(|task| task.context_profile.as_deref())
        .and_then(RdContextProfile::from_str);
    let mut profile = requested_profile
        .and_then(RdContextProfile::from_str)
        .or(parent_profile)
        .unwrap_or_else(|| RdContextProfile::from_task(mode, prompt));
    profile = normalize_rd_profile_for_mode(profile, mode);

    let mut should_deep_scan = requested_should_deep_scan
        .or_else(|| parent_task.map(|task| task.should_deep_scan))
        .unwrap_or(profile == RdContextProfile::DeepReview);
    if should_deep_scan && mode == "review" {
        profile = RdContextProfile::DeepReview;
    }
    if profile == RdContextProfile::DeepReview {
        should_deep_scan = true;
    }

    let inherited_depth = parent_task
        .and_then(|task| task.context_depth.as_deref())
        .filter(|depth| !depth.trim().is_empty());
    let depth = normalize_rd_context_depth(
        requested_depth.or(inherited_depth),
        profile,
        should_deep_scan,
    );
    if depth == "deep" && mode == "review" {
        should_deep_scan = true;
        profile = RdContextProfile::DeepReview;
    }

    RdTaskContextStrategy {
        profile,
        depth,
        should_deep_scan,
    }
}
#[allow(clippy::too_many_arguments)]
pub(in crate::routes::rd) async fn maybe_run_rd_llm_context_planner(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    mode: &str,
    prompt: &str,
    repository_id: Option<&str>,
    selected_model: Option<&str>,
    context_profile: RdContextProfile,
    deterministic_context_plan_section: &str,
    repo_context: &str,
    explicit_file_context: &RdExplicitFileContext,
    has_workflow: bool,
    workflow_stage_count: usize,
) -> Option<RdLlmContextPlan> {
    if !rd_llm_context_planner_enabled() || repository_id.is_none() {
        return None;
    }
    let repository_id = repository_id.unwrap_or_default();
    let _ = record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "context_plan_llm",
        "running",
        "LLM Context Planner 正在生成效果优先的上下文读取路线",
        json!({
            "repositoryId": repository_id,
            "mode": mode,
            "contextProfile": context_profile.as_str(),
        }),
    )
    .await;

    let planner_input = json!({
        "mode": mode,
        "deterministicProfile": context_profile.as_str(),
        "deterministicProfileName": context_profile.display_name(),
        "repositoryId": repository_id,
        "hasExplicitFiles": !explicit_file_context.files.is_empty(),
        "explicitFiles": &explicit_file_context.files,
        "hasWorkflow": has_workflow,
        "workflowStageCount": workflow_stage_count,
        "userPrompt": truncate_text(prompt, 8_000),
        "deterministicPlan": deterministic_context_plan_section,
        "availableContextPreview": truncate_text(repo_context, 14_000),
    });
    let planner_prompt = format!(
        r#"你是 AOS Code Studio 的 LLM Context Planner。你只负责规划上下文读取路线，不回答用户问题，不生成代码，不生成 Diff。

核心目标：
- 效果第一：不要为了节省 token 牺牲正确性。
- 通过索引召回、摘要、关键文件核对、渐进读取减少盲目全仓扫描。
- 判断这次任务是否应该是轻量概览、定向问答、报错解释、代码修改、审查或深度审计。
- 给主 Coding Agent 明确：优先读哪些文件/路径，先搜哪些词，分几步扩大范围，何时可以停止。
- 如果需要深度扫描，必须说明触发条件；没有必要时不要把概览/启动/架构问题升级成逐文件扫描。
- 摘要和召回只能作为候选入口，关键结论仍要读取真实文件核对。

只能返回 JSON，字段如下：
{{
  "profile": "overview|focused_ask|explain|modify|review|deep_review",
  "depth": "shallow|standard|deep",
  "shouldDeepScan": false,
  "priorityFiles": ["README.md", "package.json"],
  "searchTerms": ["router", "api"],
  "stages": ["第一步...", "第二步..."],
  "stopConditions": ["证据足够时停止..."],
  "reasoning": "一句话解释为什么这样规划",
  "confidence": 0.0-1.0
}}

输入：
```json
{}
```"#,
        serde_json::to_string_pretty(&planner_input).unwrap_or_else(|_| planner_input.to_string())
    );

    let planned = timeout(
        Duration::from_secs(RD_LLM_CONTEXT_PLANNER_TIMEOUT_SECS),
        run_rd_completion_with_options(
            state,
            &claims.tenant_id,
            &claims.sub,
            selected_model,
            planner_prompt,
            "You are the context planner for a coding agent. Return compact JSON only. Do not solve the coding task.".to_string(),
            RD_LLM_CONTEXT_PLANNER_MAX_TOKENS,
            Some(0.0),
        ),
    )
    .await;

    let completion = match planned {
        Ok(Ok(completion)) => completion,
        Ok(Err(error)) => {
            let error_text = truncate_text(&error.to_string(), 800);
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                task_id = %task_id,
                error = %error_text,
                "RD LLM context planner failed; using deterministic plan"
            );
            let _ = record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "context_plan_llm",
                "skipped",
                "LLM Context Planner 失败，已使用确定性上下文计划",
                json!({"repositoryId": repository_id, "error": error_text}),
            )
            .await;
            return None;
        }
        Err(_) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                task_id = %task_id,
                "RD LLM context planner timed out; using deterministic plan"
            );
            let _ = record_event(
                &state.db,
                &claims.tenant_id,
                task_id,
                "context_plan_llm",
                "skipped",
                "LLM Context Planner 超时，已使用确定性上下文计划",
                json!({
                    "repositoryId": repository_id,
                    "timeoutSecs": RD_LLM_CONTEXT_PLANNER_TIMEOUT_SECS,
                }),
            )
            .await;
            return None;
        }
    };

    let Some(plan) = parse_rd_llm_context_plan(&completion, mode, context_profile) else {
        let _ = record_event(
            &state.db,
            &claims.tenant_id,
            task_id,
            "context_plan_llm",
            "skipped",
            "LLM Context Planner 返回格式无效，已使用确定性上下文计划",
            json!({
                "repositoryId": repository_id,
                "model": completion.model,
                "output": truncate_text(&completion.text, 800),
            }),
        )
        .await;
        return None;
    };

    let _ = record_event(
        &state.db,
        &claims.tenant_id,
        task_id,
        "context_plan_llm",
        "completed",
        "LLM Context Planner 已生成上下文读取路线",
        rd_llm_context_plan_json(&plan, repository_id, completion.usage.as_ref()),
    )
    .await;
    Some(plan)
}

fn parse_rd_llm_context_plan(
    completion: &RdCompletionResult,
    mode: &str,
    fallback_profile: RdContextProfile,
) -> Option<RdLlmContextPlan> {
    let value = parse_json_from_model_output(&completion.text)?;
    let raw_profile = value
        .get("profile")
        .or_else(|| value.get("contextProfile"))
        .or_else(|| value.get("context_profile"))
        .and_then(Value::as_str)
        .and_then(RdContextProfile::from_str)
        .unwrap_or(fallback_profile);
    let profile = normalize_rd_profile_for_mode(raw_profile, mode);
    let should_deep_scan = value
        .get("shouldDeepScan")
        .or_else(|| value.get("should_deep_scan"))
        .and_then(Value::as_bool)
        .unwrap_or(profile == RdContextProfile::DeepReview);
    let depth = normalize_rd_context_depth(
        value
            .get("depth")
            .or_else(|| value.get("contextDepth"))
            .or_else(|| value.get("context_depth"))
            .and_then(Value::as_str),
        profile,
        should_deep_scan,
    );
    let priority_files = json_string_array(
        &value,
        &["priorityFiles", "priority_files", "files", "paths"],
        12,
        240,
    );
    let search_terms = json_string_array(
        &value,
        &["searchTerms", "search_terms", "terms", "queries"],
        16,
        120,
    );
    let stages = json_string_array(&value, &["stages", "steps"], 8, 260);
    let stop_conditions = json_string_array(
        &value,
        &["stopConditions", "stop_conditions", "stop"],
        8,
        260,
    );
    let reasoning = value
        .get("reasoning")
        .or_else(|| value.get("reason"))
        .or_else(|| value.get("rationale"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| truncate_text(reason, 360));
    let confidence = value
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|v| v.clamp(0.0, 1.0) as f32)
        .unwrap_or(0.72);
    Some(RdLlmContextPlan {
        profile,
        depth,
        should_deep_scan,
        priority_files,
        search_terms,
        stages,
        stop_conditions,
        reasoning,
        confidence,
        model: completion.model.clone(),
        provider: completion.provider.clone(),
    })
}

pub(in crate::routes::rd) fn build_rd_llm_context_plan_section(plan: &RdLlmContextPlan) -> String {
    let mut lines = vec![
        "## LLM Context Planner（效果优先）".to_string(),
        format!(
            "- 建议 profile：{}（{}），深度：{}，需要深度扫描：{}，置信度：{:.2}",
            plan.profile.as_str(),
            plan.profile.display_name(),
            plan.depth,
            plan.should_deep_scan,
            plan.confidence
        ),
    ];
    if let Some(reasoning) = plan.reasoning.as_deref() {
        lines.push(format!("- 规划理由：{reasoning}"));
    }
    if !plan.priority_files.is_empty() {
        lines.push(format!(
            "- 优先核对文件/路径：{}",
            plan.priority_files
                .iter()
                .map(|file| format!("`{file}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !plan.search_terms.is_empty() {
        lines.push(format!("- 优先搜索词：{}", plan.search_terms.join(", ")));
    }
    if !plan.stages.is_empty() {
        lines.push(format!("- 建议阶段：\n  - {}", plan.stages.join("\n  - ")));
    }
    if !plan.stop_conditions.is_empty() {
        lines.push(format!(
            "- 停止/升级条件：\n  - {}",
            plan.stop_conditions.join("\n  - ")
        ));
    }
    lines.push(
        "- 执行要求：先按上述路线渐进读取；如果真实文件证据与摘要/召回冲突，以真实文件为准。证据足够时停止继续搜索；证据不足时说明缺口并再扩大范围。"
            .to_string(),
    );
    lines.join("\n")
}

fn rd_llm_context_plan_json(
    plan: &RdLlmContextPlan,
    repository_id: &str,
    usage: Option<&RdTokenUsageSnapshot>,
) -> Value {
    json!({
        "repositoryId": repository_id,
        "source": "llm",
        "model": &plan.model,
        "provider": &plan.provider,
        "profile": plan.profile.as_str(),
        "profileName": plan.profile.display_name(),
        "depth": &plan.depth,
        "shouldDeepScan": plan.should_deep_scan,
        "priorityFiles": &plan.priority_files,
        "searchTerms": &plan.search_terms,
        "stages": &plan.stages,
        "stopConditions": &plan.stop_conditions,
        "reasoning": &plan.reasoning,
        "confidence": plan.confidence,
        "usage": usage,
    })
}

fn json_string_array(
    value: &Value,
    keys: &[&str],
    limit: usize,
    max_item_chars: usize,
) -> Vec<String> {
    let Some(array) = keys
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
    else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for item in array {
        let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) else {
            continue;
        };
        let text = truncate_text(text, max_item_chars);
        if seen.insert(text.to_ascii_lowercase()) {
            output.push(text);
        }
        if output.len() >= limit {
            break;
        }
    }
    output
}
