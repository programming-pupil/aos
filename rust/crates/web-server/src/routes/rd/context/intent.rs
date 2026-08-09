//! Intent routing for RD tasks.

use super::*;

pub(in crate::routes::rd) async fn route_rd_task_intent(
    state: &AppState,
    claims: &Claims,
    prompt: &str,
    selected_model: Option<&str>,
) -> RdIntentRouteResponse {
    let fallback = fallback_rd_task_intent(prompt);
    let fallback_profile = RdContextProfile::from_task(&fallback, prompt);
    let fallback_decision = RdIntentRouteDecision {
        mode: fallback.clone(),
        confidence: 0.45,
        reason: Some("used local fallback".to_string()),
        profile: fallback_profile,
        depth: default_rd_context_depth(fallback_profile).to_string(),
        should_deep_scan: fallback_profile == RdContextProfile::DeepReview,
    };
    let route_prompt = build_rd_intent_route_prompt(prompt);
    let routed = timeout(
        Duration::from_secs(120),
        run_rd_completion_with_options(
            state,
            &claims.tenant_id,
            &claims.sub,
            selected_model,
            route_prompt,
            "You are a precise intent router for AOS Code Studio. Return JSON only. Do not answer the user's coding request.".to_string(),
            300,
            Some(0.0),
        ),
    )
    .await;

    let completion = match routed {
        Ok(Ok(completion)) => completion,
        Ok(Err(error)) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                error = %error,
                "RD intent route LLM failed; using heuristic fallback"
            );
            let mut decision = fallback_decision.clone();
            decision.reason = Some("LLM route failed; used local fallback".to_string());
            return rd_intent_route_response(
                decision,
                "fallback",
                selected_model.map(ToOwned::to_owned),
            );
        }
        Err(_) => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                "RD intent route LLM timed out; using heuristic fallback"
            );
            let mut decision = fallback_decision.clone();
            decision.confidence = 0.4;
            decision.reason = Some("LLM route timed out; used local fallback".to_string());
            return rd_intent_route_response(
                decision,
                "fallback",
                selected_model.map(ToOwned::to_owned),
            );
        }
    };

    match parse_rd_intent_route_output(&completion.text, prompt) {
        Some(decision) => {
            if decision.confidence < 0.35 {
                let mut fallback_decision = fallback_decision.clone();
                fallback_decision.confidence = decision.confidence;
                fallback_decision.reason =
                    Some("LLM route confidence was too low; used local fallback".to_string());
                return rd_intent_route_response(
                    fallback_decision,
                    "fallback",
                    Some(completion.model),
                );
            }
            rd_intent_route_response(decision, "llm", Some(completion.model))
        }
        None => {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                model = %completion.model,
                output_chars = completion.text.chars().count(),
                "RD intent route returned invalid JSON; using heuristic fallback"
            );
            let mut decision = fallback_decision;
            decision.reason = Some("LLM route output was invalid; used local fallback".to_string());
            rd_intent_route_response(decision, "fallback", Some(completion.model))
        }
    }
}

fn build_rd_intent_route_prompt(prompt: &str) -> String {
    format!(
        r#"Classify the user's AOS Code Studio request into exactly one task mode.

Modes:
- ask: answer repository/project questions, explain architecture, provide guidance, no code diff requested.
- modify: implement, fix, refactor, optimize, add support, or otherwise produce a code diff/patch.
- explain: explain an error/log/stack trace/failure and suggest diagnosis or steps; choose modify only if the user clearly asks to change code.
- review: code review, audit, find bugs/risks/regressions/missing tests/security issues across files or project.

Decision guidance:
- Decide from the user's intent, not from keyword matching. The examples below are only references, not hard rules.
- Choose the mode that will make the coding agent most useful and safe for the actual request.
- Example modify intents: "fix", "implement", "support", "change", "refactor", "修复", "实现", "接入", "优化". Similar requests that imply changing code should also be modify even if these words are absent.
- Example review intents: "review", "audit", "risk check", "巡检", "找问题", "安全审计". Similar requests asking to inspect quality, risks, bugs, or missing tests should also be review.
- Example explain intents: pasted errors, logs, stack traces, failed commands, or "why did this happen". If the user clearly asks for a code change, choose modify instead.
- Example ask intents: architecture questions, startup instructions, "how does this work", API usage, project explanation, or guidance without asking for a diff.
- If multiple modes are plausible, choose the mode that best satisfies the user's practical goal and report an honest confidence.
- If the request is truly ambiguous, choose ask with lower confidence.

Return JSON only:
{{"mode":"ask|modify|explain|review","profile":"overview|focused_ask|explain|modify|review|deep_review","depth":"shallow|standard|deep","shouldDeepScan":false,"confidence":0.0-1.0,"reason":"short reason"}}

User request:
<<<
{}
>>>"#,
        truncate_text(prompt.trim(), 8_000)
    )
}

fn parse_rd_intent_route_output(raw: &str, prompt: &str) -> Option<RdIntentRouteDecision> {
    let value = parse_json_from_model_output(raw)?;
    let mode = value
        .get("mode")
        .or_else(|| value.get("intent"))
        .or_else(|| value.get("taskMode"))
        .or_else(|| value.get("task_mode"))
        .and_then(Value::as_str)
        .map(|mode| normalize_mode(Some(mode)))?;
    if !matches!(mode.as_str(), "ask" | "modify" | "explain" | "review") {
        return None;
    }
    let mut profile = value
        .get("profile")
        .or_else(|| value.get("contextProfile"))
        .or_else(|| value.get("context_profile"))
        .and_then(Value::as_str)
        .and_then(RdContextProfile::from_str)
        .unwrap_or_else(|| RdContextProfile::from_task(&mode, prompt));
    profile = normalize_rd_profile_for_mode(profile, &mode);
    let confidence = value
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|v| v.clamp(0.0, 1.0) as f32)
        .unwrap_or(0.7);
    let reason = value
        .get("reason")
        .or_else(|| value.get("rationale"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| truncate_text(reason, 200));
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
    Some(RdIntentRouteDecision {
        mode,
        confidence,
        reason,
        profile,
        depth,
        should_deep_scan,
    })
}

fn rd_intent_route_response(
    decision: RdIntentRouteDecision,
    source: &str,
    model: Option<String>,
) -> RdIntentRouteResponse {
    RdIntentRouteResponse {
        mode: decision.mode,
        confidence: decision.confidence,
        reason: decision.reason,
        source: source.to_string(),
        model,
        profile: decision.profile.as_str().to_string(),
        profile_name: decision.profile.display_name().to_string(),
        depth: decision.depth,
        should_deep_scan: decision.should_deep_scan,
    }
}

fn fallback_rd_task_intent(prompt: &str) -> String {
    let text = prompt.trim().to_lowercase();
    if text.is_empty() {
        return "ask".to_string();
    }
    if contains_any(
        &text,
        &[
            "修复",
            "新增",
            "实现",
            "修改",
            "改成",
            "删除",
            "重构",
            "接入",
            "支持",
            "优化",
            "补齐",
            "开发",
            "生成diff",
            "生成 diff",
            "patch",
            "fix",
            "implement",
            "change",
            "refactor",
            "optimize",
        ],
    ) {
        return "modify".to_string();
    }
    if contains_any(
        &text,
        &[
            "代码审查",
            "code review",
            "review",
            "审查",
            "找出问题",
            "找问题",
            "检查所有问题",
            "风险",
            "缺失测试",
            "安全审计",
            "全站巡检",
        ],
    ) {
        return "review".to_string();
    }
    if contains_any(
        &text,
        &[
            "解释报错",
            "报错",
            "错误",
            "异常",
            "堆栈",
            "日志",
            "失败",
            "为什么",
            "error",
            "exception",
            "stack trace",
            "traceback",
            "panic",
            "failed",
        ],
    ) {
        return "explain".to_string();
    }
    "ask".to_string()
}
