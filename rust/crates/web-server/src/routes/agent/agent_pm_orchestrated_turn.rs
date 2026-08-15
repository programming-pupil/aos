use super::*;
use pm_domain::probe_plan::{
    classify_pm_source_exhaustion_reason, pm_source_exhaustion_reason_code,
};
use pm_domain::query_hygiene::sanitize_pm_search_queries;
use pm_domain::route_plan::pm_question_likely_requires_external_evidence;
type PmStageCallback<'a> = dyn FnMut(&str, &str, usize, Option<serde_json::Value>) + Send + 'a;

struct PmPreparedOrchestrationPlan {
    plan: serde_json::Value,
    runtime_budget: PmTimeoutBudget,
    resume_detail: Option<serde_json::Value>,
    resume_skip_planner: bool,
    resume_attempt: usize,
}

fn guard_pm_report_strategy_route(
    mut route: PmTurnRoute,
    plan: &mut serde_json::Value,
    user_message: &str,
) -> PmTurnRoute {
    let signal = detect_pm_report_strategy_signal(user_message);
    if signal.matched {
        return route;
    }

    if matches!(route.turn_class, PmTurnClass::PmReportStrategy) {
        route.turn_class = PmTurnClass::PmStrategy;
        route.reason = format!(
            "{}; report_strategy_rejected_without_first_party_metric_evidence",
            route.reason.trim()
        );
    }
    if let Some(obj) = plan.as_object_mut() {
        if obj
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("business_report_strategy"))
        {
            obj.insert("mode".to_string(), serde_json::json!("auto"));
        }
        obj.remove("reportStrategy");
    }
    route
}

fn normalized_pm_plan_query_variants(plan: &serde_json::Value, user_message: &str) -> Vec<String> {
    let planned = plan
        .get("queryVariants")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if planned.is_empty() {
        return build_pm_query_variants(user_message);
    }
    sanitize_pm_search_queries(planned, Some(user_message), 5)
}

fn emit_pm_answer_snapshot(answer_delta: Option<&PmAnswerDeltaCallback>, stage: &str, text: &str) {
    let Some(answer_delta) = answer_delta else {
        return;
    };
    let visible = extract_pm_visible_answer_text(text);
    let visible = visible.trim();
    if visible.is_empty() {
        return;
    }
    let mut buf = String::new();
    for ch in visible.chars() {
        buf.push(ch);
        if buf.len() >= 384 {
            answer_delta(stage, std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        answer_delta(stage, buf);
    }
}

fn pm_shared_chat_turn_options(search_enabled: bool) -> ChatTurnOptions {
    ChatTurnOptions {
        search_mode: if search_enabled {
            ChatSearchMode::On
        } else {
            ChatSearchMode::Off
        },
        search_enabled,
        file_context: Some(ChatFileContextOptions {
            mode: ChatFileContextMode::None,
            file_ids: Vec::new(),
            strict_grounding: false,
        }),
        memory_mode: ChatMemoryMode::Auto,
    }
}

fn pm_shared_chat_turn_options_for_message(
    search_enabled: bool,
    user_message: &str,
) -> ChatTurnOptions {
    let mut options = pm_shared_chat_turn_options(search_enabled);
    if pm_user_message_has_document_context(user_message) {
        options.file_context = Some(ChatFileContextOptions {
            mode: ChatFileContextMode::AllAttached,
            file_ids: Vec::new(),
            strict_grounding: false,
        });
    }
    options
}

fn pm_user_message_has_document_context(user_message: &str) -> bool {
    user_message.contains("[附件文档上下文]") && user_message.contains("[/附件文档上下文]")
}

fn pm_route_should_use_shared_chat_engine(route: &PmTurnRoute) -> bool {
    !matches!(route.engine, PmRouteEngine::AosDeepResearch)
}

fn pm_effective_preface_turn_timeout_secs(_plan: &serde_json::Value) -> u64 {
    pm_preface_turn_timeout_secs()
}

fn pm_plan_has_external_probe_subtask(plan: &serde_json::Value) -> bool {
    plan.get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
        .map(|subtasks| {
            subtasks.iter().any(|subtask| {
                let required = subtask
                    .get("requiredEvidenceType")
                    .or_else(|| subtask.get("required_evidence_type"))
                    .or_else(|| subtask.get("evidenceType"))
                    .or_else(|| subtask.get("evidence_type"))
                    .and_then(|value| value.as_str());
                if required.is_some() {
                    return pm_domain::task_graph::pm_subtask_allows_external_probe(required);
                }
                let queries = subtask
                    .get("queries")
                    .and_then(|value| value.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str())
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let inferred = pm_domain::task_graph::infer_pm_subtask_required_evidence_type(
                    subtask.get("title").and_then(|value| value.as_str()),
                    subtask.get("goal").and_then(|value| value.as_str()),
                    subtask.get("deliverable").and_then(|value| value.as_str()),
                    &queries,
                );
                pm_domain::task_graph::pm_subtask_allows_external_probe(Some(&inferred))
            })
        })
        .unwrap_or(false)
}

fn pm_plan_should_synthesize_without_external_retrieval(
    plan: &serde_json::Value,
    user_message: &str,
) -> bool {
    if pm_question_likely_requires_external_evidence(user_message) {
        return false;
    }
    let Some(task_graph) = plan.get("taskGraph") else {
        return false;
    };
    let subtask_count = task_graph
        .get("subtasks")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    if subtask_count == 0 {
        return false;
    }
    if pm_plan_has_external_probe_subtask(plan) {
        return false;
    }
    let route = pm_plan_turn_route(plan);
    route
        .as_ref()
        .is_some_and(|route| route.is_pm_deep_strategy() || route.complexity_score >= 60)
}

fn pm_retrieval_variant_key(raw: &str) -> Option<String> {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = compact
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
        .to_ascii_lowercase();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

fn pm_retrieval_route_key(route_id: Option<&str>, route_channel: Option<&str>) -> String {
    pm_route_usage_key(route_id, route_channel).unwrap_or_else(|| "route:auto".to_string())
}

fn pm_retrieval_attempt_key(
    variant: Option<&str>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
) -> Option<String> {
    let variant_key = pm_retrieval_variant_key(variant?)?;
    Some(format!(
        "{}|{}",
        variant_key,
        pm_retrieval_route_key(route_id, route_channel)
    ))
}

fn pm_probe_candidate_route_parts(
    candidate: &PmProbeCandidate,
) -> (Option<&str>, Option<&str>, Option<&str>) {
    let route_id = candidate
        .route
        .as_ref()
        .and_then(|route| route.get("routeId"))
        .and_then(|value| value.as_str());
    let route_channel = candidate
        .route
        .as_ref()
        .and_then(|route| route.get("channel"))
        .and_then(|value| value.as_str());
    let execution_channel = candidate
        .route
        .as_ref()
        .and_then(|route| route.get("executionChannel"))
        .and_then(|value| value.as_str());
    (route_id, route_channel, execution_channel)
}

fn pm_probe_candidate_attempt_key(candidate: &PmProbeCandidate) -> Option<String> {
    let (route_id, route_channel, _) = pm_probe_candidate_route_parts(candidate);
    pm_retrieval_attempt_key(Some(candidate.variant.as_str()), route_id, route_channel)
}

fn pm_probe_candidate_subtask_key(candidate: &PmProbeCandidate) -> Option<String> {
    for value in [
        candidate.subtask_key.as_deref(),
        candidate.subtask_id.as_deref(),
        candidate.subtask_title.as_deref(),
    ] {
        let normalized = value.map(normalize_claim_key).unwrap_or_default();
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    None
}

fn pm_select_adaptive_probe_wave(
    candidates: Vec<PmProbeCandidate>,
    attempt: usize,
    candidate_cap: usize,
    focused_repair: bool,
    repair_cap: usize,
) -> Vec<PmProbeCandidate> {
    let candidate_cap = candidate_cap.max(1);
    if attempt > 1 {
        let cap = if focused_repair {
            repair_cap.max(1)
        } else {
            repair_cap.max(1).min(2)
        }
        .min(candidate_cap);
        return candidates.into_iter().take(cap).collect();
    }

    let mut selected = Vec::new();
    let mut seen_subtasks = HashSet::<String>::new();
    let mut unscoped = Vec::new();
    for candidate in candidates {
        let Some(key) = pm_probe_candidate_subtask_key(&candidate) else {
            unscoped.push(candidate);
            continue;
        };
        if seen_subtasks.insert(key) {
            selected.push(candidate);
            if selected.len() >= candidate_cap {
                break;
            }
        }
    }
    if selected.is_empty() {
        return unscoped.into_iter().take(candidate_cap).collect();
    }
    selected
}

fn pm_probe_kernel_should_run(
    enabled: bool,
    attempt: usize,
    max_attempts: usize,
    candidate_count: usize,
) -> bool {
    enabled && attempt <= max_attempts && candidate_count > 0
}

fn pm_probe_progress_message(completed: usize, total: usize) -> String {
    if total == 1 {
        format!("正在定向补齐薄弱维度（{completed}/{total}）")
    } else {
        format!("正在并行检索多个研究维度（{completed}/{total}）")
    }
}

fn pm_filter_fresh_probe_candidates(
    candidates: Vec<PmProbeCandidate>,
    used_retrieval_keys: &HashSet<String>,
) -> (Vec<PmProbeCandidate>, usize) {
    let mut seen_this_batch = HashSet::<String>::new();
    let mut skipped = 0usize;
    let mut fresh = Vec::new();
    for candidate in candidates {
        let Some(key) = pm_probe_candidate_attempt_key(&candidate) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        if used_retrieval_keys.contains(&key) || !seen_this_batch.insert(key) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        fresh.push(candidate);
    }
    (fresh, skipped)
}

fn pm_mark_probe_candidates_used(
    candidates: &[PmProbeCandidate],
    used_retrieval_keys: &mut HashSet<String>,
    used_retrieval_variants: &mut HashSet<String>,
) {
    for candidate in candidates {
        if let Some(key) = pm_probe_candidate_attempt_key(candidate) {
            used_retrieval_keys.insert(key);
        }
        if let Some(variant_key) = pm_retrieval_variant_key(&candidate.variant) {
            used_retrieval_variants.insert(variant_key);
        }
    }
}

fn pm_mark_selected_retrieval_used(
    variant: Option<&str>,
    route_id: Option<&str>,
    route_channel: Option<&str>,
    used_retrieval_keys: &mut HashSet<String>,
    used_retrieval_variants: &mut HashSet<String>,
) {
    if let Some(key) = pm_retrieval_attempt_key(variant, route_id, route_channel) {
        used_retrieval_keys.insert(key);
    }
    if let Some(variant_key) = variant.and_then(pm_retrieval_variant_key) {
        used_retrieval_variants.insert(variant_key);
    }
}

fn pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
    query_variants: &[String],
    enabled_routes: &[PmEnabledRoute],
    attempt: usize,
    route_usage_counts: &HashMap<String, usize>,
    route_blocklist: &HashSet<String>,
    max_calls_per_source: usize,
    used_retrieval_keys: &HashSet<String>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
) {
    let variant_count = query_variants.len().max(1);
    let route_count = enabled_routes.len().max(1);
    let scan_limit = variant_count
        .saturating_mul(route_count)
        .saturating_add(variant_count)
        .saturating_add(route_count)
        .max(1);
    let mut last_choice: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
    ) = (None, None, None, None, false);
    for offset in 0..scan_limit {
        let virtual_attempt = attempt.saturating_add(offset).max(1);
        let choice = pick_pm_attempt_preferences_with_source_quota_and_blocked(
            query_variants,
            enabled_routes,
            virtual_attempt,
            route_usage_counts,
            route_blocklist,
            max_calls_per_source,
        );
        last_choice = choice.clone();
        let (variant, route_id, route_channel, _, exhausted) = &choice;
        if *exhausted {
            return choice;
        }
        let Some(key) = pm_retrieval_attempt_key(
            variant.as_deref(),
            route_id.as_deref(),
            route_channel.as_deref(),
        ) else {
            return choice;
        };
        if !used_retrieval_keys.contains(&key) {
            return choice;
        }
    }

    (
        last_choice.0,
        last_choice.1,
        last_choice.2,
        last_choice.3,
        true,
    )
}

fn pm_variant_has_fresh_route(
    variant: &str,
    enabled_routes: &[PmEnabledRoute],
    route_usage_counts: &HashMap<String, usize>,
    route_blocklist: &HashSet<String>,
    max_calls_per_source: usize,
    used_retrieval_keys: &HashSet<String>,
) -> bool {
    if enabled_routes.is_empty() {
        return pm_retrieval_attempt_key(Some(variant), None, None)
            .is_some_and(|key| !used_retrieval_keys.contains(&key));
    }

    enabled_routes.iter().any(|route| {
        if is_pm_route_over_quota(
            route_usage_counts,
            Some(route.route_id.as_str()),
            Some(route.channel.as_str()),
            max_calls_per_source,
        ) || is_pm_route_blocked(
            route_blocklist,
            Some(route.route_id.as_str()),
            Some(route.channel.as_str()),
        ) {
            return false;
        }
        pm_retrieval_attempt_key(
            Some(variant),
            Some(route.route_id.as_str()),
            Some(route.channel.as_str()),
        )
        .is_some_and(|key| !used_retrieval_keys.contains(&key))
    })
}

fn pm_coverage_repair_is_actionable(
    coverage_gap_present: bool,
    repair_plan_enabled: bool,
    repair_target_selected: bool,
    fresh_target_route_available: bool,
    attempt: usize,
    max_attempts: usize,
) -> bool {
    coverage_gap_present
        && repair_plan_enabled
        && repair_target_selected
        && fresh_target_route_available
        && attempt < max_attempts
}

fn pm_deep_loop_should_finish_attempt(
    base_should_finish: bool,
    targeted_repair_actionable: bool,
    convergence_required: bool,
) -> bool {
    base_should_finish && (!targeted_repair_actionable || convergence_required)
}

fn pm_probe_outcomes_confirm_no_retrieval_route(outcomes: &[PmProbeOutcome]) -> bool {
    !outcomes.is_empty()
        && outcomes.iter().all(|outcome| {
            if outcome.turn.is_some() {
                return false;
            }
            let Some(error) = outcome.error.as_deref() else {
                return false;
            };
            error.contains("used_layer=none")
                && error.contains("native_attempts=0")
                && error.contains("mcp_attempts=0")
                && error.contains("configured_provider_attempts=0")
                && error.contains("rag_local_attempts=0")
        })
}

fn pm_probe_outcomes_confirm_retrieval_discovery_exhausted(outcomes: &[PmProbeOutcome]) -> bool {
    !outcomes.is_empty()
        && outcomes.iter().all(|outcome| {
            outcome.turn.is_none()
                && outcome
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("used_layer=none"))
        })
}

fn pm_route_can_skip_execution_contracts(route: &PmTurnRoute, user_message: &str) -> bool {
    let _ = user_message;
    !matches!(route.engine, PmRouteEngine::AosDeepResearch)
}

fn build_pm_deterministic_exec_constraints(
    plan: &serde_json::Value,
    runtime_budget: &PmTimeoutBudget,
) -> PmExecConstraints {
    let mut route_ids = plan
        .get("sourceRoutes")
        .and_then(serde_json::Value::as_array)
        .map(|routes| {
            routes
                .iter()
                .filter(|route| {
                    route
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true)
                })
                .filter_map(|route| route.get("routeId").and_then(serde_json::Value::as_str))
                .map(str::trim)
                .filter(|route_id| !route_id.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    route_ids.sort();
    route_ids.dedup();
    route_ids.truncate(20);
    if route_ids.is_empty() {
        route_ids.push("web.search.general".to_string());
    }
    PmExecConstraints::new(
        route_ids.clone(),
        route_ids,
        runtime_budget.source_slot_search_secs.max(1),
        runtime_budget.retrieve_max_tool_calls.clamp(1, 12),
        runtime_budget.pipeline_timeout_secs.max(1),
    )
}

fn pm_lightweight_chat_system_instruction(route: &PmTurnRoute) -> String {
    format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
PM_LIGHTWEIGHT_CHAT_ENGINE_ROUTE: PmTurnRouter selected a non-deep route for this turn.\n\
Rules:\n\
- Answer the user's actual question naturally in the same language.\n\
- Do not force product/operations framing when the user asks a general question.\n\
- Do not expose routing metadata, debug JSON, tool latency, or internal trace fields.\n\
- If the user explicitly asked for a deep product/operations strategy package, report, or multi-step research despite this lightweight route, say the request should use deep research instead of giving a shallow answer.\n\
- Route metadata: turnClass={}, domainScope={}, searchNeed={}, answerContract={}, complexityScore={}.\n\
{PM_ORCH_INTERNAL_END}",
        route.turn_class.as_str(),
        route.domain_scope.as_str(),
        route.search_need.as_str(),
        route.answer_contract.as_str(),
        route.complexity_score,
    )
}

fn pm_shared_chat_tool_loop_system_instruction(route: &PmTurnRoute) -> String {
    format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
PM_SHARED_CHAT_TOOL_LOOP: PmTurnRouter selected the Codex-like shared chat engine.\n\
Intent standard:\n\
- Use this engine for non-deep turns: ordinary chat, follow-up questions with history, attached-file or pasted-data analysis, stable reasoning, general research, and live/public factual lookups.\n\
- Do not force product/operations framing unless the user asks for product/business/operations strategy.\n\
- If the user clearly asked for an enterprise-grade product/business/ops/market/competitive strategy report and this route was selected by mistake, answer as far as possible and state that deep research would be more appropriate.\n\
Answer rules:\n\
- Use conversation history naturally, especially for short follow-ups such as question marks or \"continue\".\n\
- Preserve the user's requested output shape. For tables or metrics, output real Markdown tables and perform calculations from the provided data where possible.\n\
- For pasted data, CSV-like text, SQL, or attached-file calculations, prefer a concise calculation path. If you use code/REPL, rely on standard-library parsing/math first and do not assume third-party packages are installed.\n\
- When files or attached context are relevant, inspect/use the available file context and cite filenames or visible snippets where useful.\n\
- When search is enabled and current/public facts matter, search before answering, use relevant sources, and cite URLs. If search is enabled but not needed, answer directly.\n\
- If search/files are insufficient or unavailable, still provide the best model answer from the user context and clearly mark uncertainty; do not output a fixed failure template.\n\
- Keep Markdown readable: clear headings, short paragraphs, blank lines between major ideas, and no raw routing JSON/tool diagnostics in visible text.\n\
- Match the user's visible language for all default headings and prose. For a Chinese user request, do not introduce English headings such as \"Key Takeaways\" unless the user asked for English.\n\
- Route metadata for internal calibration only: engine={}, searchPolicy={}, filePolicy={}, reasoningDepth={}, turnClass={}, domainScope={}, searchNeed={}, answerContract={}, complexityScore={}.\n\
{PM_ORCH_INTERNAL_END}",
        route.engine.as_str(),
        route.search_policy.as_str(),
        route.file_policy.as_str(),
        route.reasoning_depth.as_str(),
        route.turn_class.as_str(),
        route.domain_scope.as_str(),
        route.search_need.as_str(),
        route.answer_contract.as_str(),
        route.complexity_score,
    )
}

fn pm_route_reasoning_budget_override(
    route: &PmTurnRoute,
) -> agent_gateway::InternalReasoningBudget {
    match route.reasoning_depth {
        PmReasoningDepth::Fast => agent_gateway::InternalReasoningBudget::Fast,
        PmReasoningDepth::Standard => agent_gateway::InternalReasoningBudget::Standard,
        PmReasoningDepth::Deep => agent_gateway::InternalReasoningBudget::Deep,
    }
}

fn pm_shared_chat_should_use_scratch_session(route: &PmTurnRoute, user_message: &str) -> bool {
    if !matches!(route.search_policy, PmSearchPolicy::Disabled)
        || !matches!(route.turn_class, PmTurnClass::SimpleAnswer)
    {
        return false;
    }
    let text = user_message.trim();
    let lower = text.to_ascii_lowercase();
    let line_count = text.lines().filter(|line| !line.trim().is_empty()).count();
    let has_table_like_text = line_count >= 4
        && (text.matches(',').count() >= 8
            || text.matches('\t').count() >= 4
            || text.matches('|').count() >= 6);
    let has_code_or_query = lower.contains("select ")
        || lower.contains("create table")
        || lower.contains("group by")
        || lower.contains("```");
    let has_document_context = pm_user_message_has_document_context(text);
    has_document_context || has_table_like_text || has_code_or_query || text.chars().count() > 1_200
}

fn normalize_shared_chat_visible_language(text: &str, user_message: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || !contains_cjk(user_message) {
        return text.to_string();
    }
    let mut lines = trimmed.lines().collect::<Vec<_>>();
    if let Some(first) = lines.first_mut() {
        let normalized = first
            .trim()
            .trim_start_matches('#')
            .trim()
            .to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "key takeaways" | "summary" | "executive summary" | "conclusion"
        ) {
            *first = "## 核心结论";
            return lines.join("\n");
        }
    }
    if lines
        .first()
        .is_none_or(|line| !line.trim_start().starts_with('#'))
    {
        return format!("## 核心结论\n\n{trimmed}");
    }
    text.to_string()
}

async fn run_pm_shared_chat_model_fallback(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
    model: &str,
    user_message: &str,
    route: &PmTurnRoute,
    failure_reason: &str,
    answer_delta: Option<PmAnswerDeltaCallback>,
) -> Option<TurnResult> {
    let model_hint = (!model.trim().is_empty()).then_some(model.trim());
    let transient_session = match manager
        .create_session(
            user_id,
            tenant_id,
            None,
            model_hint,
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                error = %error,
                "pm shared chat fallback: transient session creation failed"
            );
            return None;
        }
    };
    let transient_session_id = transient_session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let mut options = agent_gateway::AgentTurnOptions {
        blocked_tools: pm_blocked_non_search_research_tools(),
        reasoning_budget: pm_route_reasoning_budget_override(route),
        prefer_native_web_search: false,
        suppress_native_web_search: true,
        stream_timeout_secs: Some(pm_direct_answer_turn_timeout_secs()),
        ..agent_gateway::AgentTurnOptions::default()
    };
    options.blocked_tools.extend(
        [
            "WebSearch",
            "WebFetch",
            "ToolSearch",
            "ListMcpResources",
            "ReadMcpResource",
            agent_gateway::runtime_builder::CHAT_BLOCK_MCP_SEARCH_TOOLS,
        ]
        .iter()
        .map(|tool| (*tool).to_string()),
    );
    options.blocked_tools.sort();
    options.blocked_tools.dedup();
    options.system_instructions.push(format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
PM_SHARED_CHAT_SOFT_FALLBACK: The shared chat tool loop did not finish cleanly ({failure_reason}).\n\
Answer the user's request directly from the conversation/user-provided context. Do not browse or call tools. Preserve requested output format, especially Markdown tables or SQL. Do not mention internal timeout, routing, fallback, or diagnostics.\n\
Route: engine={}, searchPolicy={}, reasoningDepth={}, turnClass={}, answerContract={}.\n\
{PM_ORCH_INTERNAL_END}",
        route.engine.as_str(),
        route.search_policy.as_str(),
        route.reasoning_depth.as_str(),
        route.turn_class.as_str(),
        route.answer_contract.as_str(),
    ));
    let timeout_secs = pm_direct_answer_turn_timeout_secs();
    let result = if let Some(answer_delta) = answer_delta {
        run_pm_user_visible_answer_streaming_turn(
            manager.clone(),
            transient_session_id.clone(),
            user_message.to_string(),
            timeout_secs,
            "pm shared chat soft fallback turn",
            options,
            move |delta| answer_delta("shared_chat_fallback", delta),
        )
        .await
    } else {
        run_pm_turn_with_timeout_cleanup_and_options(
            manager.clone(),
            transient_session_id.clone(),
            user_message.to_string(),
            timeout_secs,
            "pm shared chat soft fallback turn",
            options,
        )
        .await
    };
    transient_session_guard.finish().await;
    match result {
        Ok(mut turn) if !turn.text.trim().is_empty() => {
            turn.session_id = session_id.to_string();
            Some(turn)
        }
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                error = %error,
                "pm shared chat fallback: model fallback failed"
            );
            None
        }
    }
}

async fn run_pm_shared_chat_turn_on_scratch_session(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    parent_session_id: &str,
    model: &str,
    user_message: &str,
    options: agent_gateway::AgentTurnOptions,
    timeout_secs: u64,
    answer_delta: Option<PmAnswerDeltaCallback>,
) -> (Result<TurnResult, GatewayError>, String) {
    let model_hint = (!model.trim().is_empty()).then_some(model.trim());
    let transient_session = manager
        .create_session(
            user_id,
            tenant_id,
            None,
            model_hint,
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await
        .map_err(|error| {
            GatewayError::RuntimeExecution(format!(
                "create scratch shared-chat PM session failed: {error}"
            ))
        });
    let transient_session = match transient_session {
        Ok(session) => session,
        Err(error) => return (Err(error), String::new()),
    };
    let transient_session_id = transient_session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let (result, partial) = if let Some(answer_delta) = answer_delta {
        run_pm_user_visible_answer_streaming_turn_preserving_partial(
            manager.clone(),
            transient_session_id.clone(),
            user_message.to_string(),
            timeout_secs,
            "pm shared chat scratch turn",
            options,
            move |delta| answer_delta("shared_chat_scratch", delta),
        )
        .await
    } else {
        (
            run_pm_turn_with_timeout_cleanup_and_options(
                manager.clone(),
                transient_session_id.clone(),
                user_message.to_string(),
                timeout_secs,
                "pm shared chat scratch turn",
                options,
            )
            .await,
            String::new(),
        )
    };
    transient_session_guard.finish().await;
    (
        result.map(|mut turn| {
            turn.session_id = parent_session_id.to_string();
            turn
        }),
        partial,
    )
}

fn pm_shared_chat_search_call_summary(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> serde_json::Value {
    let mut native = 0usize;
    let mut mcp = 0usize;
    let mut configured_provider = 0usize;
    let mut rag_local = 0usize;
    let mut other_search = 0usize;
    for tc in tool_calls {
        let haystack = format!(
            "{} {} {} {} {}",
            tc.tool_name, tc.source, tc.source_name, tc.input, tc.output
        )
        .to_ascii_lowercase();
        if haystack.contains("native_model_search")
            || haystack.contains("responses_native_web_search")
            || haystack.contains("web_search_preview")
        {
            native += 1;
        } else if tc.source.eq_ignore_ascii_case("mcp")
            || haystack.contains("mcp_search")
            || haystack.contains("mcp")
        {
            mcp += 1;
        } else if haystack.contains("configured_search_provider")
            || haystack.contains("search_extension")
            || haystack.contains("serper")
            || haystack.contains("brave")
            || haystack.contains("duckduckgo")
            || haystack.contains("google")
        {
            configured_provider += 1;
        } else if haystack.contains("rag") || haystack.contains("local") {
            rag_local += 1;
        } else if tc.tool_name.eq_ignore_ascii_case("WebSearch")
            || tc.tool_name.eq_ignore_ascii_case("WebFetch")
            || haystack.contains("websearch")
            || haystack.contains("web_search")
            || haystack.contains("search")
        {
            other_search += 1;
        }
    }
    serde_json::json!({
        "native": native,
        "mcp": mcp,
        "configuredProvider": configured_provider,
        "ragLocal": rag_local,
        "otherSearch": other_search,
        "totalToolCalls": tool_calls.len(),
    })
}

fn pm_turn_route_human_summary(route: &PmTurnRoute, user_message: &str) -> String {
    let cjk = contains_cjk(user_message);
    if matches!(route.turn_class, PmTurnClass::LiveLookup)
        || matches!(route.search_need, PmSearchNeed::FreshFact)
    {
        return if cjk {
            "已识别为实时查询，将联网获取最新信息并给出来源。".to_string()
        } else {
            "Detected a live lookup; searching for current source-backed information.".to_string()
        };
    }
    if route.is_pm_deep_strategy() {
        return if cjk {
            "已识别为产运策略研究，将进入深度研究流程。".to_string()
        } else {
            "Detected a product operations strategy request; entering deep research.".to_string()
        };
    }
    if matches!(route.search_need, PmSearchNeed::EvidenceAugmented) {
        return if cjk {
            "已识别为需要资料支撑的问题，正在规划检索和综合。".to_string()
        } else {
            "Detected an evidence-backed question; planning retrieval and synthesis.".to_string()
        };
    }
    if cjk {
        "已完成任务理解，准备生成回答。".to_string()
    } else {
        "Task understanding completed; preparing the answer.".to_string()
    }
}

fn parse_pm_report_semantic_extraction(text: &str) -> Option<PmReportSemanticExtraction> {
    extract_first_json_object(text)
        .and_then(|raw| parse_json_object_relaxed(&raw).or_else(|| serde_json::from_str(&raw).ok()))
        .and_then(|value| PmReportSemanticExtraction::from_value(&value))
}

fn pm_blocked_report_semantic_extraction_tools() -> Vec<String> {
    let mut tools = pm_blocked_non_search_research_tools();
    tools.extend(
        [
            agent_gateway::runtime_builder::CHAT_BLOCK_MCP_SEARCH_TOOLS,
            "WebSearch",
            "WebFetch",
            "ToolSearch",
            "ListMcpResources",
            "ReadMcpResource",
            "browser",
            "fetch",
            "web_search",
            "web_fetch",
        ]
        .iter()
        .map(|tool| (*tool).to_string()),
    );
    tools.sort();
    tools.dedup();
    tools
}

fn pm_should_run_report_semantic_extraction(user_message: &str, plan: &serde_json::Value) -> bool {
    if pm_flag_enabled("PM_REPORT_SEMANTIC_EXTRACT_ALWAYS", false) {
        return true;
    }
    let min_chars = pm_env_usize("PM_REPORT_SEMANTIC_EXTRACT_MIN_CHARS", 900).clamp(300, 8_000);
    let message_chars = user_message.chars().count();
    let non_empty_lines = user_message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let first_party_chars = plan
        .get("reportStrategy")
        .and_then(|value| value.get("firstPartyEvidenceJson"))
        .map(ToString::to_string)
        .map(|value| value.chars().count())
        .unwrap_or(0);
    message_chars >= min_chars || non_empty_lines >= 10 || first_party_chars >= min_chars / 2
}

async fn enrich_pm_report_strategy_with_semantic_extraction(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    plan: &mut serde_json::Value,
    on_stage: &mut PmStageCallback<'_>,
) {
    if !pm_is_report_strategy_mode(plan) {
        return;
    }
    let extract_started = Instant::now();
    if !pm_should_run_report_semantic_extraction(user_message, plan) {
        on_stage(
            "report_extract",
            "skipped",
            1,
            Some(serde_json::json!({
                "mode": "business_report_strategy",
                "phase": "semantic_extraction",
                "applied": false,
                "durationMs": extract_started.elapsed().as_millis(),
                "reason": "deterministic_extraction_sufficient_for_compact_input",
                "fallback": "deterministic_report_extraction",
            })),
        );
        return;
    }
    on_stage(
        "report_extract",
        "running",
        1,
        Some(serde_json::json!({
            "mode": "business_report_strategy",
            "phase": "semantic_extraction",
            "message": "正在提取一手报告里的指标、人群、约束与关键句",
            "toolPolicy": "disabled",
            "timingPolicy": "bounded",
            "timeoutSecs": pm_report_semantic_extract_timeout_secs(),
        })),
    );
    let prompt = wrap_pm_research_prompt(
        session_source,
        build_pm_report_semantic_extract_prompt(user_message, plan),
    );
    let Some(handle) = manager.get_session(session_id).await else {
        on_stage(
            "report_extract",
            "degraded",
            1,
            Some(serde_json::json!({
                "mode": "business_report_strategy",
                "phase": "semantic_extraction",
                "applied": false,
                "degraded": true,
                "durationMs": extract_started.elapsed().as_millis(),
                "reason": "session_not_found_for_transient_semantic_extraction",
                "fallback": "deterministic_report_extraction",
            })),
        );
        return;
    };
    let model_hint = if handle.model.trim().is_empty() {
        None
    } else {
        Some(handle.model.as_str())
    };
    let transient_session = match manager
        .create_session(
            &handle.user_id,
            &handle.tenant_id,
            None,
            model_hint,
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            on_stage(
                "report_extract",
                "degraded",
                1,
                Some(serde_json::json!({
                    "mode": "business_report_strategy",
                    "phase": "semantic_extraction",
                    "applied": false,
                    "degraded": true,
                    "durationMs": extract_started.elapsed().as_millis(),
                    "reason": format!("transient_session_create_failed: {error}"),
                    "fallback": "deterministic_report_extraction",
                })),
            );
            return;
        }
    };
    let transient_session_id = transient_session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let mut options = agent_gateway::AgentTurnOptions {
        blocked_tools: pm_blocked_report_semantic_extraction_tools(),
        disable_tools: true,
        reasoning_budget: agent_gateway::InternalReasoningBudget::Standard,
        prefer_native_web_search: false,
        suppress_native_web_search: true,
        stream_timeout_secs: Some(pm_report_semantic_extract_timeout_secs()),
        ..agent_gateway::AgentTurnOptions::default()
    };
    options.system_instructions.push(
        "This internal report-extraction turn is strictly no-tools: do not browse, search, fetch URLs, inspect files, or call MCP/resources. Return only the requested JSON."
            .to_string(),
    );
    let result = run_pm_turn_with_timeout_cleanup_and_options(
        manager.clone(),
        transient_session_id.clone(),
        prompt,
        pm_report_semantic_extract_timeout_secs(),
        "pm report semantic extraction turn",
        options,
    )
    .await;
    transient_session_guard.finish().await;
    match result {
        Ok(turn) => {
            if !turn.tool_calls.is_empty() {
                tracing::warn!(
                    session_id = %session_id,
                    transient_session_id = %transient_session_id,
                    tool_call_count = turn.tool_calls.len(),
                    "pm report semantic extraction unexpectedly used tools; ignoring extraction result"
                );
                on_stage(
                    "report_extract",
                    "degraded",
                    1,
                    Some(serde_json::json!({
                        "mode": "business_report_strategy",
                        "phase": "semantic_extraction",
                        "applied": false,
                        "degraded": true,
                        "durationMs": extract_started.elapsed().as_millis(),
                        "reason": "semantic_extraction_used_tool",
                        "toolCallCount": turn.tool_calls.len(),
                        "fallback": "deterministic_report_extraction",
                    })),
                );
            } else if let Some(extraction) = parse_pm_report_semantic_extraction(&turn.text) {
                let applied = apply_pm_report_semantic_extraction(plan, &extraction);
                on_stage(
                    "report_extract",
                    if applied { "completed" } else { "degraded" },
                    1,
                    Some(serde_json::json!({
                        "mode": "business_report_strategy",
                        "phase": "semantic_extraction",
                        "applied": applied,
                        "durationMs": extract_started.elapsed().as_millis(),
                        "toolPolicy": "disabled",
                        "timingPolicy": "bounded",
                        "timeoutSecs": pm_report_semantic_extract_timeout_secs(),
                        "source": extraction.source,
                        "domainTerms": extraction.domain_terms,
                        "productTerms": extraction.product_terms,
                        "metricTerms": extraction.metric_terms,
                        "objectiveTerms": extraction.objective_terms,
                        "constraintTerms": extraction.constraint_terms,
                        "segmentTerms": extraction.segment_terms,
                        "mechanismTerms": extraction.mechanism_terms,
                        "priorExperimentTerms": extraction.prior_experiment_terms,
                        "keySentenceCount": extraction.key_sentences.len(),
                        "searchQueryCount": extraction.search_queries.len(),
                    })),
                );
            } else {
                on_stage(
                    "report_extract",
                    "degraded",
                    1,
                    Some(serde_json::json!({
                        "mode": "business_report_strategy",
                        "phase": "semantic_extraction",
                        "applied": false,
                        "degraded": true,
                        "durationMs": extract_started.elapsed().as_millis(),
                        "reason": "semantic_extraction_missing_valid_json",
                        "fallback": "deterministic_report_extraction",
                    })),
                );
            }
        }
        Err(error) => {
            on_stage(
                "report_extract",
                "degraded",
                1,
                Some(serde_json::json!({
                    "mode": "business_report_strategy",
                    "phase": "semantic_extraction",
                    "applied": false,
                    "degraded": true,
                    "durationMs": extract_started.elapsed().as_millis(),
                    "reason": error.to_string(),
                    "timeoutSecs": pm_report_semantic_extract_timeout_secs(),
                    "fallback": "deterministic_report_extraction",
                })),
            );
        }
    }
}

pub(super) async fn run_pm_orchestrated_turn(
    state: &AppState,
    manager: Arc<AgentSessionManager>,
    db: &sqlx::SqlitePool,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    primary_message: String,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    run_id_hint: Option<&str>,
    cancel_task_id: Option<&str>,
    resume_checkpoint: Option<&PmResumeCheckpoint>,
    memory_instruction: Option<String>,
    on_stage_fn: &mut PmStageCallback<'_>,
    answer_delta: Option<PmAnswerDeltaCallback>,
) -> Result<(TurnResult, PmAnswerQualityDto), GatewayError> {
    let mut on_stage =
        |stage: &str, status: &str, attempt: usize, detail: Option<serde_json::Value>| {
            let detail = if stage.eq_ignore_ascii_case("synthesize") {
                detail.map(|value| pm_attach_force_synth_diag(session_id, value))
            } else {
                detail
            };
            on_stage_fn(stage, status, attempt, detail);
        };
    let resume_stage = resume_checkpoint.and_then(|checkpoint| checkpoint.stage.as_deref());
    let should_announce_understanding = !resume_stage.is_some_and(|stage| {
        matches!(
            stage,
            "planner" | "retrieve" | "verify" | "retry_repair" | "synthesize"
        )
    });
    if should_announce_understanding {
        on_stage(
            "understand",
            "running",
            1,
            Some(serde_json::json!({
                "message": "正在先理解问题、确认研究目标和需要核对的证据。",
                "humanSummary": "正在先理解问题、确认研究目标和需要核对的证据。",
            })),
        );
    }
    let run_id = run_id_hint
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| format!("pm-run-{}", uuid::Uuid::new_v4()));
    let memory_instruction = match crate::semantic_kernel_store::load_pm_requirement_state_context(
        db, tenant_id, session_id,
    )
    .await
    {
        Ok(Some(requirement_context)) => Some(match memory_instruction {
            Some(existing) if !existing.trim().is_empty() => {
                format!("{existing}\n\n{requirement_context}")
            }
            _ => requirement_context,
        }),
        Ok(None) => memory_instruction,
        Err(error) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                session_id = %session_id,
                error = %error,
                "failed to load PM requirement-state context"
            );
            memory_instruction
        }
    };
    let (runtime_budget, budget_snapshot) = resolve_pm_budget_snapshot(db, tenant_id).await;
    persist_pm_run_start(
        db,
        &run_id,
        cancel_task_id,
        tenant_id,
        user_id,
        session_id,
        if cancel_task_id.is_some() {
            "background_task"
        } else {
            "foreground_turn"
        },
        user_message,
        &budget_snapshot,
    )
    .await;
    record_pm_prompt_usage(
        state.telemetry_db(),
        tenant_id,
        &run_id,
        "planner",
        "pm_orchestrator_retrieve",
        "v2",
        &sha256_hex("pm_orchestrator_retrieve_v2"),
    )
    .await;
    record_pm_audit_event(
        state.telemetry_db(),
        tenant_id,
        user_id,
        &run_id,
        "pm_run_started",
        "info",
        "pm turn started",
        Some(&serde_json::json!({
            "sessionId": session_id,
            "source": session_source,
            "model": model,
            "budgetProfile": budget_snapshot.budget_profile,
            "pipelineTimeoutSecs": runtime_budget.pipeline_timeout_secs
        })),
    )
    .await;

    let (session_mcp_servers, session_skills) = manager
        .get_session(session_id)
        .await
        .map(|handle| {
            (
                handle.session_metadata.mcp_servers,
                handle.session_metadata.skills,
            )
        })
        .unwrap_or_default();
    if let Err(error) = crate::semantic_kernel_store::persist_pm_prompt_context_manifest(
        db,
        tenant_id,
        session_id,
        &run_id,
        model,
        session_source,
        &primary_message,
        memory_instruction.as_deref(),
        &session_mcp_servers,
        &session_skills,
    )
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            tenant_id = %tenant_id,
            error = %error,
            "failed to persist PM prompt/context manifest"
        );
    }
    let prepared = prepare_pm_orchestration_plan(
        manager.clone(),
        db,
        session_id,
        session_source,
        user_message,
        user_id,
        tenant_id,
        model,
        runtime_budget,
        resume_checkpoint,
        &session_mcp_servers,
        &session_skills,
        &mut on_stage,
    )
    .await?;

    let mut plan = prepared.plan;
    match crate::semantic_kernel_store::persist_pm_requirement_state_delta(
        db,
        tenant_id,
        session_id,
        &run_id,
        user_message,
        &plan,
    )
    .await
    {
        Ok(requirement_state) => {
            let next_question = pm_domain::requirement_state::next_question(&requirement_state);
            on_stage(
                "requirement_state",
                "completed",
                1,
                Some(serde_json::json!({
                    "message": "需求状态已根据本轮输入增量更新。",
                    "requirementState": requirement_state,
                    "nextQuestion": next_question,
                })),
            );
        }
        Err(error) => {
            tracing::warn!(
                run_id = %run_id,
                tenant_id = %tenant_id,
                error = %error,
                "failed to persist PM requirement-state delta"
            );
            on_stage(
                "requirement_state",
                "degraded",
                1,
                Some(serde_json::json!({
                    "message": "需求状态暂未更新，本轮研究仍继续。",
                    "errorClass": "requirement_state_persistence",
                })),
            );
        }
    }
    let runtime_budget = prepared.runtime_budget;
    let resume_detail = prepared.resume_detail;
    let resume_skip_planner = prepared.resume_skip_planner;
    let resume_attempt = prepared.resume_attempt;
    let planned_turn_route = pm_plan_turn_route(&plan);
    if let Some(route) = planned_turn_route.clone() {
        if pm_route_should_use_shared_chat_engine(&route) {
            return run_pm_routed_shared_chat_tool_loop(
                state,
                db,
                tenant_id,
                user_id,
                &run_id,
                session_id,
                user_message,
                model,
                route,
                manager.clone(),
                memory_instruction.clone(),
                &mut on_stage,
                answer_delta.clone(),
            )
            .await;
        }
    }

    if planned_turn_route.is_none() {
        let task_graph = plan
            .get("taskGraph")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let intent = task_graph
            .get("intent")
            .and_then(|v| v.as_str())
            .unwrap_or("analysis");
        let decomposition_mode = task_graph
            .get("decompositionMode")
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        let route = build_pm_fallback_turn_route(user_message, &plan);
        apply_pm_turn_route_to_plan(&mut plan, &route);
        tracing::warn!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            session_id = %session_id,
            run_id = %run_id,
            intent = %intent,
            decomposition_mode = %decomposition_mode,
            engine = %route.engine.as_str(),
            search_policy = %route.search_policy.as_str(),
            "TURN_ROUTE missing; applying fallback router"
        );
        if pm_route_should_use_shared_chat_engine(&route) {
            return run_pm_routed_shared_chat_tool_loop(
                state,
                db,
                tenant_id,
                user_id,
                &run_id,
                session_id,
                user_message,
                model,
                route,
                manager.clone(),
                memory_instruction.clone(),
                &mut on_stage,
                answer_delta.clone(),
            )
            .await;
        }
    }

    // Capability diagnostics are only needed by the durable deep-research branch. Delaying this
    // lookup keeps ordinary turns and the initial task-understanding stage off the DB/config path.
    let search_doctor_detail = build_pm_search_doctor_detail(state, tenant_id, model).await;

    let subtask_runtime_metas = collect_pm_subtask_runtime_metas(&plan);
    let mut subtask_run_ids = HashMap::<String, u64>::new();
    let mut subtask_probe_attempt_seq = HashMap::<String, usize>::new();
    for meta in &subtask_runtime_metas {
        if let Some(subtask_run_id) = upsert_pm_subtask_run(
            state.telemetry_db(),
            &PmSubtaskRunUpsertPayload {
                run_id: run_id.clone(),
                task_id: cancel_task_id.map(std::string::ToString::to_string),
                tenant_id: tenant_id.to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                subtask_key: meta.key.clone(),
                subtask_id: meta.subtask_id.clone(),
                title: meta.title.clone(),
                goal: meta.goal.clone(),
                deliverable: meta.deliverable.clone(),
                required_evidence_type: meta.required_evidence_type.clone(),
                priority: meta.priority.clone(),
                status: "queued".to_string(),
                probe_candidate_count: 0,
                probe_completed_count: 0,
                citation_count: 0,
                domain_count: 0,
                tool_call_count: 0,
                quality_score: None,
                error_code: None,
                error_message: None,
                detail: None,
            },
        )
        .await
        {
            subtask_run_ids.insert(meta.key.clone(), subtask_run_id);
        }
    }

    let resume_variant = resume_detail
        .as_ref()
        .and_then(|detail| detail.get("selectedVariant"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    let resume_route = resume_detail
        .as_ref()
        .and_then(|detail| detail.get("selectedRoute"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    let resume_route_channel = resume_detail
        .as_ref()
        .and_then(|detail| detail.get("selectedRouteChannel"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);

    let mut attempt: usize = if resume_skip_planner {
        resume_attempt.saturating_add(1)
    } else {
        1
    };
    let decomposition_mode = plan
        .get("taskGraph")
        .and_then(|value| value.get("decompositionMode"))
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "light".to_string());
    let subtask_count = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    // Extra global attempts are opt-in. The adaptive probe wave repairs only dimensions
    // that remain thin instead of paying a fixed retry for every planned subtask.
    let subtask_gap_extra_attempts = pm_env_usize("PM_SUBTASK_GAP_EXTRA_ATTEMPTS", 0);
    let max_attempts_hard_cap = pm_env_usize("PM_MAX_ATTEMPTS_HARD_CAP", 12).max(1);
    let dynamic_extra_attempts = if decomposition_mode == "none" || subtask_count == 0 {
        0
    } else {
        subtask_count.min(subtask_gap_extra_attempts)
    };
    let max_attempts: usize = runtime_budget
        .max_attempts
        .max(1)
        .saturating_add(dynamic_extra_attempts)
        .min(max_attempts_hard_cap);
    if attempt > max_attempts {
        attempt = max_attempts.max(1);
    }
    let source_slot_budget_secs = if resume_route_channel
        .as_deref()
        .is_some_and(|channel| channel.eq_ignore_ascii_case("browser"))
    {
        runtime_budget.source_slot_browser_secs
    } else {
        runtime_budget.source_slot_search_secs
    };
    let mut open_domain_circuit_keys = load_open_pm_domain_circuit_keys(db, tenant_id, 96).await;
    let mut current_message = wrap_pm_research_prompt(
        session_source,
        build_pm_retrieve_prompt(
            user_message,
            &plan,
            resume_variant.as_deref(),
            resume_route.as_deref(),
            attempt,
            &runtime_budget,
            source_slot_budget_secs,
            &open_domain_circuit_keys,
        ),
    );
    let mut current_attempt_strategy: Option<PmRepairStrategy> = None;
    if current_message.trim().is_empty() {
        current_message = primary_message;
    }
    let plan_query_variants = normalized_pm_plan_query_variants(&plan, user_message);
    let plan_enabled_routes = collect_enabled_pm_routes(&plan);
    let plan_route_count = plan_enabled_routes.len();
    let parallel_subtask_enabled = pm_flag_enabled(
        "PM_ENABLE_PARALLEL_SUBTASK_KERNEL",
        PM_ENABLE_PARALLEL_PROBE_SELECT_DEFAULT,
    );
    let parallel_subtask_max_candidates = pm_env_usize(
        "PM_PARALLEL_SUBTASK_MAX_CANDIDATES",
        PM_PARALLEL_SUBTASK_MAX_CANDIDATES_DEFAULT,
    )
    .max(1);
    let parallel_subtask_max_concurrency = pm_env_usize(
        "PM_PARALLEL_SUBTASK_MAX_CONCURRENCY",
        PM_PARALLEL_SUBTASK_MAX_CONCURRENCY_DEFAULT,
    )
    .max(1);
    let parallel_subtask_max_attempts = pm_env_usize(
        "PM_PARALLEL_SUBTASK_MAX_ATTEMPTS",
        PM_PARALLEL_SUBTASK_MAX_ATTEMPTS_DEFAULT,
    )
    .max(1);
    let parallel_subtask_use_best_turn = pm_flag_enabled("PM_PARALLEL_SUBTASK_USE_BEST_TURN", true);
    let adaptive_probe_waves = pm_flag_enabled("PM_ADAPTIVE_PROBE_WAVES", true);
    let adaptive_probe_repair_cap =
        pm_env_usize("PM_ADAPTIVE_PROBE_REPAIR_MAX_CANDIDATES", 1).clamp(1, 4);
    let route_fail_block_threshold = pm_env_usize(
        "PM_ROUTE_FAIL_STREAK_BLOCK_THRESHOLD",
        PM_ROUTE_FAIL_STREAK_BLOCK_THRESHOLD_DEFAULT,
    )
    .max(1);
    let source_quota_limit = runtime_budget.max_calls_per_source.max(1);
    let domain_quota_limit = runtime_budget.max_calls_per_source.max(1);
    let deep_loop_enabled = pm_turn_route_allows_deep_strategy(&plan, user_message)
        && PmDeepResearchLoop::should_enable(&plan, user_message);
    let deep_loop_max_wall_secs = pm_deep_loop_max_wall_secs();
    let deep_loop_no_new_evidence_limit = pm_deep_loop_no_new_evidence_limit();
    let mut route_usage_counts: HashMap<String, usize> = HashMap::new();
    let mut route_fail_streaks: HashMap<String, usize> = HashMap::new();
    let mut route_blocklist: HashSet<String> = HashSet::new();
    let mut used_retrieval_keys: HashSet<String> = HashSet::new();
    let mut used_retrieval_variants: HashSet<String> = HashSet::new();
    let mut domain_usage_counts: HashMap<String, usize> = HashMap::new();
    let orchestration_started = Instant::now();
    let mut last_usable_turn: Option<TurnResult> = None;
    let mut last_usable_quality: Option<PmAnswerQualityDto> = None;
    let mut best_turn: Option<TurnResult> = None;
    let mut best_quality: Option<PmAnswerQualityDto> = None;
    let mut accumulated_observed_tool_calls: Vec<agent_gateway::ToolCallRecord> = Vec::new();
    let mut probe_outcomes: Vec<PmProbeOutcome> = Vec::new();
    let strict_subtask_closure_enabled = pm_flag_enabled("PM_STRICT_SUBTASK_CLOSURE", true);
    let subtask_max_repair_attempts =
        pm_env_usize("PM_SUBTASK_MAX_REPAIR_ATTEMPTS_PER_TASK", 2).max(1);
    let probe_history_cap =
        pm_env_usize("PM_SUBTASK_PROBE_OUTCOME_HISTORY_CAP", 640).clamp(64, 4096);
    let mut accumulated_probe_outcomes: Vec<PmProbeOutcome> = Vec::new();
    let mut pending_subtask_repair_queue: Vec<String> = Vec::new();
    let mut subtask_repair_attempts: HashMap<String, usize> = HashMap::new();
    let mut active_subtask_focus: Option<String> = None;
    let mut no_new_evidence_repeats: usize = 0;
    let mut best_evidence_signal: usize = 0;
    let mut llm_expert_review_completed = false;
    let mut retained_llm_expert_review: Option<PmLlmExpertReview> = None;
    let mut retained_llm_expert_review_trace = serde_json::json!({
        "enabled": false,
        "reason": "not_run_yet"
    });
    let subtask_meta_map: HashMap<String, PmSubtaskRuntimeMeta> = subtask_runtime_metas
        .iter()
        .map(|meta| (meta.key.clone(), meta.clone()))
        .collect();
    if pm_plan_should_synthesize_without_external_retrieval(&plan, user_message) {
        tracing::info!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            session_id = %session_id,
            run_id = %run_id,
            "pm plan selected first-party-only deep synthesis; bypassing external retrieval loop"
        );
        on_stage(
            "retrieve",
            "completed",
            attempt,
            Some(serde_json::json!({
                "mode": "first_party_synthesis",
                "humanSummary": "已确认本轮主要依赖用户提供的一手数据，跳过外部检索并进入深度综合。",
                "externalRetrievalBypassed": true,
            })),
        );
        on_stage(
            "synthesize",
            "running",
            attempt,
            Some(serde_json::json!({
                "mode": "first_party_synthesis",
                "humanSummary": "正在基于一手数据进行深度策略综合。",
            })),
        );
        let turn = match run_pm_force_synthesize_fallback_turn_with_observed_tools(
            manager.clone(),
            session_id,
            session_source,
            user_message,
            &[],
            attempt,
            &[],
            answer_delta.clone(),
        )
        .await
        {
            Ok(turn) => turn,
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    session_id = %session_id,
                    run_id = %run_id,
                    "first-party synthesis failed; returning local first-party synthesis fallback: {}",
                    error
                );
                build_pm_local_strategy_synthesis_turn(
                    session_id,
                    model,
                    user_message,
                    &format!("first-party synthesis failed: {error}"),
                    attempt,
                    &[],
                )
            }
        };
        let mut quality = evaluate_pm_answer_quality(&turn);
        apply_pm_first_party_quality_policy(&mut quality, &turn.text);
        let _ =
            apply_pm_report_strategy_quality_gate(&mut quality, &plan, user_message, &turn.text);
        on_stage(
            "synthesize",
            "completed",
            attempt,
            Some(serde_json::json!({
                "answerLength": turn.text.chars().count(),
                "qualityGatePassed": quality.passed,
                "reason": "first_party_synthesis_completed"
            })),
        );
        return finalize_pm_orchestration_result(
            state.telemetry_db(),
            tenant_id,
            &run_id,
            session_id,
            turn,
            quality,
        )
        .await;
    }
    let min_synthesize_window_secs = if deep_loop_enabled {
        pm_env_u64(
            "PM_SYNTHESIZE_RESERVED_WINDOW_SECS",
            pm_deep_loop_min_synthesis_window_secs(),
        )
        .max(pm_deep_loop_min_synthesis_window_secs())
    } else {
        pm_env_u64("PM_SYNTHESIZE_RESERVED_WINDOW_SECS", 50).max(25)
    };
    if deep_loop_enabled {
        let deep_loop_detail = serde_json::json!({
            "event": "pm.deep_loop.started",
            "loopState": "initialize",
            "maxWallSecs": deep_loop_max_wall_secs,
            "noNewEvidenceLimit": deep_loop_no_new_evidence_limit,
            "expertLenses": [
                "growth",
                "monetization",
                "retention",
                "user_segmentation",
                "value_exchange_economics",
                "experiment_design",
                "risk_fraud_compliance",
                "ux_user_psychology",
                "business_model_unit_economics",
                "platform_policy"
            ],
        });
        on_stage("deep_loop", "running", 1, Some(deep_loop_detail.clone()));
        record_pm_audit_event(
            state.telemetry_db(),
            tenant_id,
            user_id,
            &run_id,
            "pm.deep_loop.started",
            "info",
            "PM deep research loop started",
            Some(&deep_loop_detail),
        )
        .await;
        let lens_detail = serde_json::json!({
            "event": "pm.deep_loop.lens_generated",
            "loopState": "build_expert_lens_matrix",
            "expertLenses": deep_loop_detail
                .get("expertLenses")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        });
        record_pm_audit_event(
            state.telemetry_db(),
            tenant_id,
            user_id,
            &run_id,
            "pm.deep_loop.lens_generated",
            "info",
            "PM deep research expert lens matrix generated",
            Some(&lens_detail),
        )
        .await;
    }

    loop {
        open_domain_circuit_keys = load_open_pm_domain_circuit_keys(db, tenant_id, 96).await;
        let elapsed_secs = orchestration_started.elapsed().as_secs();
        let remaining_pipeline_secs = runtime_budget
            .pipeline_timeout_secs
            .saturating_sub(elapsed_secs);
        if remaining_pipeline_secs <= min_synthesize_window_secs {
            on_stage(
                "retrieve",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "error": format!(
                        "pipeline_timeout_after_{}s",
                        runtime_budget.pipeline_timeout_secs
                    ),
                    "pipelineElapsedSecs": elapsed_secs,
                    "pipelineRemainingSecs": remaining_pipeline_secs,
                })),
            );
            if let Some(turn) = last_usable_turn.clone() {
                let quality = degrade_pm_quality_with_reason(
                    last_usable_quality
                        .clone()
                        .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                    "pipeline_timeout",
                    "Final attempt hit time budget; kept the best available answer from previous retrieval.",
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "pipeline_timeout_partial_answer_kept"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            on_stage(
                "retry_repair",
                "running",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_pipeline_timeout",
                    "reason": "pipeline_timeout_no_retrieval_output",
                })),
            );
            // Do not wrap visible synthesis in another timeout. The synthesis
            // helper owns its shared primary/continuation budget and preserves
            // every emitted delta. Dropping this future at the pipeline edge
            // used to discard a nearly complete report.
            let forced_turn = match run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                &accumulated_observed_tool_calls,
                answer_delta.clone(),
            )
            .await
            {
                Ok(turn) => Some(turn),
                Err(error) => {
                    tracing::warn!(
                        session_id = %session_id,
                        attempt = attempt,
                        error = %error,
                        "pipeline-budget forced synthesis failed; using emergency conclusion"
                    );
                    None
                }
            };
            if let Some(turn) = forced_turn {
                let quality = evaluate_pm_answer_quality(&turn);
                let quality_gate_passed = quality.passed;
                on_stage(
                    "retry_repair",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "force_synthesize_after_pipeline_timeout",
                        "result": if quality_gate_passed {
                            "recovered_answer_delivered"
                        } else {
                            "degraded_answer_delivered"
                        },
                    })),
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": quality_gate_passed,
                        "reason": "forced_synthesize_after_pipeline_budget"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            let fallback_tool_summary = last_usable_turn
                .as_ref()
                .and_then(|turn| {
                    if turn.tool_calls.is_empty() {
                        None
                    } else {
                        Some(build_pm_tool_summary_value(&turn.tool_calls))
                    }
                })
                .or_else(|| {
                    if accumulated_observed_tool_calls.is_empty() {
                        None
                    } else {
                        Some(build_pm_tool_summary_value(
                            &accumulated_observed_tool_calls,
                        ))
                    }
                });
            let emergency_turn = TurnResult {
                session_id: session_id.to_string(),
                text: build_pm_emergency_conclusion_text(
                    user_message,
                    &format!(
                        "pipeline timeout after {}s",
                        runtime_budget.pipeline_timeout_secs
                    ),
                    attempt,
                    fallback_tool_summary.as_ref(),
                    last_usable_quality.as_ref(),
                    Some(&probe_outcomes),
                ),
                tool_calls: accumulated_observed_tool_calls.clone(),
                usage: TokenUsageRecord {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    total_tokens: 0,
                    estimated_cost_usd: 0.0,
                    model: model.to_string(),
                },
                compacted: None,
                iterations: 1,
                metadata: None,
                hot_reloaded: false,
                thinking: None,
            };
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&emergency_turn),
                "pipeline_timeout_emergency_conclusion",
                "Pipeline timeout persisted after repair attempts; returned emergency conclusion.",
            );
            on_stage(
                "retry_repair",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_pipeline_timeout",
                    "reason": "forced_synthesis_failed",
                })),
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": emergency_turn.text.chars().count(),
                    "qualityGatePassed": false,
                    "reason": "pipeline_timeout_emergency_conclusion"
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                emergency_turn,
                quality,
            )
            .await;
        }
        if let Some(task_id) = cancel_task_id {
            if pm_research_task_manager()
                .is_cancel_requested(task_id)
                .await
            {
                return Err(GatewayError::Internal(
                    "pm_research_task_cancelled".to_string(),
                ));
            }
        }
        if attempt > 1 {
            let backoff_wait_ms =
                pm_apply_retry_governance_delay(db, tenant_id, &run_id, session_id, attempt).await;
            if backoff_wait_ms > 0 {
                on_stage(
                    "retry_repair",
                    "running",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "distributed_retry_backoff",
                        "delayMs": backoff_wait_ms,
                    })),
                );
                on_stage(
                    "retry_repair",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "distributed_retry_backoff",
                        "delayMs": backoff_wait_ms,
                    })),
                );
            }
        }

        on_stage("retrieve", "running", attempt, None);
        if attempt == 1 {
            on_stage(
                "retrieve",
                "running",
                attempt,
                Some(serde_json::json!({
                    "message": "PM Search Orchestrator initialized",
                    "searchPipeline": search_doctor_detail,
                })),
            );
        }
        let retrieve_started = Instant::now();
        let mut retrieve_route = "main_session";
        let mut selected_probe_variant: Option<String> = None;
        let mut selected_probe_route_id: Option<String> = None;
        let mut selected_probe_route_channel: Option<String> = None;
        let mut selected_probe_score: Option<i64> = None;
        probe_outcomes.clear();
        let mut selected_probe_turn: Option<TurnResult> = None;
        let plan_parallel = plan.get("parallelism").and_then(|v| v.as_object());
        let planned_probe_candidate_max = plan_parallel
            .and_then(|obj| obj.get("probeCandidateMax"))
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let min_sources_per_subtask = plan_parallel
            .and_then(|obj| obj.get("minSourcesPerSubtask"))
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(1)
            .max(1);
        let planned_subtask_count = plan
            .get("taskGraph")
            .and_then(|value| value.get("subtasks"))
            .and_then(|value| value.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);
        let fallback_probe_candidate_target = planned_subtask_count
            .saturating_mul(min_sources_per_subtask)
            .max(1);
        let requested_probe_candidate_cap = if planned_probe_candidate_max > 0 {
            planned_probe_candidate_max
        } else {
            fallback_probe_candidate_target
        };
        let hard_probe_candidate_cap = parallel_subtask_max_candidates.max(1);
        let probe_candidate_cap = if adaptive_probe_waves && attempt == 1 {
            requested_probe_candidate_cap
                .max(planned_subtask_count)
                .clamp(1, hard_probe_candidate_cap)
        } else {
            requested_probe_candidate_cap.clamp(1, hard_probe_candidate_cap)
        };
        let repair_targets = active_subtask_focus
            .as_ref()
            .map(|title| vec![title.clone()])
            .unwrap_or_default();
        let (probe_candidates, skipped_duplicate_probe_candidates) = if parallel_subtask_enabled
            && attempt <= parallel_subtask_max_attempts
        {
            let mut candidates = build_pm_probe_candidates(&plan);
            if strict_subtask_closure_enabled && !repair_targets.is_empty() {
                candidates =
                    prioritize_pm_probe_candidates_for_subtasks(candidates, &repair_targets, true);
            }
            let (fresh_candidates, skipped_duplicate_probe_candidates) =
                pm_filter_fresh_probe_candidates(candidates, &used_retrieval_keys);
            let selected = if adaptive_probe_waves {
                pm_select_adaptive_probe_wave(
                    fresh_candidates,
                    attempt,
                    probe_candidate_cap,
                    !repair_targets.is_empty(),
                    adaptive_probe_repair_cap,
                )
            } else {
                fresh_candidates
                    .into_iter()
                    .take(probe_candidate_cap)
                    .collect::<Vec<_>>()
            };
            (selected, skipped_duplicate_probe_candidates)
        } else {
            (Vec::new(), 0)
        };
        if skipped_duplicate_probe_candidates > 0 {
            on_stage(
                "retrieve",
                "running",
                attempt,
                Some(serde_json::json!({
                    "message": "skipping repeated research probes",
                    "skippedDuplicateProbeCandidates": skipped_duplicate_probe_candidates,
                    "usedRetrievalKeyCount": used_retrieval_keys.len(),
                })),
            );
        }
        pm_mark_probe_candidates_used(
            &probe_candidates,
            &mut used_retrieval_keys,
            &mut used_retrieval_variants,
        );
        let probe_kernel_active = !probe_candidates.is_empty();
        let mut best_turn_adopted = false;
        let mut attempt_subtask_candidate_counts = HashMap::<String, usize>::new();
        for candidate in &probe_candidates {
            if let Some(key) = candidate
                .subtask_key
                .as_deref()
                .map(normalize_claim_key)
                .filter(|value| !value.is_empty())
            {
                let entry = attempt_subtask_candidate_counts.entry(key).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }
        for (subtask_key, candidate_count) in &attempt_subtask_candidate_counts {
            let meta = subtask_meta_map.get(subtask_key);
            let fallback_title = meta
                .map(|item| item.title.clone())
                .unwrap_or_else(|| subtask_key.clone());
            let subtask_run_id = if let Some(existing) = subtask_run_ids.get(subtask_key).copied() {
                existing
            } else if let Some(new_id) = upsert_pm_subtask_run(
                state.telemetry_db(),
                &PmSubtaskRunUpsertPayload {
                    run_id: run_id.clone(),
                    task_id: cancel_task_id.map(std::string::ToString::to_string),
                    tenant_id: tenant_id.to_string(),
                    user_id: user_id.to_string(),
                    session_id: session_id.to_string(),
                    subtask_key: subtask_key.clone(),
                    subtask_id: meta.and_then(|item| item.subtask_id.clone()),
                    title: fallback_title.clone(),
                    goal: meta.and_then(|item| item.goal.clone()),
                    deliverable: meta.and_then(|item| item.deliverable.clone()),
                    required_evidence_type: meta
                        .and_then(|item| item.required_evidence_type.clone()),
                    priority: meta
                        .map(|item| item.priority.clone())
                        .unwrap_or_else(|| "medium".to_string()),
                    status: "running".to_string(),
                    probe_candidate_count: *candidate_count,
                    probe_completed_count: 0,
                    citation_count: 0,
                    domain_count: 0,
                    tool_call_count: 0,
                    quality_score: None,
                    error_code: None,
                    error_message: None,
                    detail: Some(serde_json::json!({
                        "attempt": attempt,
                        "candidateCount": candidate_count,
                        "source": "probe_scheduler",
                    })),
                },
            )
            .await
            {
                subtask_run_ids.insert(subtask_key.clone(), new_id);
                new_id
            } else {
                continue;
            };
            let _ = upsert_pm_subtask_run(
                state.telemetry_db(),
                &PmSubtaskRunUpsertPayload {
                    run_id: run_id.clone(),
                    task_id: cancel_task_id.map(std::string::ToString::to_string),
                    tenant_id: tenant_id.to_string(),
                    user_id: user_id.to_string(),
                    session_id: session_id.to_string(),
                    subtask_key: subtask_key.clone(),
                    subtask_id: meta.and_then(|item| item.subtask_id.clone()),
                    title: fallback_title,
                    goal: meta.and_then(|item| item.goal.clone()),
                    deliverable: meta.and_then(|item| item.deliverable.clone()),
                    required_evidence_type: meta
                        .and_then(|item| item.required_evidence_type.clone()),
                    priority: meta
                        .map(|item| item.priority.clone())
                        .unwrap_or_else(|| "medium".to_string()),
                    status: "running".to_string(),
                    probe_candidate_count: *candidate_count,
                    probe_completed_count: 0,
                    citation_count: 0,
                    domain_count: 0,
                    tool_call_count: 0,
                    quality_score: None,
                    error_code: None,
                    error_message: None,
                    detail: Some(serde_json::json!({
                        "attempt": attempt,
                        "candidateCount": candidate_count,
                        "source": "probe_scheduler",
                        "subtaskRunId": subtask_run_id,
                    })),
                },
            )
            .await;
        }

        if pm_probe_kernel_should_run(
            parallel_subtask_enabled,
            attempt,
            parallel_subtask_max_attempts,
            probe_candidates.len(),
        ) {
            retrieve_route = if probe_candidates.len() > 1 {
                "parallel_probe_select"
            } else {
                "focused_probe_select"
            };
            on_stage(
                "retrieve",
                "running",
                attempt,
                Some(serde_json::json!({
                    "message": pm_probe_progress_message(0, probe_candidates.len()),
                    "probeCandidateCount": probe_candidates.len(),
                    "targetSubtask": active_subtask_focus.clone(),
                })),
            );
            let concurrency_limit = parallel_subtask_max_concurrency
                .min(probe_candidates.len())
                .max(1);
            let user_id_owned = user_id.to_string();
            let tenant_id_owned = tenant_id.to_string();
            let model_owned = model.to_string();
            let question_owned = user_message.to_string();
            let (probe_native_runtime, probe_search_context) = tokio::join!(
                resolve_pm_native_search_runtime(state, tenant_id, model),
                crate::routes::search_orchestrator_runtime::prepare_unified_search_context(
                    state,
                    tenant_id,
                    Some(model),
                    true,
                    true,
                ),
            );
            let probe_stream = futures_util::stream::iter(probe_candidates.iter().cloned())
                .map(|candidate| {
                    let manager_clone = manager.clone();
                    let state_clone = state.clone();
                    let user_id = user_id_owned.clone();
                    let tenant_id = tenant_id_owned.clone();
                    let model = model_owned.clone();
                    let question = question_owned.clone();
                    let native_runtime = probe_native_runtime.clone();
                    let prepared_context = Some(probe_search_context.clone());
                    async move {
                        run_pm_probe_turn(
                            state_clone,
                            manager_clone,
                            &user_id,
                            &tenant_id,
                            &model,
                            candidate,
                            &question,
                            native_runtime,
                            prepared_context,
                        )
                        .await
                    }
                })
                .buffer_unordered(concurrency_limit);
            tokio::pin!(probe_stream);
            let mut completed = 0usize;
            while let Some(outcome) = probe_stream.next().await {
                if let Some(subtask_key) = resolve_subtask_runtime_key(&outcome) {
                    let subtask_run_id =
                        if let Some(existing_id) = subtask_run_ids.get(&subtask_key).copied() {
                            existing_id
                        } else {
                            let fallback_title = outcome
                                .subtask_title
                                .clone()
                                .unwrap_or_else(|| subtask_key.clone());
                            let inserted = upsert_pm_subtask_run(
                                state.telemetry_db(),
                                &PmSubtaskRunUpsertPayload {
                                    run_id: run_id.clone(),
                                    task_id: cancel_task_id.map(std::string::ToString::to_string),
                                    tenant_id: tenant_id.to_string(),
                                    user_id: user_id.to_string(),
                                    session_id: session_id.to_string(),
                                    subtask_key: subtask_key.clone(),
                                    subtask_id: outcome.subtask_id.clone(),
                                    title: fallback_title,
                                    goal: outcome.subtask_goal.clone(),
                                    deliverable: outcome.subtask_deliverable.clone(),
                                    required_evidence_type: outcome
                                        .subtask_required_evidence_type
                                        .clone(),
                                    priority: outcome
                                        .subtask_priority
                                        .clone()
                                        .unwrap_or_else(|| "medium".to_string()),
                                    status: "running".to_string(),
                                    probe_candidate_count: attempt_subtask_candidate_counts
                                        .get(&subtask_key)
                                        .copied()
                                        .unwrap_or(0),
                                    probe_completed_count: 0,
                                    citation_count: 0,
                                    domain_count: 0,
                                    tool_call_count: 0,
                                    quality_score: None,
                                    error_code: None,
                                    error_message: None,
                                    detail: Some(serde_json::json!({
                                        "attempt": attempt,
                                        "source": "probe_outcome_bootstrap",
                                    })),
                                },
                            )
                            .await;
                            if let Some(inserted_id) = inserted {
                                subtask_run_ids.insert(subtask_key.clone(), inserted_id);
                                inserted_id
                            } else {
                                0
                            }
                        };
                    if subtask_run_id > 0 {
                        let attempt_seq_entry = subtask_probe_attempt_seq
                            .entry(subtask_key.clone())
                            .or_insert(0);
                        *attempt_seq_entry = attempt_seq_entry.saturating_add(1);
                        let attempt_seq = *attempt_seq_entry;
                        let quality_score = outcome.quality.as_ref().map(|quality| {
                            (quality.triad_coverage * 0.7 + if quality.passed { 0.3 } else { 0.0 })
                                .clamp(0.0, 1.0)
                        });
                        let citation_count = outcome
                            .quality
                            .as_ref()
                            .map(|quality| quality.citation_count)
                            .unwrap_or(0);
                        let domain_count = outcome
                            .quality
                            .as_ref()
                            .map(|quality| quality.domain_count)
                            .unwrap_or(0);
                        let tool_call_count = outcome
                            .quality
                            .as_ref()
                            .map(|quality| quality.tool_call_count)
                            .or_else(|| outcome.turn.as_ref().map(|turn| turn.tool_calls.len()))
                            .or_else(|| {
                                outcome
                                    .diagnostic_turn
                                    .as_ref()
                                    .map(|turn| turn.tool_calls.len())
                            })
                            .unwrap_or(0);
                        let is_success = outcome.turn.is_some();
                        let attempt_status = if is_success { "completed" } else { "failed" };
                        let error_text = outcome.error.clone();
                        let error_code = error_text
                            .as_deref()
                            .map(classify_pm_runtime_error_code)
                            .map(std::string::ToString::to_string);
                        let route_key = outcome.route_id.clone();
                        let route_channel = outcome.route_channel.clone();
                        let variant = Some(outcome.variant.clone());
                        let _ = upsert_pm_subtask_attempt(
                            state.telemetry_db(),
                            &PmSubtaskAttemptUpsertPayload {
                                subtask_run_id,
                                run_id: run_id.clone(),
                                subtask_key: subtask_key.clone(),
                                attempt_no: attempt_seq,
                                attempt_key: format!(
                                    "{}-{}-{}",
                                    attempt,
                                    attempt_seq,
                                    sha256_hex(
                                        format!(
                                            "{}|{}|{}",
                                            outcome.variant,
                                            outcome.route_id.clone().unwrap_or_default(),
                                            outcome.route_channel.clone().unwrap_or_default(),
                                        )
                                        .as_str()
                                    )
                                    .chars()
                                    .take(12)
                                    .collect::<String>()
                                ),
                                variant: variant.clone(),
                                route_key: route_key.clone(),
                                route_channel: route_channel.clone(),
                                status: attempt_status.to_string(),
                                elapsed_ms: outcome.elapsed_ms,
                                citation_count,
                                domain_count,
                                tool_call_count,
                                quality_score,
                                error_code: error_code.clone(),
                                error_message: error_text.clone(),
                                detail: Some(serde_json::json!({
                                    "attempt": attempt,
                                    "selectedRoute": route_key.clone(),
                                    "selectedRouteChannel": route_channel.clone(),
                                    "subtaskTitle": outcome.subtask_title.clone(),
                                })),
                            },
                        )
                        .await;
                        let ledger_turn =
                            outcome.turn.as_ref().or(outcome.diagnostic_turn.as_ref());
                        if let Some(ledger_turn) = ledger_turn {
                            let slot_seq =
                                attempt.saturating_mul(10_000).saturating_add(attempt_seq);
                            let slot_detail = serde_json::json!({
                                "attempt": attempt,
                                "subtaskAttempt": attempt_seq,
                                "subtaskKey": subtask_key.clone(),
                                "subtaskTitle": outcome.subtask_title.clone(),
                                "selectedRoute": route_key.clone(),
                                "selectedRouteChannel": route_channel.clone(),
                                "variant": variant.clone(),
                                "diagnosticOnly": outcome.turn.is_none(),
                                "error": error_text.clone(),
                                "quality": outcome.quality.as_ref().map(|quality| serde_json::json!({
                                    "passed": quality.passed,
                                    "citationCount": quality.citation_count,
                                    "domainCount": quality.domain_count,
                                    "toolCallCount": quality.tool_call_count,
                                })),
                            });
                            persist_pm_source_slot_and_tool_ledger(
                                state.pm_telemetry(),
                                &run_id,
                                slot_seq,
                                route_key.as_deref(),
                                route_channel.as_deref(),
                                Some(outcome.variant.as_str()),
                                attempt_status,
                                outcome.elapsed_ms,
                                error_code.as_deref(),
                                error_text.as_deref(),
                                Some(&slot_detail),
                                &ledger_turn.tool_calls,
                            )
                            .await;
                        }
                        let _ = upsert_pm_subtask_run(
                            state.telemetry_db(),
                            &PmSubtaskRunUpsertPayload {
                                run_id: run_id.clone(),
                                task_id: cancel_task_id.map(std::string::ToString::to_string),
                                tenant_id: tenant_id.to_string(),
                                user_id: user_id.to_string(),
                                session_id: session_id.to_string(),
                                subtask_key: subtask_key.clone(),
                                subtask_id: outcome.subtask_id.clone(),
                                title: outcome
                                    .subtask_title
                                    .clone()
                                    .or_else(|| {
                                        subtask_meta_map
                                            .get(&subtask_key)
                                            .map(|meta| meta.title.clone())
                                    })
                                    .unwrap_or_else(|| subtask_key.clone()),
                                goal: outcome.subtask_goal.clone().or_else(|| {
                                    subtask_meta_map
                                        .get(&subtask_key)
                                        .and_then(|meta| meta.goal.clone())
                                }),
                                deliverable: outcome.subtask_deliverable.clone().or_else(|| {
                                    subtask_meta_map
                                        .get(&subtask_key)
                                        .and_then(|meta| meta.deliverable.clone())
                                }),
                                required_evidence_type: outcome
                                    .subtask_required_evidence_type
                                    .clone()
                                    .or_else(|| {
                                        subtask_meta_map
                                            .get(&subtask_key)
                                            .and_then(|meta| meta.required_evidence_type.clone())
                                    }),
                                priority: outcome
                                    .subtask_priority
                                    .clone()
                                    .or_else(|| {
                                        subtask_meta_map
                                            .get(&subtask_key)
                                            .map(|meta| meta.priority.clone())
                                    })
                                    .unwrap_or_else(|| "medium".to_string()),
                                status: if is_success {
                                    "completed".to_string()
                                } else {
                                    "failed".to_string()
                                },
                                probe_candidate_count: attempt_subtask_candidate_counts
                                    .get(&subtask_key)
                                    .copied()
                                    .unwrap_or(0),
                                probe_completed_count: if is_success { 1 } else { 0 },
                                citation_count,
                                domain_count,
                                tool_call_count,
                                quality_score,
                                error_code,
                                error_message: error_text,
                                detail: Some(serde_json::json!({
                                    "attempt": attempt,
                                    "selectedRoute": route_key.clone(),
                                    "selectedRouteChannel": route_channel.clone(),
                                    "variant": variant.clone(),
                                })),
                            },
                        )
                        .await;
                    }
                }
                probe_outcomes.push(outcome);
                completed += 1;
                on_stage(
                    "retrieve",
                    "running",
                    attempt,
                    Some(serde_json::json!({
                        "message": pm_probe_progress_message(completed, probe_candidates.len()),
                        "probeCandidateCount": probe_candidates.len(),
                        "probeCompletedCount": completed,
                        "targetSubtask": active_subtask_focus.clone(),
                    })),
                );
            }
        }

        if !probe_outcomes.is_empty() {
            let mut best_score = i64::MIN;
            let probe_admission_for_selection =
                admit_pm_external_evidence(user_message, &probe_outcomes, &[]);
            let selectable_probe_outcomes = if probe_admission_for_selection
                .accepted_probe_outcomes
                .is_empty()
            {
                probe_outcomes.as_slice()
            } else {
                probe_admission_for_selection
                    .accepted_probe_outcomes
                    .as_slice()
            };
            if probe_admission_for_selection.examined_evidence_count > 0 {
                on_stage(
                    "retrieve",
                    "running",
                    attempt,
                    Some(serde_json::json!({
                        "message": "external evidence admission checked",
                        "evidenceAdmission": probe_admission_for_selection.to_json(),
                    })),
                );
            }
            for outcome in selectable_probe_outcomes {
                let (Some(turn), Some(quality)) = (&outcome.turn, &outcome.quality) else {
                    continue;
                };
                let score = score_pm_probe_quality(quality);
                if score > best_score {
                    best_score = score;
                    selected_probe_variant = Some(outcome.variant.clone());
                    selected_probe_route_id = outcome.route_id.clone();
                    selected_probe_route_channel = outcome.route_channel.clone();
                    selected_probe_score = Some(score);
                    selected_probe_turn = Some(turn.clone());
                }
            }
        }

        // The unified probe traverses configured Search, model-native search,
        // MCP search/browser, and local/RAG. If every layer is absent, letting
        // the model guess URLs for WebFetch only creates slow, repetitive and
        // weak evidence. Converge immediately to a no-tool expert synthesis.
        if probe_kernel_active && pm_probe_outcomes_confirm_no_retrieval_route(&probe_outcomes) {
            on_stage(
                "retrieve",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "degraded": true,
                    "reason": "all_retrieval_layers_unavailable",
                    "message": "No configured, native, MCP, or local retrieval route is available; synthesizing from first-party context and expert reasoning.",
                    "probeCount": probe_outcomes.len(),
                })),
            );
            on_stage(
                "synthesize",
                "running",
                attempt,
                Some(serde_json::json!({
                    "mode": "expert_without_external_search",
                    "reason": "all_retrieval_layers_unavailable",
                })),
            );
            let turn = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                &accumulated_observed_tool_calls,
                answer_delta.clone(),
            )
            .await
            .unwrap_or_else(|error| {
                build_pm_local_strategy_synthesis_turn(
                    session_id,
                    model,
                    user_message,
                    &format!("expert synthesis failed after retrieval became unavailable: {error}"),
                    attempt,
                    &accumulated_observed_tool_calls,
                )
            });
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&turn),
                "all_retrieval_layers_unavailable",
                "External retrieval was unavailable; delivered a non-empty expert synthesis from first-party context and model knowledge.",
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": turn.text.chars().count(),
                    "qualityGatePassed": quality.passed,
                    "degraded": true,
                    "reason": "all_retrieval_layers_unavailable",
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                turn,
                quality,
            )
            .await;
        }

        let mut source_quota_exhausted = false;
        if selected_probe_variant.is_none() || selected_probe_route_id.is_none() {
            let (
                fallback_variant,
                fallback_route_id,
                fallback_route_channel,
                _fallback_execution_channel,
                fallback_quota_exhausted,
            ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                &plan_query_variants,
                &plan_enabled_routes,
                attempt,
                &route_usage_counts,
                &route_blocklist,
                source_quota_limit,
                &used_retrieval_keys,
            );
            source_quota_exhausted = fallback_quota_exhausted;
            if selected_probe_variant.is_none() {
                selected_probe_variant = fallback_variant;
            }
            if selected_probe_route_id.is_none() {
                selected_probe_route_id = fallback_route_id;
                selected_probe_route_channel = fallback_route_channel;
            }
        }

        if !source_quota_exhausted
            && (is_pm_route_over_quota(
                &route_usage_counts,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                source_quota_limit,
            ) || is_pm_route_blocked(
                &route_blocklist,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
            ))
        {
            let (
                quota_variant,
                quota_route_id,
                quota_route_channel,
                _quota_exec_channel,
                quota_exhausted,
            ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                &plan_query_variants,
                &plan_enabled_routes,
                attempt,
                &route_usage_counts,
                &route_blocklist,
                source_quota_limit,
                &used_retrieval_keys,
            );
            source_quota_exhausted = quota_exhausted;
            if selected_probe_variant.is_none() {
                selected_probe_variant = quota_variant;
            }
            if !source_quota_exhausted {
                selected_probe_route_id = quota_route_id;
                selected_probe_route_channel = quota_route_channel;
            }
        }

        if source_quota_exhausted {
            let source_exhaustion_reason =
                pm_source_exhaustion_reason_code(classify_pm_source_exhaustion_reason(
                    &plan_enabled_routes,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                ));
            on_stage(
                "retrieve",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "error": "source_quota_exhausted",
                    "reason": source_exhaustion_reason,
                    "maxCallsPerSource": source_quota_limit,
                    "selectedVariant": selected_probe_variant.clone(),
                    "selectedRoute": selected_probe_route_id.clone(),
                    "selectedRouteChannel": selected_probe_route_channel.clone(),
                    "routeUsageCounts": route_usage_counts.clone(),
                    "routeBlocklist": route_blocklist.clone(),
                })),
            );
            if let Some(turn) = last_usable_turn.clone() {
                let quality = degrade_pm_quality_with_reason(
                    last_usable_quality
                        .clone()
                        .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                    source_exhaustion_reason,
                    "All enabled sources reached per-source quota; returned the best answer collected so far.",
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "source_quota_exhausted_partial_answer_kept"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                &accumulated_observed_tool_calls,
                answer_delta.clone(),
            )
            .await
            {
                let quality = degrade_pm_quality_with_reason(
                    evaluate_pm_answer_quality(&turn),
                    source_exhaustion_reason,
                    "All enabled sources reached per-source quota; forced synthesis from available evidence.",
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "source_quota_exhausted_force_synthesize"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            let fallback_tool_summary = last_usable_turn
                .as_ref()
                .and_then(|turn| {
                    if turn.tool_calls.is_empty() {
                        None
                    } else {
                        Some(build_pm_tool_summary_value(&turn.tool_calls))
                    }
                })
                .or_else(|| {
                    if accumulated_observed_tool_calls.is_empty() {
                        None
                    } else {
                        Some(build_pm_tool_summary_value(
                            &accumulated_observed_tool_calls,
                        ))
                    }
                });
            let emergency_turn = TurnResult {
                session_id: session_id.to_string(),
                text: build_pm_emergency_conclusion_text(
                    user_message,
                    "all enabled sources reached per-source quota",
                    attempt,
                    fallback_tool_summary.as_ref(),
                    last_usable_quality.as_ref(),
                    Some(&probe_outcomes),
                ),
                tool_calls: accumulated_observed_tool_calls.clone(),
                usage: TokenUsageRecord {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    total_tokens: 0,
                    estimated_cost_usd: 0.0,
                    model: model.to_string(),
                },
                compacted: None,
                iterations: 1,
                metadata: None,
                hot_reloaded: false,
                thinking: None,
            };
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&emergency_turn),
                "source_quota_exhausted_emergency_conclusion",
                "Source quotas exhausted and forced synthesis failed; emitted emergency conclusion.",
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": emergency_turn.text.chars().count(),
                    "qualityGatePassed": false,
                    "reason": "source_quota_exhausted_emergency_conclusion"
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                emergency_turn,
                quality,
            )
            .await;
        }

        let selected_route_circuit_key = pm_retrieve_circuit_route_key(
            selected_probe_route_id.as_deref(),
            selected_probe_route_channel.as_deref(),
        );
        if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
            if let Err(reason) = pm_retrieve_circuit_allow(db, tenant_id, circuit_key).await {
                if plan_route_count <= 1 {
                    on_stage(
                        "retrieve",
                        "running",
                        attempt,
                        Some(serde_json::json!({
                            "message": "route circuit is open but only one route is enabled; bypassing circuit gate and continuing retrieval",
                            "reason": reason,
                            "selectedVariant": selected_probe_variant.clone(),
                            "selectedRoute": selected_probe_route_id.clone(),
                            "selectedRouteChannel": selected_probe_route_channel.clone(),
                        })),
                    );
                } else {
                    on_stage(
                        "retrieve",
                        "failed",
                        attempt,
                        Some(serde_json::json!({
                            "error": "route_circuit_open",
                            "reason": reason.clone(),
                            "selectedVariant": selected_probe_variant.clone(),
                            "selectedRoute": selected_probe_route_id.clone(),
                            "selectedRouteChannel": selected_probe_route_channel.clone(),
                        })),
                    );
                    if attempt < max_attempts {
                        let next_attempt = attempt + 1;
                        let (
                            next_variant,
                            next_route_id,
                            next_route_channel,
                            _next_execution_channel,
                            next_source_quota_exhausted,
                        ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                            &plan_query_variants,
                            &plan_enabled_routes,
                            next_attempt,
                            &route_usage_counts,
                            &route_blocklist,
                            source_quota_limit,
                            &used_retrieval_keys,
                        );
                        if !next_source_quota_exhausted {
                            on_stage(
                                "retry_repair",
                                "running",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": "circuit_open_failover_next_source",
                                    "reason": "route_circuit_open",
                                    "message": "selected source is temporarily unhealthy, switching route immediately",
                                    "nextVariant": next_variant.clone(),
                                    "nextRoute": next_route_id.clone(),
                                    "nextRouteChannel": next_route_channel.clone(),
                                })),
                            );
                            current_message = wrap_pm_research_prompt(
                                session_source,
                                build_pm_retrieve_prompt(
                                    user_message,
                                    &plan,
                                    next_variant.as_deref(),
                                    next_route_id.as_deref(),
                                    next_attempt,
                                    &runtime_budget,
                                    if next_route_channel
                                        .as_deref()
                                        .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                                    {
                                        runtime_budget.source_slot_browser_secs
                                    } else {
                                        runtime_budget.source_slot_search_secs
                                    },
                                    &merge_blocked_domains(
                                        blocked_domains_from_usage(
                                            &domain_usage_counts,
                                            domain_quota_limit,
                                        ),
                                        &open_domain_circuit_keys,
                                    ),
                                ),
                            );
                            current_attempt_strategy = None;
                            on_stage(
                                "retry_repair",
                                "completed",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": "circuit_open_failover_next_source",
                                    "nextVariant": next_variant,
                                    "nextRoute": next_route_id,
                                    "nextRouteChannel": next_route_channel,
                                })),
                            );
                            attempt = next_attempt;
                            continue;
                        }
                    }
                    if let Some(turn) = last_usable_turn.clone() {
                        let quality = degrade_pm_quality_with_reason(
                            last_usable_quality
                                .clone()
                                .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                            "route_circuit_open",
                            "All remaining routes were temporarily unhealthy; returned the best answer collected so far.",
                        );
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": turn.text.chars().count(),
                                "qualityGatePassed": false,
                                "reason": "route_circuit_open_partial_answer_kept"
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                        manager.clone(),
                        session_id,
                        session_source,
                        user_message,
                        &probe_outcomes,
                        attempt,
                        &accumulated_observed_tool_calls,
                        answer_delta.clone(),
                    )
                    .await
                    {
                        let quality = degrade_pm_quality_with_reason(
                            evaluate_pm_answer_quality(&turn),
                            "route_circuit_open_force_synthesize",
                            "Selected routes were temporarily unhealthy; forced synthesis from available evidence.",
                        );
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": turn.text.chars().count(),
                                "qualityGatePassed": false,
                                "reason": "route_circuit_open_force_synthesize"
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    let fallback_tool_summary = last_usable_turn
                        .as_ref()
                        .and_then(|turn| {
                            if turn.tool_calls.is_empty() {
                                None
                            } else {
                                Some(build_pm_tool_summary_value(&turn.tool_calls))
                            }
                        })
                        .or_else(|| {
                            if accumulated_observed_tool_calls.is_empty() {
                                None
                            } else {
                                Some(build_pm_tool_summary_value(
                                    &accumulated_observed_tool_calls,
                                ))
                            }
                        });
                    let emergency_turn = TurnResult {
                        session_id: session_id.to_string(),
                        text: build_pm_emergency_conclusion_text(
                            user_message,
                            "retrieval routes temporarily unhealthy (circuit open)",
                            attempt,
                            fallback_tool_summary.as_ref(),
                            last_usable_quality.as_ref(),
                            Some(&probe_outcomes),
                        ),
                        tool_calls: accumulated_observed_tool_calls.clone(),
                        usage: TokenUsageRecord {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            total_tokens: 0,
                            estimated_cost_usd: 0.0,
                            model: model.to_string(),
                        },
                        compacted: None,
                        iterations: 1,
                        metadata: None,
                        hot_reloaded: false,
                        thinking: None,
                    };
                    let quality = degrade_pm_quality_with_reason(
                        evaluate_pm_answer_quality(&emergency_turn),
                        "route_circuit_open_emergency_conclusion",
                        "Route circuit remained open and forced synthesis failed; emitted emergency conclusion.",
                    );
                    on_stage(
                        "synthesize",
                        "completed",
                        attempt,
                        Some(serde_json::json!({
                            "answerLength": emergency_turn.text.chars().count(),
                            "qualityGatePassed": false,
                            "reason": "route_circuit_open_emergency_conclusion"
                        })),
                    );
                    return finalize_pm_orchestration_result(
                        state.telemetry_db(),
                        tenant_id,
                        &run_id,
                        session_id,
                        emergency_turn,
                        quality,
                    )
                    .await;
                }
            }
        }

        // Probe-only retrieval attempts are used for route scoring and evidence sampling;
        // they should not consume per-source execution quota.
        if pm_should_consume_source_quota(probe_kernel_active, best_turn_adopted) {
            if let Some(route_key) = pm_route_usage_key(
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
            ) {
                let entry = route_usage_counts.entry(route_key).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }

        on_stage(
            "retrieve",
            "running",
            attempt,
            Some(serde_json::json!({
                "message": "正在按统一联网链路检索证据",
                "humanSummary": "正在按 Search Orchestrator 检索：有健康 Search 扩展时优先使用；否则优先模型原生联网；参考质量不够时切换后续层。",
                "selectedVariant": selected_probe_variant.clone(),
                "selectedRoute": selected_probe_route_id.clone(),
                "selectedRouteChannel": selected_probe_route_channel.clone(),
                "nativeWebSearch": {
                    "preferred": false,
                    "status": "requested",
                    "fallbackOrder": ["search_extensions_if_configured", "model_native_streaming", "mcp_search", "rag_local"]
                },
            })),
        );
        pm_mark_selected_retrieval_used(
            selected_probe_variant.as_deref(),
            selected_probe_route_id.as_deref(),
            selected_probe_route_channel.as_deref(),
            &mut used_retrieval_keys,
            &mut used_retrieval_variants,
        );

        if attempt == 1 {
            current_message = wrap_pm_research_prompt(
                session_source,
                build_pm_retrieve_prompt(
                    user_message,
                    &plan,
                    selected_probe_variant.as_deref(),
                    selected_probe_route_id.as_deref(),
                    attempt,
                    &runtime_budget,
                    if selected_probe_route_channel
                        .as_deref()
                        .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                    {
                        runtime_budget.source_slot_browser_secs
                    } else {
                        runtime_budget.source_slot_search_secs
                    },
                    &merge_blocked_domains(
                        blocked_domains_from_usage(&domain_usage_counts, domain_quota_limit),
                        &open_domain_circuit_keys,
                    ),
                ),
            );
            current_attempt_strategy = None;
        }

        let adaptive_source_slot_cap = if remaining_pipeline_secs > min_synthesize_window_secs {
            remaining_pipeline_secs
                .saturating_sub(min_synthesize_window_secs)
                .min(source_slot_budget_secs.max(1))
                .max(12)
        } else {
            12
        };
        let source_slot_timeout_secs = current_attempt_strategy
            .map(|strategy| pm_source_slot_timeout_for_strategy(strategy, &runtime_budget))
            .unwrap_or_else(|| {
                if selected_probe_route_channel
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                {
                    runtime_budget.source_slot_browser_secs
                } else {
                    runtime_budget.source_slot_search_secs
                }
            })
            .min(adaptive_source_slot_cap);

        let turn = if parallel_subtask_use_best_turn && attempt <= parallel_subtask_max_attempts {
            let merge_admission = admit_pm_external_evidence(user_message, &probe_outcomes, &[]);
            let merge_probe_outcomes = if merge_admission.accepted_probe_outcomes.is_empty() {
                probe_outcomes.as_slice()
            } else {
                merge_admission.accepted_probe_outcomes.as_slice()
            };
            let merged_probe_turn =
                merge_pm_probe_turns(merge_probe_outcomes, selected_probe_turn.as_ref());
            if let Some(probe_turn) = merged_probe_turn {
                best_turn_adopted = true;
                let observed_tool_calls = merge_pm_tool_calls_unique(
                    &accumulated_observed_tool_calls,
                    &probe_turn.tool_calls,
                );
                accumulated_observed_tool_calls = observed_tool_calls.clone();
                on_stage(
                    "retrieve",
                    "running",
                    attempt,
                    Some(serde_json::json!({
                        "message": "parallel subtasks completed; synthesizing user answer from merged probe evidence",
                        "selectedVariant": selected_probe_variant.clone(),
                        "selectedRoute": selected_probe_route_id.clone(),
                        "selectedRouteChannel": selected_probe_route_channel.clone(),
                        "selectedScore": selected_probe_score,
                        "subtaskKernel": true,
                        "forceSynthesize": true,
                        "bestTurnAdopted": true,
                        "probeOnlyForRouting": false,
                        "mergedProbeCount": merge_probe_outcomes.iter().filter(|x| x.turn.is_some()).count(),
                        "evidenceAdmission": merge_admission.to_json(),
                    })),
                );
                match run_pm_force_synthesize_fallback_turn_with_observed_tools(
                    manager.clone(),
                    session_id,
                    session_source,
                    user_message,
                    merge_probe_outcomes,
                    attempt,
                    &observed_tool_calls,
                    None,
                )
                .await
                {
                    Ok(synth_turn) => synth_turn,
                    Err(error) => {
                        tracing::warn!(
                            session_id = %session_id,
                            attempt = attempt,
                            "parallel subtask merged-probe synthesize failed; fallback to tool-only retry path: {}",
                            error
                        );
                        let mut tool_only_probe_turn = probe_turn.clone();
                        tool_only_probe_turn.text.clear();
                        tool_only_probe_turn.tool_calls = observed_tool_calls;
                        tool_only_probe_turn
                    }
                }
            } else {
                let mut live_stage_callback =
                    |stage: &str,
                     status: &str,
                     stage_attempt: usize,
                     detail: Option<serde_json::Value>| {
                        on_stage(stage, status, stage_attempt, detail)
                    };
                match run_pm_retrieve_turn_with_live_events(
                    manager.clone(),
                    session_id,
                    current_message.clone(),
                    source_slot_timeout_secs,
                    "retrieve source slot",
                    attempt,
                    selected_probe_variant.as_deref(),
                    selected_probe_route_id.as_deref(),
                    selected_probe_route_channel.as_deref(),
                    runtime_budget.retrieve_max_tool_calls,
                    &mut live_stage_callback,
                )
                .await
                {
                    Ok((t, observed_tool_calls)) => {
                        let observed_tool_calls = merge_pm_tool_calls_unique(
                            &accumulated_observed_tool_calls,
                            &observed_tool_calls,
                        );
                        accumulated_observed_tool_calls = observed_tool_calls.clone();
                        merge_pm_turn_with_observed_tool_calls(t, &observed_tool_calls)
                    }
                    Err((e, observed_tool_calls)) => {
                        let observed_tool_calls = merge_pm_tool_calls_unique(
                            &accumulated_observed_tool_calls,
                            &observed_tool_calls,
                        );
                        accumulated_observed_tool_calls = observed_tool_calls.clone();
                        let err_text = e.to_string();
                        let blocked_route = record_pm_route_failure_and_maybe_block(
                            &mut route_fail_streaks,
                            &mut route_blocklist,
                            selected_probe_route_id.as_deref(),
                            selected_probe_route_channel.as_deref(),
                            route_fail_block_threshold,
                        );
                        if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                            pm_retrieve_circuit_report(
                                db,
                                tenant_id,
                                selected_probe_route_channel.as_deref(),
                                circuit_key,
                                false,
                                Some(classify_pm_runtime_error_code(&err_text)),
                                Some(&err_text),
                            )
                            .await;
                        }
                        let is_timeout = err_text.contains("timed out after");
                        let partial_evidence_admission = admit_pm_external_evidence(
                            user_message,
                            &probe_outcomes,
                            &observed_tool_calls,
                        );
                        let has_usable_partial_evidence =
                            is_timeout && partial_evidence_admission.external_evidence_usable;
                        let slot_status = if has_usable_partial_evidence {
                            "partial_success"
                        } else if is_timeout {
                            "timed_out"
                        } else {
                            "failed"
                        };
                        let slot_error_code = if is_timeout {
                            Some("timeout")
                        } else {
                            Some("runtime_error")
                        };
                        let slot_detail = serde_json::json!({
                            "error": err_text.clone(),
                            "selectedVariant": selected_probe_variant.clone(),
                            "selectedRoute": selected_probe_route_id.clone(),
                            "selectedRouteChannel": selected_probe_route_channel.clone(),
                            "route": retrieve_route,
                            "routeTemporarilyBlocked": blocked_route.is_some(),
                            "blockedRouteKey": blocked_route,
                        });
                        persist_pm_source_slot_and_tool_ledger(
                            state.pm_telemetry(),
                            &run_id,
                            attempt,
                            selected_probe_route_id.as_deref(),
                            selected_probe_route_channel.as_deref(),
                            selected_probe_variant.as_deref(),
                            slot_status,
                            Some(
                                retrieve_started
                                    .elapsed()
                                    .as_millis()
                                    .try_into()
                                    .unwrap_or(u64::MAX),
                            ),
                            slot_error_code,
                            Some(
                                slot_detail
                                    .get("error")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("runtime_error"),
                            ),
                            Some(&slot_detail),
                            &observed_tool_calls,
                        )
                        .await;
                        upsert_pm_provider_health(
                            state.telemetry_db(),
                            tenant_id,
                            model,
                            "model",
                            has_usable_partial_evidence,
                            Some(
                                retrieve_started
                                    .elapsed()
                                    .as_millis()
                                    .try_into()
                                    .unwrap_or(u64::MAX),
                            ),
                            (!has_usable_partial_evidence)
                                .then(|| classify_pm_runtime_error_code(&err_text)),
                        )
                        .await;
                        on_stage(
                            "retrieve",
                            if has_usable_partial_evidence {
                                "completed"
                            } else {
                                "failed"
                            },
                            attempt,
                            Some(serde_json::json!({
                                "error": err_text,
                                "partialSuccess": has_usable_partial_evidence,
                                "evidenceAdmission": partial_evidence_admission.to_json(),
                                "selectedVariant": selected_probe_variant,
                                "selectedRoute": selected_probe_route_id,
                                "selectedRouteChannel": selected_probe_route_channel,
                                "sourceSlotTimeoutSecs": source_slot_timeout_secs,
                                "attemptElapsedMs": retrieve_started.elapsed().as_millis(),
                            })),
                        );
                        if has_usable_partial_evidence {
                            let turn = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                                manager.clone(),
                                session_id,
                                session_source,
                                user_message,
                                &probe_outcomes,
                                attempt,
                                &observed_tool_calls,
                                answer_delta.clone(),
                            )
                            .await?;
                            let turn =
                                merge_pm_turn_with_observed_tool_calls(turn, &observed_tool_calls);
                            let quality = evaluate_pm_answer_quality(&turn);
                            on_stage(
                                "synthesize",
                                "completed",
                                attempt,
                                Some(serde_json::json!({
                                    "answerLength": turn.text.chars().count(),
                                    "qualityGatePassed": quality.passed,
                                    "reason": "retrieve_timeout_partial_evidence_synthesized"
                                })),
                            );
                            return finalize_pm_orchestration_result(
                                state.telemetry_db(),
                                tenant_id,
                                &run_id,
                                session_id,
                                turn,
                                quality,
                            )
                            .await;
                        }
                        if attempt < max_attempts
                            && !(probe_kernel_active
                                && pm_probe_outcomes_confirm_retrieval_discovery_exhausted(
                                    &probe_outcomes,
                                ))
                        {
                            let next_attempt = attempt + 1;
                            if is_timeout {
                                let (
                                    next_variant,
                                    next_route_id,
                                    next_route_channel,
                                    _next_execution_channel,
                                    next_source_quota_exhausted,
                                ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                                    &plan_query_variants,
                                    &plan_enabled_routes,
                                    next_attempt,
                                    &route_usage_counts,
                                    &route_blocklist,
                                    source_quota_limit,
                                    &used_retrieval_keys,
                                );
                                if next_source_quota_exhausted {
                                    let source_exhaustion_reason = pm_source_exhaustion_reason_code(
                                        classify_pm_source_exhaustion_reason(
                                            &plan_enabled_routes,
                                            &route_usage_counts,
                                            &route_blocklist,
                                            source_quota_limit,
                                        ),
                                    );
                                    on_stage(
                                        "retry_repair",
                                        "failed",
                                        next_attempt,
                                        Some(serde_json::json!({
                                            "strategy": "timeout_fast_failover_next_source",
                                            "reason": source_exhaustion_reason,
                                            "maxCallsPerSource": source_quota_limit,
                                            "nextVariant": next_variant.clone(),
                                            "nextRoute": next_route_id.clone(),
                                            "nextRouteChannel": next_route_channel.clone(),
                                            "routeUsageCounts": route_usage_counts.clone(),
                                            "routeBlocklist": route_blocklist.clone(),
                                        })),
                                    );
                                    if let Some(turn) = last_usable_turn.clone() {
                                        let quality = degrade_pm_quality_with_reason(
                                            last_usable_quality.clone().unwrap_or_else(|| {
                                                evaluate_pm_answer_quality(&turn)
                                            }),
                                            source_exhaustion_reason,
                                            "All enabled sources reached per-source quota after timeout failover.",
                                        );
                                        on_stage(
                                            "synthesize",
                                            "completed",
                                            attempt,
                                            Some(serde_json::json!({
                                                "answerLength": turn.text.chars().count(),
                                                "qualityGatePassed": false,
                                                "reason": "source_quota_exhausted_partial_answer_kept"
                                            })),
                                        );
                                        return finalize_pm_orchestration_result(
                                            state.telemetry_db(),
                                            tenant_id,
                                            &run_id,
                                            session_id,
                                            turn,
                                            quality,
                                        )
                                        .await;
                                    }
                                    if let Ok(turn) =
                                        run_pm_force_synthesize_fallback_turn_with_observed_tools(
                                            manager.clone(),
                                            session_id,
                                            session_source,
                                            user_message,
                                            &probe_outcomes,
                                            next_attempt,
                                            &observed_tool_calls,
                                            answer_delta.clone(),
                                        )
                                        .await
                                    {
                                        let quality = degrade_pm_quality_with_reason(
                                            evaluate_pm_answer_quality(&turn),
                                            source_exhaustion_reason,
                                            "All enabled sources reached per-source quota after timeout failover; forced synthesis.",
                                        );
                                        on_stage(
                                            "synthesize",
                                            "completed",
                                            next_attempt,
                                            Some(serde_json::json!({
                                                "answerLength": turn.text.chars().count(),
                                                "qualityGatePassed": false,
                                                "reason": "source_quota_exhausted_force_synthesize"
                                            })),
                                        );
                                        return finalize_pm_orchestration_result(
                                            state.telemetry_db(),
                                            tenant_id,
                                            &run_id,
                                            session_id,
                                            turn,
                                            quality,
                                        )
                                        .await;
                                    }
                                }
                                on_stage(
                                    "retry_repair",
                                    "running",
                                    next_attempt,
                                    Some(serde_json::json!({
                                        "strategy": "timeout_fast_failover_next_source",
                                        "reason": "retrieve_timeout",
                                        "message": "retrieve turn hit time budget, switching source immediately",
                                        "nextVariant": next_variant.clone(),
                                        "nextRoute": next_route_id.clone(),
                                        "nextRouteChannel": next_route_channel.clone(),
                                    })),
                                );
                                current_message = wrap_pm_research_prompt(
                                    session_source,
                                    build_pm_retrieve_prompt(
                                        user_message,
                                        &plan,
                                        next_variant.as_deref(),
                                        next_route_id.as_deref(),
                                        next_attempt,
                                        &runtime_budget,
                                        if next_route_channel
                                            .as_deref()
                                            .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                                        {
                                            runtime_budget.source_slot_browser_secs
                                        } else {
                                            runtime_budget.source_slot_search_secs
                                        },
                                        &merge_blocked_domains(
                                            blocked_domains_from_usage(
                                                &domain_usage_counts,
                                                domain_quota_limit,
                                            ),
                                            &open_domain_circuit_keys,
                                        ),
                                    ),
                                );
                                current_attempt_strategy = None;
                                on_stage(
                                    "retry_repair",
                                    "completed",
                                    next_attempt,
                                    Some(serde_json::json!({
                                        "strategy": "timeout_fast_failover_next_source",
                                        "nextVariant": next_variant,
                                        "nextRoute": next_route_id,
                                        "nextRouteChannel": next_route_channel,
                                    })),
                                );
                                attempt = next_attempt;
                                continue;
                            }
                            let strategy = pm_retry_strategy(next_attempt);
                            let strategy_key = strategy.as_key();
                            on_stage(
                                "retry_repair",
                                "running",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": strategy_key,
                                    "message": "当前来源失败，正在切换修复路径补齐证据",
                                })),
                            );
                            let synthetic_quality = build_runtime_error_quality();
                            current_message = wrap_pm_research_prompt(
                                session_source,
                                build_pm_retry_prompt(
                                    user_message,
                                    &format!("runtime error: {}", e),
                                    &synthetic_quality,
                                    strategy,
                                    next_attempt,
                                    None,
                                    None,
                                    None,
                                    None,
                                    &runtime_budget,
                                    match strategy {
                                        PmRepairStrategy::BrowserFallback => {
                                            runtime_budget.source_slot_browser_secs
                                        }
                                        _ => runtime_budget.source_slot_search_secs,
                                    },
                                    &merge_blocked_domains(
                                        blocked_domains_from_usage(
                                            &domain_usage_counts,
                                            domain_quota_limit,
                                        ),
                                        &open_domain_circuit_keys,
                                    ),
                                ),
                            );
                            current_attempt_strategy = Some(strategy);
                            on_stage(
                                "retry_repair",
                                "completed",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": strategy_key
                                })),
                            );
                            attempt = next_attempt;
                            continue;
                        }
                        if let Some(turn) = last_usable_turn.clone() {
                            let quality = degrade_pm_quality_with_reason(
                                last_usable_quality
                                    .clone()
                                    .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                                "retrieve_runtime_error",
                                "A later retrieval attempt failed; kept the best available answer from earlier attempts.",
                            );
                            on_stage(
                                "synthesize",
                                "completed",
                                attempt,
                                Some(serde_json::json!({
                                    "answerLength": turn.text.chars().count(),
                                    "qualityGatePassed": false,
                                    "reason": "retrieve_runtime_error_partial_answer_kept"
                                })),
                            );
                            return finalize_pm_orchestration_result(
                                state.telemetry_db(),
                                tenant_id,
                                &run_id,
                                session_id,
                                turn,
                                quality,
                            )
                            .await;
                        }
                        on_stage(
                            "retry_repair",
                            "running",
                            attempt,
                            Some(serde_json::json!({
                                "strategy": "force_synthesize_after_runtime_error",
                                "reason": "runtime_error_no_retrieval_output",
                            })),
                        );
                        if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                            manager.clone(),
                            session_id,
                            session_source,
                            user_message,
                            &probe_outcomes,
                            attempt,
                            &observed_tool_calls,
                            answer_delta.clone(),
                        )
                        .await
                        {
                            let turn =
                                merge_pm_turn_with_observed_tool_calls(turn, &observed_tool_calls);
                            let quality = evaluate_pm_answer_quality(&turn);
                            let recovered_quality_passed = quality.passed;
                            on_stage(
                                "retry_repair",
                                "completed",
                                attempt,
                                Some(serde_json::json!({
                                    "strategy": "force_synthesize_after_runtime_error",
                                    "result": if recovered_quality_passed {
                                        "recovered_answer_delivered"
                                    } else {
                                        "degraded_answer_delivered"
                                    },
                                    "priorRuntimeWarning": "retrieval_runtime_error",
                                })),
                            );
                            on_stage(
                                "synthesize",
                                "completed",
                                attempt,
                                Some(serde_json::json!({
                                    "answerLength": turn.text.chars().count(),
                                    "qualityGatePassed": recovered_quality_passed,
                                    "reason": if recovered_quality_passed {
                                        "recovered_after_runtime_error"
                                    } else {
                                        "forced_synthesize_after_runtime_error"
                                    }
                                })),
                            );
                            return finalize_pm_orchestration_result(
                                state.telemetry_db(),
                                tenant_id,
                                &run_id,
                                session_id,
                                turn,
                                quality,
                            )
                            .await;
                        }
                        let observed_tool_summary =
                            build_pm_tool_summary_value(&observed_tool_calls);
                        let emergency_turn = TurnResult {
                            session_id: session_id.to_string(),
                            text: build_pm_emergency_conclusion_text(
                                user_message,
                                &err_text,
                                attempt,
                                Some(&observed_tool_summary),
                                last_usable_quality.as_ref(),
                                Some(&probe_outcomes),
                            ),
                            tool_calls: observed_tool_calls.clone(),
                            usage: TokenUsageRecord {
                                input_tokens: 0,
                                output_tokens: 0,
                                cache_creation_tokens: 0,
                                cache_read_tokens: 0,
                                total_tokens: 0,
                                estimated_cost_usd: 0.0,
                                model: model.to_string(),
                            },
                            compacted: None,
                            iterations: 1,
                            metadata: None,
                            hot_reloaded: false,
                            thinking: None,
                        };
                        let quality = degrade_pm_quality_with_reason(
                            evaluate_pm_answer_quality(&emergency_turn),
                            "runtime_error_emergency_conclusion",
                            "Forced synthesis failed, returned deterministic emergency conclusion.",
                        );
                        on_stage(
                            "retry_repair",
                            "failed",
                            attempt,
                            Some(serde_json::json!({
                                "strategy": "force_synthesize_after_runtime_error",
                                "reason": "forced_synthesis_failed",
                            })),
                        );
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": emergency_turn.text.chars().count(),
                                "qualityGatePassed": false,
                                "reason": "runtime_error_emergency_conclusion"
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            emergency_turn,
                            quality,
                        )
                        .await;
                    }
                }
            }
        } else {
            let mut live_stage_callback =
                |stage: &str,
                 status: &str,
                 stage_attempt: usize,
                 detail: Option<serde_json::Value>| {
                    on_stage(stage, status, stage_attempt, detail)
                };
            match run_pm_retrieve_turn_with_live_events(
                manager.clone(),
                session_id,
                current_message.clone(),
                source_slot_timeout_secs,
                "retrieve source slot",
                attempt,
                selected_probe_variant.as_deref(),
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                runtime_budget.retrieve_max_tool_calls,
                &mut live_stage_callback,
            )
            .await
            {
                Ok((t, observed_tool_calls)) => {
                    let observed_tool_calls = merge_pm_tool_calls_unique(
                        &accumulated_observed_tool_calls,
                        &observed_tool_calls,
                    );
                    accumulated_observed_tool_calls = observed_tool_calls.clone();
                    merge_pm_turn_with_observed_tool_calls(t, &observed_tool_calls)
                }
                Err((e, observed_tool_calls)) => {
                    let observed_tool_calls = merge_pm_tool_calls_unique(
                        &accumulated_observed_tool_calls,
                        &observed_tool_calls,
                    );
                    accumulated_observed_tool_calls = observed_tool_calls.clone();
                    let err_text = e.to_string();
                    let blocked_route = record_pm_route_failure_and_maybe_block(
                        &mut route_fail_streaks,
                        &mut route_blocklist,
                        selected_probe_route_id.as_deref(),
                        selected_probe_route_channel.as_deref(),
                        route_fail_block_threshold,
                    );
                    if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                        pm_retrieve_circuit_report(
                            db,
                            tenant_id,
                            selected_probe_route_channel.as_deref(),
                            circuit_key,
                            false,
                            Some(classify_pm_runtime_error_code(&err_text)),
                            Some(&err_text),
                        )
                        .await;
                    }
                    let is_timeout = err_text.contains("timed out after");
                    let partial_evidence_admission = admit_pm_external_evidence(
                        user_message,
                        &probe_outcomes,
                        &observed_tool_calls,
                    );
                    let has_usable_partial_evidence =
                        is_timeout && partial_evidence_admission.external_evidence_usable;
                    let slot_status = if has_usable_partial_evidence {
                        "partial_success"
                    } else if is_timeout {
                        "timed_out"
                    } else {
                        "failed"
                    };
                    let slot_error_code = if is_timeout {
                        Some("timeout")
                    } else {
                        Some("runtime_error")
                    };
                    let slot_detail = serde_json::json!({
                        "error": err_text.clone(),
                        "selectedVariant": selected_probe_variant.clone(),
                        "selectedRoute": selected_probe_route_id.clone(),
                        "selectedRouteChannel": selected_probe_route_channel.clone(),
                        "route": retrieve_route,
                        "routeTemporarilyBlocked": blocked_route.is_some(),
                        "blockedRouteKey": blocked_route,
                    });
                    persist_pm_source_slot_and_tool_ledger(
                        state.pm_telemetry(),
                        &run_id,
                        attempt,
                        selected_probe_route_id.as_deref(),
                        selected_probe_route_channel.as_deref(),
                        selected_probe_variant.as_deref(),
                        slot_status,
                        Some(
                            retrieve_started
                                .elapsed()
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX),
                        ),
                        slot_error_code,
                        Some(
                            slot_detail
                                .get("error")
                                .and_then(|x| x.as_str())
                                .unwrap_or("runtime_error"),
                        ),
                        Some(&slot_detail),
                        &observed_tool_calls,
                    )
                    .await;
                    upsert_pm_provider_health(
                        state.telemetry_db(),
                        tenant_id,
                        model,
                        "model",
                        has_usable_partial_evidence,
                        Some(
                            retrieve_started
                                .elapsed()
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX),
                        ),
                        (!has_usable_partial_evidence)
                            .then(|| classify_pm_runtime_error_code(&err_text)),
                    )
                    .await;
                    on_stage(
                        "retrieve",
                        if has_usable_partial_evidence {
                            "completed"
                        } else {
                            "failed"
                        },
                        attempt,
                        Some(serde_json::json!({
                            "error": err_text,
                            "partialSuccess": has_usable_partial_evidence,
                            "evidenceAdmission": partial_evidence_admission.to_json(),
                            "selectedVariant": selected_probe_variant,
                            "selectedRoute": selected_probe_route_id,
                            "selectedRouteChannel": selected_probe_route_channel,
                            "sourceSlotTimeoutSecs": source_slot_timeout_secs,
                            "attemptElapsedMs": retrieve_started.elapsed().as_millis(),
                        })),
                    );
                    if has_usable_partial_evidence {
                        let turn = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                            manager.clone(),
                            session_id,
                            session_source,
                            user_message,
                            &probe_outcomes,
                            attempt,
                            &observed_tool_calls,
                            answer_delta.clone(),
                        )
                        .await?;
                        let turn =
                            merge_pm_turn_with_observed_tool_calls(turn, &observed_tool_calls);
                        let quality = evaluate_pm_answer_quality(&turn);
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": turn.text.chars().count(),
                                "qualityGatePassed": quality.passed,
                                "reason": "retrieve_timeout_partial_evidence_synthesized"
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    if attempt < max_attempts
                        && !(probe_kernel_active
                            && pm_probe_outcomes_confirm_retrieval_discovery_exhausted(
                                &probe_outcomes,
                            ))
                    {
                        let next_attempt = attempt + 1;
                        if is_timeout {
                            let (
                                next_variant,
                                next_route_id,
                                next_route_channel,
                                _next_execution_channel,
                                next_source_quota_exhausted,
                            ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                                &plan_query_variants,
                                &plan_enabled_routes,
                                next_attempt,
                                &route_usage_counts,
                                &route_blocklist,
                                source_quota_limit,
                                &used_retrieval_keys,
                            );
                            if next_source_quota_exhausted {
                                let source_exhaustion_reason = pm_source_exhaustion_reason_code(
                                    classify_pm_source_exhaustion_reason(
                                        &plan_enabled_routes,
                                        &route_usage_counts,
                                        &route_blocklist,
                                        source_quota_limit,
                                    ),
                                );
                                on_stage(
                                    "retry_repair",
                                    "failed",
                                    next_attempt,
                                    Some(serde_json::json!({
                                        "strategy": "timeout_fast_failover_next_source",
                                        "reason": source_exhaustion_reason,
                                        "maxCallsPerSource": source_quota_limit,
                                        "nextVariant": next_variant.clone(),
                                        "nextRoute": next_route_id.clone(),
                                        "nextRouteChannel": next_route_channel.clone(),
                                        "routeUsageCounts": route_usage_counts.clone(),
                                        "routeBlocklist": route_blocklist.clone(),
                                    })),
                                );
                                if let Some(turn) = last_usable_turn.clone() {
                                    let quality = degrade_pm_quality_with_reason(
                                        last_usable_quality
                                            .clone()
                                            .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                                        source_exhaustion_reason,
                                        "All enabled sources reached per-source quota after timeout failover.",
                                    );
                                    on_stage(
                                        "synthesize",
                                        "completed",
                                        attempt,
                                        Some(serde_json::json!({
                                            "answerLength": turn.text.chars().count(),
                                            "qualityGatePassed": false,
                                            "reason": "source_quota_exhausted_partial_answer_kept"
                                        })),
                                    );
                                    return finalize_pm_orchestration_result(
                                        state.telemetry_db(),
                                        tenant_id,
                                        &run_id,
                                        session_id,
                                        turn,
                                        quality,
                                    )
                                    .await;
                                }
                                if let Ok(turn) =
                                    run_pm_force_synthesize_fallback_turn_with_observed_tools(
                                        manager.clone(),
                                        session_id,
                                        session_source,
                                        user_message,
                                        &probe_outcomes,
                                        next_attempt,
                                        &observed_tool_calls,
                                        answer_delta.clone(),
                                    )
                                    .await
                                {
                                    let quality = degrade_pm_quality_with_reason(
                                        evaluate_pm_answer_quality(&turn),
                                        source_exhaustion_reason,
                                        "All enabled sources reached per-source quota after timeout failover; forced synthesis.",
                                    );
                                    on_stage(
                                        "synthesize",
                                        "completed",
                                        next_attempt,
                                        Some(serde_json::json!({
                                            "answerLength": turn.text.chars().count(),
                                            "qualityGatePassed": false,
                                            "reason": "source_quota_exhausted_force_synthesize"
                                        })),
                                    );
                                    return finalize_pm_orchestration_result(
                                        state.telemetry_db(),
                                        tenant_id,
                                        &run_id,
                                        session_id,
                                        turn,
                                        quality,
                                    )
                                    .await;
                                }
                            }
                            on_stage(
                                "retry_repair",
                                "running",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": "timeout_fast_failover_next_source",
                                    "reason": "retrieve_timeout",
                                    "message": "retrieve turn hit time budget, switching source immediately",
                                    "nextVariant": next_variant.clone(),
                                    "nextRoute": next_route_id.clone(),
                                    "nextRouteChannel": next_route_channel.clone(),
                                })),
                            );
                            current_message = wrap_pm_research_prompt(
                                session_source,
                                build_pm_retrieve_prompt(
                                    user_message,
                                    &plan,
                                    next_variant.as_deref(),
                                    next_route_id.as_deref(),
                                    next_attempt,
                                    &runtime_budget,
                                    if next_route_channel
                                        .as_deref()
                                        .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                                    {
                                        runtime_budget.source_slot_browser_secs
                                    } else {
                                        runtime_budget.source_slot_search_secs
                                    },
                                    &merge_blocked_domains(
                                        blocked_domains_from_usage(
                                            &domain_usage_counts,
                                            domain_quota_limit,
                                        ),
                                        &open_domain_circuit_keys,
                                    ),
                                ),
                            );
                            current_attempt_strategy = None;
                            on_stage(
                                "retry_repair",
                                "completed",
                                next_attempt,
                                Some(serde_json::json!({
                                    "strategy": "timeout_fast_failover_next_source",
                                    "nextVariant": next_variant,
                                    "nextRoute": next_route_id,
                                    "nextRouteChannel": next_route_channel,
                                })),
                            );
                            attempt = next_attempt;
                            continue;
                        }
                        let strategy = pm_retry_strategy(next_attempt);
                        let strategy_key = strategy.as_key();
                        on_stage(
                            "retry_repair",
                            "running",
                            next_attempt,
                            Some(serde_json::json!({
                                "strategy": strategy_key,
                                "message": "当前来源失败，正在切换修复路径补齐证据",
                            })),
                        );
                        let synthetic_quality = build_runtime_error_quality();
                        current_message = wrap_pm_research_prompt(
                            session_source,
                            build_pm_retry_prompt(
                                user_message,
                                &format!("runtime error: {}", e),
                                &synthetic_quality,
                                strategy,
                                next_attempt,
                                None,
                                None,
                                None,
                                None,
                                &runtime_budget,
                                match strategy {
                                    PmRepairStrategy::BrowserFallback => {
                                        runtime_budget.source_slot_browser_secs
                                    }
                                    _ => runtime_budget.source_slot_search_secs,
                                },
                                &merge_blocked_domains(
                                    blocked_domains_from_usage(
                                        &domain_usage_counts,
                                        domain_quota_limit,
                                    ),
                                    &open_domain_circuit_keys,
                                ),
                            ),
                        );
                        current_attempt_strategy = Some(strategy);
                        on_stage(
                            "retry_repair",
                            "completed",
                            next_attempt,
                            Some(serde_json::json!({
                                "strategy": strategy_key
                            })),
                        );
                        attempt = next_attempt;
                        continue;
                    }
                    if let Some(turn) = last_usable_turn.clone() {
                        let quality = degrade_pm_quality_with_reason(
                            last_usable_quality
                                .clone()
                                .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                            "retrieve_runtime_error",
                            "A later retrieval attempt failed; kept the best available answer from earlier attempts.",
                        );
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": turn.text.chars().count(),
                                "qualityGatePassed": false,
                                "reason": "retrieve_runtime_error_partial_answer_kept"
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    on_stage(
                        "retry_repair",
                        "running",
                        attempt,
                        Some(serde_json::json!({
                            "strategy": "force_synthesize_after_runtime_error",
                            "reason": "runtime_error_no_retrieval_output",
                        })),
                    );
                    if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                        manager.clone(),
                        session_id,
                        session_source,
                        user_message,
                        &probe_outcomes,
                        attempt,
                        &observed_tool_calls,
                        answer_delta.clone(),
                    )
                    .await
                    {
                        let turn =
                            merge_pm_turn_with_observed_tool_calls(turn, &observed_tool_calls);
                        let quality = evaluate_pm_answer_quality(&turn);
                        let recovered_quality_passed = quality.passed;
                        on_stage(
                            "retry_repair",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "strategy": "force_synthesize_after_runtime_error",
                                "result": if recovered_quality_passed {
                                    "recovered_answer_delivered"
                                } else {
                                    "degraded_answer_delivered"
                                },
                                "priorRuntimeWarning": "retrieval_runtime_error",
                            })),
                        );
                        on_stage(
                            "synthesize",
                            "completed",
                            attempt,
                            Some(serde_json::json!({
                                "answerLength": turn.text.chars().count(),
                                "qualityGatePassed": recovered_quality_passed,
                                "reason": if recovered_quality_passed {
                                    "recovered_after_runtime_error"
                                } else {
                                    "forced_synthesize_after_runtime_error"
                                }
                            })),
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    let observed_tool_summary = build_pm_tool_summary_value(&observed_tool_calls);
                    let emergency_turn = TurnResult {
                        session_id: session_id.to_string(),
                        text: build_pm_emergency_conclusion_text(
                            user_message,
                            &format!("runtime error: {}", e),
                            attempt,
                            Some(&observed_tool_summary),
                            last_usable_quality.as_ref(),
                            Some(&probe_outcomes),
                        ),
                        tool_calls: observed_tool_calls.clone(),
                        usage: TokenUsageRecord {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            total_tokens: 0,
                            estimated_cost_usd: 0.0,
                            model: model.to_string(),
                        },
                        compacted: None,
                        iterations: 1,
                        metadata: None,
                        hot_reloaded: false,
                        thinking: None,
                    };
                    let quality = degrade_pm_quality_with_reason(
                        evaluate_pm_answer_quality(&emergency_turn),
                        "runtime_error_emergency_conclusion",
                        "Runtime recovery failed after retries; returned deterministic emergency conclusion.",
                    );
                    on_stage(
                        "retry_repair",
                        "failed",
                        attempt,
                        Some(serde_json::json!({
                            "strategy": "force_synthesize_after_runtime_error",
                            "reason": "forced_synthesis_failed",
                        })),
                    );
                    on_stage(
                        "synthesize",
                        "completed",
                        attempt,
                        Some(serde_json::json!({
                            "answerLength": emergency_turn.text.chars().count(),
                            "qualityGatePassed": false,
                            "reason": "runtime_error_emergency_conclusion"
                        })),
                    );
                    return finalize_pm_orchestration_result(
                        state.telemetry_db(),
                        tenant_id,
                        &run_id,
                        session_id,
                        emergency_turn,
                        quality,
                    )
                    .await;
                }
            }
        };

        let tool_error_count = turn.tool_calls.iter().filter(|tc| tc.is_error).count();
        let tool_summary_value = build_pm_tool_summary_value(&turn.tool_calls);
        let tool_summary = tool_summary_value.to_string();
        let domain_outcomes = collect_pm_domain_tool_outcomes(&turn.tool_calls);
        for (domain, outcome) in &domain_outcomes {
            let should_mark_success =
                outcome.success_count > 0 && outcome.success_count >= outcome.error_count;
            pm_domain_circuit_report(
                db,
                tenant_id,
                domain,
                should_mark_success,
                if should_mark_success {
                    None
                } else {
                    outcome
                        .last_error_code
                        .as_deref()
                        .or(Some("domain_tool_error"))
                },
                if should_mark_success {
                    None
                } else {
                    outcome.last_error_message.as_deref()
                },
            )
            .await;
        }
        if !domain_outcomes.is_empty() {
            open_domain_circuit_keys = load_open_pm_domain_circuit_keys(db, tenant_id, 96).await;
        }
        tracing::info!(
            session_id = %session_id,
            attempt = attempt,
            route = retrieve_route,
            tool_call_count = turn.tool_calls.len(),
            tool_error_count = tool_error_count,
            tool_summary = %tool_summary,
            search_pipeline = %search_doctor_detail,
            "pm retrieve turn completed"
        );
        let slot_detail = serde_json::json!({
            "route": retrieve_route,
            "selectedVariant": selected_probe_variant.clone(),
            "selectedRoute": selected_probe_route_id.clone(),
            "selectedRouteChannel": selected_probe_route_channel.clone(),
            "toolSummary": tool_summary_value.clone(),
            "searchPipeline": search_doctor_detail,
            "toolCallCount": turn.tool_calls.len(),
        });
        persist_pm_source_slot_and_tool_ledger(
            state.pm_telemetry(),
            &run_id,
            attempt,
            selected_probe_route_id.as_deref(),
            selected_probe_route_channel.as_deref(),
            selected_probe_variant.as_deref(),
            "completed",
            Some(
                retrieve_started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            ),
            None,
            None,
            Some(&slot_detail),
            &turn.tool_calls,
        )
        .await;
        upsert_pm_provider_health(
            state.telemetry_db(),
            tenant_id,
            model,
            "model",
            true,
            Some(
                retrieve_started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            ),
            None,
        )
        .await;

        if turn.tool_calls.len() > runtime_budget.retrieve_max_tool_calls {
            if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                pm_retrieve_circuit_report(
                    db,
                    tenant_id,
                    selected_probe_route_channel.as_deref(),
                    circuit_key,
                    false,
                    Some("tool_budget_exceeded"),
                    Some("tool budget exceeded"),
                )
                .await;
            }
            let slot_detail = serde_json::json!({
                "error": "tool_budget_exceeded_force_converge",
                "toolCallCount": turn.tool_calls.len(),
                "toolBudget": runtime_budget.retrieve_max_tool_calls,
                "route": retrieve_route,
                "toolSummary": tool_summary_value.clone(),
            });
            persist_pm_source_slot_and_tool_ledger(
                state.pm_telemetry(),
                &run_id,
                attempt,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                selected_probe_variant.as_deref(),
                "failed",
                Some(
                    retrieve_started
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                ),
                Some("tool_budget_exceeded"),
                Some("tool budget exceeded"),
                Some(&slot_detail),
                &turn.tool_calls,
            )
            .await;
            on_stage(
                "retrieve",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "error": "tool_budget_exceeded_force_converge",
                    "toolCallCount": turn.tool_calls.len(),
                    "toolBudget": runtime_budget.retrieve_max_tool_calls,
                    "route": retrieve_route,
                    "toolSummary": tool_summary_value.clone(),
                })),
            );
            on_stage(
                "retry_repair",
                "running",
                attempt,
                Some(serde_json::json!({
                    "strategy": "degraded_summary",
                    "reason": "tool_budget_exceeded",
                    "message": "tool call budget exceeded; forcing final synthesis from collected evidence",
                })),
            );
            let budget_admission =
                admit_pm_external_evidence(user_message, &probe_outcomes, &turn.tool_calls);
            if let Ok(synth_turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                if budget_admission.accepted_tool_calls.is_empty() {
                    &accumulated_observed_tool_calls
                } else {
                    budget_admission.accepted_tool_calls.as_slice()
                },
                answer_delta.clone(),
            )
            .await
            {
                let quality = degrade_pm_quality_with_reason(
                    evaluate_pm_answer_quality(&synth_turn),
                    "tool_budget_exceeded_force_converge",
                    "Tool budget exceeded on retrieval; forced synthesis with available evidence.",
                );
                on_stage(
                    "retry_repair",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "degraded_summary",
                        "result": "forced_synthesis_delivered",
                    })),
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": synth_turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "tool_budget_exceeded_force_converge"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    synth_turn,
                    quality,
                )
                .await;
            }
            let emergency_text = build_pm_emergency_conclusion_text(
                user_message,
                &format!(
                    "tool budget exceeded ({} > {})",
                    turn.tool_calls.len(),
                    runtime_budget.retrieve_max_tool_calls
                ),
                attempt,
                Some(&tool_summary_value),
                last_usable_quality.as_ref(),
                Some(&probe_outcomes),
            );
            let emergency_turn = TurnResult {
                session_id: turn.session_id.clone(),
                text: emergency_text,
                tool_calls: turn.tool_calls.clone(),
                usage: turn.usage.clone(),
                compacted: turn.compacted.clone(),
                iterations: turn.iterations,
                metadata: turn.metadata.clone(),
                hot_reloaded: turn.hot_reloaded,
                thinking: turn.thinking.clone(),
            };
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&emergency_turn),
                "tool_budget_exceeded_emergency_conclusion",
                "Forced synthesis failed, returned deterministic emergency conclusion.",
            );
            on_stage(
                "retry_repair",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "strategy": "degraded_summary",
                    "reason": "forced_synthesis_failed",
                    "evidenceAdmission": budget_admission.to_json(),
                })),
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": emergency_turn.text.chars().count(),
                    "qualityGatePassed": false,
                    "reason": "tool_budget_exceeded_emergency_conclusion"
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                emergency_turn,
                quality,
            )
            .await;
        }

        let disallowed_tools = collect_pm_disallowed_research_tools(&turn.tool_calls);
        if !disallowed_tools.is_empty() {
            let blocked_route = record_pm_route_failure_and_maybe_block(
                &mut route_fail_streaks,
                &mut route_blocklist,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                route_fail_block_threshold,
            );
            if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                pm_retrieve_circuit_report(
                    db,
                    tenant_id,
                    selected_probe_route_channel.as_deref(),
                    circuit_key,
                    false,
                    Some("tool_policy_violation"),
                    Some("disallowed tools used in PM retrieval"),
                )
                .await;
            }
            let disallowed_joined = disallowed_tools.join(", ");
            let slot_detail = serde_json::json!({
                "error": "tool_policy_violation",
                "route": retrieve_route,
                "disallowedTools": disallowed_tools.clone(),
                "toolCallCount": turn.tool_calls.len(),
                "toolSummary": tool_summary_value.clone(),
                "routeTemporarilyBlocked": blocked_route.is_some(),
                "blockedRouteKey": blocked_route,
            });
            persist_pm_source_slot_and_tool_ledger(
                state.pm_telemetry(),
                &run_id,
                attempt,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                selected_probe_variant.as_deref(),
                "failed",
                Some(
                    retrieve_started
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                ),
                Some("tool_policy_violation"),
                Some("disallowed tools used in PM retrieval"),
                Some(&slot_detail),
                &turn.tool_calls,
            )
            .await;
            on_stage(
                "retrieve",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "error": "tool_policy_violation",
                    "disallowedTools": disallowed_tools.clone(),
                    "route": retrieve_route,
                    "toolSummary": tool_summary_value.clone(),
                })),
            );
            tracing::warn!(
                session_id = %session_id,
                attempt = attempt,
                route = retrieve_route,
                disallowed_tools = %disallowed_joined,
                "pm retrieve turn violated tool policy; switching source immediately"
            );
            if attempt < max_attempts {
                let next_attempt = attempt + 1;
                let (
                    next_variant,
                    next_route_id,
                    next_route_channel,
                    _next_execution_channel,
                    next_source_quota_exhausted,
                ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                    &plan_query_variants,
                    &plan_enabled_routes,
                    next_attempt,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                    &used_retrieval_keys,
                );
                if !next_source_quota_exhausted {
                    on_stage(
                        "retry_repair",
                        "running",
                        next_attempt,
                        Some(serde_json::json!({
                            "strategy": "tool_policy_failover_next_source",
                            "reason": "tool_policy_violation",
                            "message": "disallowed tools detected, switching source immediately",
                            "disallowedTools": disallowed_tools,
                            "nextVariant": next_variant.clone(),
                            "nextRoute": next_route_id.clone(),
                            "nextRouteChannel": next_route_channel.clone(),
                        })),
                    );
                    current_message = wrap_pm_research_prompt(
                        session_source,
                        build_pm_retrieve_prompt(
                            user_message,
                            &plan,
                            next_variant.as_deref(),
                            next_route_id.as_deref(),
                            next_attempt,
                            &runtime_budget,
                            if next_route_channel
                                .as_deref()
                                .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                            {
                                runtime_budget.source_slot_browser_secs
                            } else {
                                runtime_budget.source_slot_search_secs
                            },
                            &merge_blocked_domains(
                                blocked_domains_from_usage(
                                    &domain_usage_counts,
                                    domain_quota_limit,
                                ),
                                &open_domain_circuit_keys,
                            ),
                        ),
                    );
                    current_attempt_strategy = None;
                    on_stage(
                        "retry_repair",
                        "completed",
                        next_attempt,
                        Some(serde_json::json!({
                            "strategy": "tool_policy_failover_next_source",
                            "nextVariant": next_variant,
                            "nextRoute": next_route_id,
                            "nextRouteChannel": next_route_channel,
                        })),
                    );
                    attempt = next_attempt;
                    continue;
                }
            }

            if let Some(turn) = last_usable_turn.clone() {
                let quality = degrade_pm_quality_with_reason(
                    last_usable_quality
                        .clone()
                        .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                    "tool_policy_violation",
                    "Disallowed tools were used during retrieval; returned best prior answer from valid evidence.",
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "tool_policy_violation_partial_answer_kept"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }

            on_stage(
                "retry_repair",
                "running",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_tool_policy_violation",
                    "reason": "tool_policy_violation_no_valid_answer",
                })),
            );
            if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                &accumulated_observed_tool_calls,
                answer_delta.clone(),
            )
            .await
            {
                let quality = degrade_pm_quality_with_reason(
                    evaluate_pm_answer_quality(&turn),
                    "tool_policy_violation_force_synthesize",
                    "Disallowed tools were detected; forced synthesis using available compliant evidence.",
                );
                on_stage(
                    "retry_repair",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "force_synthesize_after_tool_policy_violation",
                        "result": "degraded_answer_delivered",
                    })),
                );
                on_stage(
                    "synthesize",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "answerLength": turn.text.chars().count(),
                        "qualityGatePassed": false,
                        "reason": "tool_policy_violation_force_synthesize"
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            let emergency_turn = TurnResult {
                session_id: session_id.to_string(),
                text: build_pm_emergency_conclusion_text(
                    user_message,
                    &format!("tool policy violation: {}", disallowed_joined),
                    attempt,
                    Some(&tool_summary_value),
                    last_usable_quality.as_ref(),
                    Some(&probe_outcomes),
                ),
                tool_calls: turn.tool_calls.clone(),
                usage: turn.usage.clone(),
                compacted: turn.compacted.clone(),
                iterations: turn.iterations,
                metadata: turn.metadata.clone(),
                hot_reloaded: turn.hot_reloaded,
                thinking: turn.thinking.clone(),
            };
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&emergency_turn),
                "tool_policy_violation_emergency_conclusion",
                "Disallowed tools persisted and forced synthesis failed; returned emergency conclusion.",
            );
            on_stage(
                "retry_repair",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_tool_policy_violation",
                    "reason": "forced_synthesis_failed",
                })),
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": emergency_turn.text.chars().count(),
                    "qualityGatePassed": false,
                    "reason": "tool_policy_violation_emergency_conclusion"
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                emergency_turn,
                quality,
            )
            .await;
        }

        let turn_domains = collect_pm_turn_domains(&turn.tool_calls);
        let mut domain_quota_exceeded: Vec<String> = Vec::new();
        for domain in turn_domains {
            let entry = domain_usage_counts.entry(domain.clone()).or_insert(0);
            let next = entry.saturating_add(1);
            *entry = next;
            if next > domain_quota_limit {
                domain_quota_exceeded.push(domain);
            }
        }
        domain_quota_exceeded.sort();
        domain_quota_exceeded.dedup();

        if !domain_quota_exceeded.is_empty() && attempt < max_attempts {
            let next_attempt = attempt + 1;
            let (
                next_variant,
                next_route_id,
                next_route_channel,
                _next_execution_channel,
                next_source_quota_exhausted,
            ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                &plan_query_variants,
                &plan_enabled_routes,
                next_attempt,
                &route_usage_counts,
                &route_blocklist,
                source_quota_limit,
                &used_retrieval_keys,
            );
            on_stage(
                "retry_repair",
                "running",
                next_attempt,
                Some(serde_json::json!({
                    "strategy": "domain_quota_failover",
                    "reason": "domain_quota_exceeded",
                    "exceededDomains": domain_quota_exceeded.clone(),
                    "nextVariant": next_variant.clone(),
                    "nextRoute": next_route_id.clone(),
                    "nextRouteChannel": next_route_channel.clone(),
                })),
            );
            if next_source_quota_exhausted {
                let source_exhaustion_reason =
                    pm_source_exhaustion_reason_code(classify_pm_source_exhaustion_reason(
                        &plan_enabled_routes,
                        &route_usage_counts,
                        &route_blocklist,
                        source_quota_limit,
                    ));
                on_stage(
                    "retry_repair",
                    "failed",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "domain_quota_failover",
                        "reason": source_exhaustion_reason,
                        "maxCallsPerSource": source_quota_limit,
                        "nextVariant": next_variant.clone(),
                        "nextRoute": next_route_id.clone(),
                        "nextRouteChannel": next_route_channel.clone(),
                        "routeUsageCounts": route_usage_counts.clone(),
                        "routeBlocklist": route_blocklist.clone(),
                    })),
                );
            } else {
                current_message = wrap_pm_research_prompt(
                    session_source,
                    build_pm_retrieve_prompt(
                        user_message,
                        &plan,
                        next_variant.as_deref(),
                        next_route_id.as_deref(),
                        next_attempt,
                        &runtime_budget,
                        if next_route_channel
                            .as_deref()
                            .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                        {
                            runtime_budget.source_slot_browser_secs
                        } else {
                            runtime_budget.source_slot_search_secs
                        },
                        &merge_blocked_domains(
                            blocked_domains_from_usage(&domain_usage_counts, domain_quota_limit),
                            &open_domain_circuit_keys,
                        ),
                    ),
                );
                current_attempt_strategy = None;
                on_stage(
                    "retry_repair",
                    "completed",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "domain_quota_failover",
                        "exceededDomains": domain_quota_exceeded,
                        "nextVariant": next_variant,
                        "nextRoute": next_route_id,
                        "nextRouteChannel": next_route_channel,
                    })),
                );
                attempt = next_attempt;
                continue;
            }
        }

        let tool_only_turn = !turn.tool_calls.is_empty() && turn.text.trim().is_empty();
        if tool_only_turn {
            let mut tool_only_quality = evaluate_pm_answer_quality(&turn);
            apply_pm_contract_gate(&mut tool_only_quality, &turn.text, &runtime_budget);
            apply_pm_conflict_gate(&mut tool_only_quality);
            update_best_pm_turn_quality(
                &mut best_turn,
                &mut best_quality,
                &turn,
                &tool_only_quality,
            );
            if best_turn.is_some() && best_quality.is_some() {
                last_usable_turn = best_turn.clone();
                last_usable_quality = best_quality.clone();
            } else {
                last_usable_turn = Some(turn.clone());
                last_usable_quality = Some(tool_only_quality.clone());
            }
            if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                pm_retrieve_circuit_report(
                    db,
                    tenant_id,
                    selected_probe_route_channel.as_deref(),
                    circuit_key,
                    false,
                    Some("tool_only_no_text"),
                    Some("tool-only response without final text"),
                )
                .await;
            }
            let slot_detail = serde_json::json!({
                "error": "tool_only_no_text",
                "toolCallCount": turn.tool_calls.len(),
                "route": retrieve_route,
                "toolSummary": tool_summary_value.clone(),
                "toolOnlyQuality": {
                    "citationCount": tool_only_quality.citation_count,
                    "domainCount": tool_only_quality.domain_count,
                    "triadCoverage": tool_only_quality.triad_coverage,
                },
            });
            persist_pm_source_slot_and_tool_ledger(
                state.pm_telemetry(),
                &run_id,
                attempt,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                selected_probe_variant.as_deref(),
                "failed",
                Some(
                    retrieve_started
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                ),
                Some("tool_only_no_text"),
                Some("tool-only response without final text"),
                Some(&slot_detail),
                &turn.tool_calls,
            )
            .await;
            tracing::warn!(
                session_id = %session_id,
                attempt = attempt,
                route = retrieve_route,
                tool_call_count = turn.tool_calls.len(),
                tool_error_count = tool_error_count,
                tool_summary = %tool_summary,
                "pm orchestrator retrieve produced tool-only turn; forcing no-tool synthesize retry"
            );
            on_stage(
                "retrieve",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "error": "tool_only_no_text",
                    "toolCallCount": turn.tool_calls.len(),
                    "route": retrieve_route,
                    "toolSummary": tool_summary_value.clone(),
                })),
            );
            if attempt < max_attempts {
                let next_attempt = attempt + 1;
                on_stage(
                    "retry_repair",
                    "running",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "force_synthesize_after_tool_only",
                        "reason": "tool_only_no_text",
                    })),
                );
                let tool_only_admission =
                    admit_pm_external_evidence(user_message, &probe_outcomes, &turn.tool_calls);
                let probe_context =
                    build_pm_probe_repair_context(&tool_only_admission.accepted_probe_outcomes);
                let observed_context =
                    build_pm_observed_tool_context(&tool_only_admission.accepted_tool_calls);
                let previous_answer_for_retry =
                    match (probe_context.is_empty(), observed_context.is_empty()) {
                        (true, true) => "Tool-only turn without admitted source-backed evidence. Discard weak snippets and answer from first-party data plus expert reasoning; do not cite rejected sources.".to_string(),
                        (false, true) => probe_context,
                        (true, false) => observed_context,
                        (false, false) => format!("{}\n\n{}", probe_context, observed_context),
                    };
                current_message = wrap_pm_research_prompt(
                    session_source,
                    build_pm_force_synthesize_prompt(
                        user_message,
                        &previous_answer_for_retry,
                        next_attempt,
                    ),
                );
                current_attempt_strategy = Some(PmRepairStrategy::DegradedSummary);
                on_stage(
                    "retry_repair",
                    "completed",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "force_synthesize_after_tool_only"
                    })),
                );
                attempt = next_attempt;
                continue;
            }
        }

        if should_fast_fail_after_tool_errors(&turn) {
            if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
                pm_retrieve_circuit_report(
                    db,
                    tenant_id,
                    selected_probe_route_channel.as_deref(),
                    circuit_key,
                    false,
                    Some("network_request_failed"),
                    Some("all tool calls failed with network errors"),
                )
                .await;
            }
            let slot_detail = serde_json::json!({
                "error": "network_request_failed_fast_exit",
                "toolCallCount": turn.tool_calls.len(),
                "route": retrieve_route,
                "toolSummary": tool_summary_value.clone(),
            });
            persist_pm_source_slot_and_tool_ledger(
                state.pm_telemetry(),
                &run_id,
                attempt,
                selected_probe_route_id.as_deref(),
                selected_probe_route_channel.as_deref(),
                selected_probe_variant.as_deref(),
                "failed",
                Some(
                    retrieve_started
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX),
                ),
                Some("network_request_failed"),
                Some("all tool calls failed with network errors"),
                Some(&slot_detail),
                &turn.tool_calls,
            )
            .await;
            on_stage(
                "retrieve",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "error": "network_request_failed_fast_exit",
                    "toolCallCount": turn.tool_calls.len(),
                    "route": retrieve_route,
                    "toolSummary": tool_summary_value.clone(),
                })),
            );
            if attempt < max_attempts {
                let next_attempt = attempt + 1;
                let (
                    next_variant,
                    next_route_id,
                    next_route_channel,
                    _next_execution_channel,
                    next_source_quota_exhausted,
                ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                    &plan_query_variants,
                    &plan_enabled_routes,
                    next_attempt,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                    &used_retrieval_keys,
                );
                if next_source_quota_exhausted {
                    let source_exhaustion_reason =
                        pm_source_exhaustion_reason_code(classify_pm_source_exhaustion_reason(
                            &plan_enabled_routes,
                            &route_usage_counts,
                            &route_blocklist,
                            source_quota_limit,
                        ));
                    on_stage(
                        "retry_repair",
                        "failed",
                        next_attempt,
                        Some(serde_json::json!({
                            "strategy": "fast_failover_next_source",
                            "reason": source_exhaustion_reason,
                            "maxCallsPerSource": source_quota_limit,
                            "nextVariant": next_variant.clone(),
                            "nextRoute": next_route_id.clone(),
                            "nextRouteChannel": next_route_channel.clone(),
                            "routeUsageCounts": route_usage_counts.clone(),
                            "routeBlocklist": route_blocklist.clone(),
                        })),
                    );
                    if let Some(turn) = last_usable_turn.clone() {
                        let quality = degrade_pm_quality_with_reason(
                            last_usable_quality
                                .clone()
                                .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                            source_exhaustion_reason,
                            "All enabled sources reached per-source quota during network fast failover.",
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                    if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                        manager.clone(),
                        session_id,
                        session_source,
                        user_message,
                        &probe_outcomes,
                        next_attempt,
                        &accumulated_observed_tool_calls,
                        answer_delta.clone(),
                    )
                    .await
                    {
                        let quality = degrade_pm_quality_with_reason(
                            evaluate_pm_answer_quality(&turn),
                            source_exhaustion_reason,
                            "All enabled sources reached per-source quota during network failover; forced synthesis.",
                        );
                        return finalize_pm_orchestration_result(
                            state.telemetry_db(),
                            tenant_id,
                            &run_id,
                            session_id,
                            turn,
                            quality,
                        )
                        .await;
                    }
                }
                on_stage(
                    "retry_repair",
                    "running",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "fast_failover_next_source",
                        "reason": "all_tool_calls_network_error",
                        "message": "network blocked on current source, switching immediately",
                        "nextVariant": next_variant.clone(),
                        "nextRoute": next_route_id.clone(),
                        "nextRouteChannel": next_route_channel.clone(),
                    })),
                );
                current_message = wrap_pm_research_prompt(
                    session_source,
                    build_pm_retrieve_prompt(
                        user_message,
                        &plan,
                        next_variant.as_deref(),
                        next_route_id.as_deref(),
                        next_attempt,
                        &runtime_budget,
                        if next_route_channel
                            .as_deref()
                            .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                        {
                            runtime_budget.source_slot_browser_secs
                        } else {
                            runtime_budget.source_slot_search_secs
                        },
                        &merge_blocked_domains(
                            blocked_domains_from_usage(&domain_usage_counts, domain_quota_limit),
                            &open_domain_circuit_keys,
                        ),
                    ),
                );
                current_attempt_strategy = None;
                on_stage(
                    "retry_repair",
                    "completed",
                    next_attempt,
                    Some(serde_json::json!({
                        "strategy": "fast_failover_next_source",
                        "nextVariant": next_variant,
                        "nextRoute": next_route_id,
                        "nextRouteChannel": next_route_channel,
                    })),
                );
                attempt = next_attempt;
                continue;
            }
            on_stage(
                "retry_repair",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "strategy": "fast_fail_network",
                    "reason": "all_tool_calls_network_error",
                })),
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": 0,
                    "qualityGatePassed": false,
                    "reason": "network_request_failed_fast_exit"
                })),
            );
            if let Some(turn) = last_usable_turn.clone() {
                let quality = degrade_pm_quality_with_reason(
                    last_usable_quality
                        .clone()
                        .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                    "network_fast_fail",
                    "Some sources were unreachable; kept the best available answer from earlier attempts.",
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            on_stage(
                "retry_repair",
                "running",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_network_fast_fail",
                    "reason": "network_fast_fail_no_retrieval_output",
                })),
            );
            if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &probe_outcomes,
                attempt,
                &accumulated_observed_tool_calls,
                answer_delta.clone(),
            )
            .await
            {
                let quality = degrade_pm_quality_with_reason(
                    evaluate_pm_answer_quality(&turn),
                    "forced_synthesize_after_network_fast_fail",
                    "Key sources were unreachable; generated a best-effort answer and flagged evidence gaps.",
                );
                on_stage(
                    "retry_repair",
                    "completed",
                    attempt,
                    Some(serde_json::json!({
                        "strategy": "force_synthesize_after_network_fast_fail",
                        "result": "degraded_answer_delivered",
                    })),
                );
                return finalize_pm_orchestration_result(
                    state.telemetry_db(),
                    tenant_id,
                    &run_id,
                    session_id,
                    turn,
                    quality,
                )
                .await;
            }
            let emergency_turn = TurnResult {
                session_id: session_id.to_string(),
                text: build_pm_emergency_conclusion_text(
                    user_message,
                    "network request failures across all tool calls",
                    attempt,
                    Some(&tool_summary_value),
                    last_usable_quality.as_ref(),
                    Some(&probe_outcomes),
                ),
                tool_calls: Vec::new(),
                usage: TokenUsageRecord {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    total_tokens: 0,
                    estimated_cost_usd: 0.0,
                    model: model.to_string(),
                },
                compacted: None,
                iterations: 1,
                metadata: None,
                hot_reloaded: false,
                thinking: None,
            };
            let quality = degrade_pm_quality_with_reason(
                evaluate_pm_answer_quality(&emergency_turn),
                "network_fast_fail_emergency_conclusion",
                "All retrieval channels failed and forced synthesis did not return text; emitted emergency conclusion.",
            );
            on_stage(
                "retry_repair",
                "failed",
                attempt,
                Some(serde_json::json!({
                    "strategy": "force_synthesize_after_network_fast_fail",
                    "reason": "forced_synthesis_failed",
                })),
            );
            on_stage(
                "synthesize",
                "completed",
                attempt,
                Some(serde_json::json!({
                    "answerLength": emergency_turn.text.chars().count(),
                    "qualityGatePassed": false,
                    "reason": "network_fast_fail_emergency_conclusion"
                })),
            );
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                emergency_turn,
                quality,
            )
            .await;
        }

        if let Some(circuit_key) = selected_route_circuit_key.as_deref() {
            pm_retrieve_circuit_report(
                db,
                tenant_id,
                selected_probe_route_channel.as_deref(),
                circuit_key,
                true,
                None,
                None,
            )
            .await;
        }
        record_pm_route_success(
            &mut route_fail_streaks,
            &mut route_blocklist,
            selected_probe_route_id.as_deref(),
            selected_probe_route_channel.as_deref(),
        );
        on_stage(
            "retrieve",
            "completed",
            attempt,
            Some(serde_json::json!({
                "durationMs": retrieve_started.elapsed().as_millis(),
                "route": retrieve_route,
                "selectedVariant": selected_probe_variant,
                "selectedRoute": selected_probe_route_id,
                "selectedRouteChannel": selected_probe_route_channel,
                "selectedScore": selected_probe_score,
                "probeCount": probe_outcomes.len(),
                "probeCandidateCount": probe_candidates.len(),
                "bestTurnAdopted": best_turn_adopted,
                "probeOnlyForRouting": probe_kernel_active && !best_turn_adopted,
                "sourceRouteCount": plan_route_count,
                "queryVariantCount": plan_query_variants.len(),
                "targetSubtask": active_subtask_focus.clone(),
                "toolCallCount": turn.tool_calls.len(),
                "toolSummary": tool_summary_value,
            })),
        );

        if strict_subtask_closure_enabled && !probe_outcomes.is_empty() {
            accumulated_probe_outcomes.extend(probe_outcomes.iter().cloned());
            if accumulated_probe_outcomes.len() > probe_history_cap {
                let overflow = accumulated_probe_outcomes.len() - probe_history_cap;
                accumulated_probe_outcomes.drain(0..overflow);
            }
        }
        let verify_started = Instant::now();
        let mut quality = evaluate_pm_answer_quality(&turn);
        let evidence_admission_report =
            admit_pm_external_evidence(user_message, &probe_outcomes, &turn.tool_calls);
        let min_parallel_agents = pm_env_usize("PM_SUBTASK_MIN_PARALLEL_AGENTS", 1).max(1);
        let min_citations_per_subtask = pm_env_usize("PM_SUBTASK_MIN_CITATIONS", 3).max(1);
        let min_domains_per_subtask = pm_env_usize("PM_SUBTASK_MIN_DOMAINS", 2).max(1);
        let accumulated_evidence_admission_report = if strict_subtask_closure_enabled {
            Some(admit_pm_external_evidence(
                user_message,
                &accumulated_probe_outcomes,
                &turn.tool_calls,
            ))
        } else {
            None
        };
        let effective_evidence_admission_report = accumulated_evidence_admission_report
            .as_ref()
            .filter(|report| report.external_evidence_usable)
            .unwrap_or(&evidence_admission_report);
        apply_pm_evidence_admission_gate(
            &mut quality,
            &turn.text,
            effective_evidence_admission_report,
        );
        apply_pm_contract_gate(&mut quality, &turn.text, &runtime_budget);
        apply_pm_conflict_gate(&mut quality);
        let depth_probe_outcomes = if strict_subtask_closure_enabled {
            let accumulated_admission = accumulated_evidence_admission_report
                .as_ref()
                .expect("strict closure admission report exists");
            if accumulated_admission.accepted_probe_outcomes.is_empty() {
                accumulated_probe_outcomes.as_slice()
            } else {
                accumulated_admission.accepted_probe_outcomes.as_slice()
            }
        } else if evidence_admission_report.accepted_probe_outcomes.is_empty() {
            probe_outcomes.as_slice()
        } else {
            evidence_admission_report.accepted_probe_outcomes.as_slice()
        };
        let depth_gate_result: PmDepthCoverageGateResult = apply_pm_depth_coverage_gate(
            &mut quality,
            &plan,
            &turn.text,
            depth_probe_outcomes,
            min_parallel_agents,
            min_citations_per_subtask,
            min_domains_per_subtask,
        );
        let report_strategy_gate_result =
            apply_pm_report_strategy_quality_gate(&mut quality, &plan, user_message, &turn.text);
        let mut llm_expert_review: Option<PmLlmExpertReview> = None;
        let mut llm_expert_review_trace = serde_json::json!({
            "enabled": false,
            "reason": "deep_loop_not_enabled"
        });
        let deterministic_depth_gap = depth_gate_result.gap_repair_plan.enabled;
        if deep_loop_enabled
            && !llm_expert_review_completed
            && (!deterministic_depth_gap || attempt >= max_attempts)
        {
            let reviewed = run_pm_llm_expert_review(
                manager.clone(),
                tenant_id,
                user_id,
                model,
                user_message,
                &plan,
                &turn,
                &quality,
                attempt,
                max_attempts,
            )
            .await;
            quality = reviewed.quality;
            llm_expert_review = reviewed.review;
            llm_expert_review_trace = reviewed.trace;
            llm_expert_review_completed = true;
            retained_llm_expert_review = llm_expert_review.clone();
            retained_llm_expert_review_trace = llm_expert_review_trace.clone();
            record_pm_audit_event(
                state.telemetry_db(),
                tenant_id,
                user_id,
                &run_id,
                "pm.deep_loop.llm_expert_review",
                if llm_expert_review.is_some() {
                    "info"
                } else {
                    "warn"
                },
                "PM deep research LLM expert review completed",
                Some(&llm_expert_review_trace),
            )
            .await;
        } else if deep_loop_enabled && deterministic_depth_gap {
            llm_expert_review_trace = serde_json::json!({
                "enabled": true,
                "status": "deferred",
                "reason": "deterministic_depth_gaps_must_be_repaired_first",
                "attempt": attempt,
            });
        } else if deep_loop_enabled && llm_expert_review_completed {
            llm_expert_review_trace = serde_json::json!({
                "enabled": true,
                "status": "reused",
                "reason": "one_semantic_review_per_research_run",
                "review": retained_llm_expert_review.as_ref().map(PmLlmExpertReview::to_json),
                "originalTrace": retained_llm_expert_review_trace.clone(),
            });
        }
        // Count independently admitted evidence, not tool invocations or answer
        // verbosity. Repeating the same searches must not reset convergence.
        let evidence_signal = quality
            .citation_count
            .saturating_mul(2)
            .saturating_add(quality.domain_count.saturating_mul(3))
            .saturating_add(
                effective_evidence_admission_report
                    .accepted_probe_outcomes
                    .len(),
            );
        if evidence_signal > best_evidence_signal {
            best_evidence_signal = evidence_signal;
            no_new_evidence_repeats = 0;
        } else if deep_loop_enabled {
            no_new_evidence_repeats = no_new_evidence_repeats.saturating_add(1);
        }
        let planned_subtask_count_verify = depth_gate_result.coverage_gate.bundles.len();
        let executed_subtask_count_verify = depth_gate_result
            .coverage_gate
            .bundles
            .iter()
            .filter(|bundle| {
                bundle.probe_count > 0 || bundle.citation_count > 0 || bundle.domain_count > 0
            })
            .count();
        let queued_subtask_estimate_verify =
            planned_subtask_count_verify.saturating_sub(executed_subtask_count_verify);
        let knowledge_coverage_ratio_verify = if planned_subtask_count_verify == 0 {
            1.0
        } else {
            (executed_subtask_count_verify as f64 / planned_subtask_count_verify as f64)
                .clamp(0.0, 1.0)
        };
        let pm_quality_visible_debug = pm_flag_enabled("PM_QUALITY_VISIBLE_DEBUG", true);
        update_best_pm_turn_quality(&mut best_turn, &mut best_quality, &turn, &quality);
        if best_turn.is_some() && best_quality.is_some() {
            last_usable_turn = best_turn.clone();
            last_usable_quality = best_quality.clone();
        } else if !turn.text.trim().is_empty() || !turn.tool_calls.is_empty() {
            last_usable_turn = Some(turn.clone());
            last_usable_quality = Some(quality.clone());
        }
        let external_search_available = plan_route_count > 0
            || search_doctor_detail
                .get("layers")
                .and_then(|value| value.as_array())
                .map(|layers| {
                    layers.iter().any(|layer| {
                        let key = layer
                            .get("key")
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        let available = layer
                            .get("available")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        available
                            && matches!(
                                key,
                                "native_model_search" | "mcp_search" | "configured_search_provider"
                            )
                    })
                })
                .unwrap_or(false);
        let deep_loop_output = if deep_loop_enabled {
            Some(PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
                plan: &plan,
                question: user_message,
                answer: &turn.text,
                quality_passed: quality.passed,
                deliverable: quality.deliverable,
                citation_count: quality.citation_count,
                domain_count: quality.domain_count,
                claim_count: quality.claim_count,
                triad_coverage: quality.triad_coverage,
                conflict_confidence: quality.conflict_confidence,
                missing: &quality.missing,
                suggestions: &quality.suggestions,
                attempt,
                max_attempts,
                elapsed_secs: orchestration_started.elapsed().as_secs(),
                max_wall_secs: deep_loop_max_wall_secs,
                no_new_evidence_repeats,
                no_new_evidence_limit: deep_loop_no_new_evidence_limit,
                external_search_available,
                admitted_external_evidence: effective_evidence_admission_report
                    .external_evidence_usable,
                rejected_external_evidence_count: effective_evidence_admission_report
                    .rejected_evidence_count,
            }))
        } else {
            None
        };
        let mut verify_detail = serde_json::json!({
            "durationMs": verify_started.elapsed().as_millis(),
            "passed": quality.passed,
            "toolCallCount": quality.tool_call_count,
            "citationCount": quality.citation_count,
            "domainCount": quality.domain_count,
            "deliverable": quality.deliverable,
            "qualityLevel": quality.quality_level,
            "conflictMatrixCount": quality.conflict_matrix.len(),
            "conflictGraphEdgeCount": quality.conflict_graph.edge_count,
            "conflictAvgConfidence": quality.conflict_graph.avg_confidence,
            "depthGateEnforced": depth_gate_result.enforced,
            "reportStrategyGateEnforced": report_strategy_gate_result.enforced,
            "reportStrategyGatePassed": report_strategy_gate_result.passed,
            "reportStrategyMetricMatches": report_strategy_gate_result.matched_metric_count,
            "reportStrategyHasSegmentStrategy": report_strategy_gate_result.has_segment_strategy,
            "reportStrategyHasExperimentPlan": report_strategy_gate_result.has_experiment_plan,
            "reportStrategyHasGuardrails": report_strategy_gate_result.has_guardrails,
            "reportStrategyHasOpportunityCohorts": report_strategy_gate_result.has_opportunity_cohorts,
            "reportStrategyRespectsAntiPatterns": report_strategy_gate_result.respects_anti_patterns,
            "subtaskGapCount": depth_gate_result.subtask_gap_titles.len(),
            "dimensionGapCount": depth_gate_result.dimension_gap_titles.len(),
            "minParallelAgents": depth_gate_result.coverage_gate.min_parallel_agents,
            "subtaskCoveragePassed": depth_gate_result.coverage_gate.passed,
            "plannedSubtaskCount": planned_subtask_count_verify,
            "executedSubtaskCount": executed_subtask_count_verify,
            "queuedSubtaskEstimate": queued_subtask_estimate_verify,
            "knowledgeCoverageRatio": knowledge_coverage_ratio_verify,
            "bestTurnAdopted": best_turn_adopted,
            "probeOnlyForRouting": probe_kernel_active && !best_turn_adopted,
            "llmExpertReview": llm_expert_review_trace.clone(),
            "evidenceAdmission": effective_evidence_admission_report.to_json(),
            "currentAttemptEvidenceAdmission": evidence_admission_report.to_json(),
        });
        if let (Some(obj), Some(loop_output)) =
            (verify_detail.as_object_mut(), deep_loop_output.as_ref())
        {
            obj.insert("deepLoop".to_string(), loop_output.to_json());
        }
        if pm_quality_visible_debug {
            if let Some(obj) = verify_detail.as_object_mut() {
                obj.insert(
                    "subtaskGaps".to_string(),
                    serde_json::json!(depth_gate_result.subtask_gap_titles.clone()),
                );
                obj.insert(
                    "dimensionGaps".to_string(),
                    serde_json::json!(depth_gate_result.dimension_gap_titles.clone()),
                );
                obj.insert(
                    "missing".to_string(),
                    serde_json::json!(quality.missing.clone()),
                );
                obj.insert(
                    "reportStrategyMissingChecks".to_string(),
                    serde_json::json!(report_strategy_gate_result.missing_checks.clone()),
                );
            }
        }
        on_stage("verify", "completed", attempt, Some(verify_detail));
        if let Some(loop_output) = deep_loop_output.as_ref() {
            let evidence_score_detail = serde_json::json!({
                "event": "pm.deep_loop.evidence_scored",
                "loopState": "score_evidence",
                "scores": loop_output.scores,
                "evidenceScore": loop_output.evidence_score,
                "expertReviewScore": loop_output.expert_review_score,
                "llmExpertReview": llm_expert_review.as_ref().map(PmLlmExpertReview::to_json),
                "llmExpertReviewTrace": llm_expert_review_trace.clone(),
                "researchBranchQueue": loop_output.research_branch_queue,
                "hypothesisEvidenceGraph": loop_output.hypothesis_evidence_graph,
                "goldenEvalHints": loop_output.golden_eval_hints,
                "attempt": attempt,
                "externalSearchAvailable": external_search_available,
                "evidenceAdmission": effective_evidence_admission_report.to_json(),
                "currentAttemptEvidenceAdmission": evidence_admission_report.to_json(),
            });
            record_pm_audit_event(
                state.telemetry_db(),
                tenant_id,
                user_id,
                &run_id,
                "pm.deep_loop.evidence_scored",
                "info",
                "PM deep research evidence scored",
                Some(&evidence_score_detail),
            )
            .await;
            let event_name = match loop_output.decision.action {
                PmDeepResearchAction::ContinueResearch => "pm.deep_loop.gap_detected",
                PmDeepResearchAction::Rewrite => {
                    if loop_output.degraded {
                        "pm.deep_loop.degraded_synthesis"
                    } else {
                        "pm.deep_loop.quality_failed"
                    }
                }
                PmDeepResearchAction::Finalize => "pm.deep_loop.finalized",
                PmDeepResearchAction::AskClarification => "pm.deep_loop.gap_detected",
            };
            let loop_detail = serde_json::json!({
                "event": event_name,
                "loopState": loop_output.state.as_str(),
                "decision": loop_output.decision.to_json(),
                "scores": loop_output.scores,
                "evidenceScore": loop_output.evidence_score,
                "expertLensMatrix": loop_output.expert_lens_matrix.to_json(),
                "expertReviewScore": loop_output.expert_review_score,
                "llmExpertReview": llm_expert_review.as_ref().map(PmLlmExpertReview::to_json),
                "llmExpertReviewTrace": llm_expert_review_trace.clone(),
                "researchBranchQueue": loop_output.research_branch_queue,
                "hypothesisEvidenceGraph": loop_output.hypothesis_evidence_graph,
                "goldenEvalHints": loop_output.golden_eval_hints,
                "attempt": attempt,
                "maxAttempts": max_attempts,
                "noNewEvidenceRepeats": no_new_evidence_repeats,
                "externalSearchAvailable": external_search_available,
                "evidenceAdmission": effective_evidence_admission_report.to_json(),
                "currentAttemptEvidenceAdmission": evidence_admission_report.to_json(),
            });
            on_stage("deep_loop", "running", attempt, Some(loop_detail.clone()));
            record_pm_audit_event(
                state.telemetry_db(),
                tenant_id,
                user_id,
                &run_id,
                event_name,
                if matches!(loop_output.decision.action, PmDeepResearchAction::Finalize) {
                    "info"
                } else {
                    "warn"
                },
                "PM deep research loop decision",
                Some(&loop_detail),
            )
            .await;
            if matches!(
                loop_output.decision.action,
                PmDeepResearchAction::ContinueResearch | PmDeepResearchAction::AskClarification
            ) && !loop_output.decision.next_queries.is_empty()
            {
                let followup_detail = serde_json::json!({
                    "event": "pm.deep_loop.followup_planned",
                    "loopState": "branch_followup_research",
                    "nextQueries": loop_output.decision.next_queries,
                    "missingEvidence": loop_output.decision.missing_evidence,
                    "weakClaims": loop_output.decision.weak_claims,
                    "attempt": attempt,
                });
                record_pm_audit_event(
                    state.telemetry_db(),
                    tenant_id,
                    user_id,
                    &run_id,
                    "pm.deep_loop.followup_planned",
                    "info",
                    "PM deep research follow-up retrieval planned",
                    Some(&followup_detail),
                )
                .await;
            }
        }
        let mut gap_titles_for_retry = depth_gate_result.gap_repair_plan.target_subtasks.clone();
        let mut strict_focus_for_next: Option<String> = None;
        if strict_subtask_closure_enabled {
            if depth_gate_result.gap_repair_plan.enabled {
                strict_focus_for_next = pick_pm_subtask_focus_for_repair(
                    &mut pending_subtask_repair_queue,
                    &mut subtask_repair_attempts,
                    &gap_titles_for_retry,
                    subtask_max_repair_attempts,
                );
                if let Some(title) = strict_focus_for_next.as_ref() {
                    gap_titles_for_retry = vec![title.clone()];
                } else {
                    gap_titles_for_retry.clear();
                }
            } else {
                active_subtask_focus = None;
                pending_subtask_repair_queue.clear();
                subtask_repair_attempts.clear();
            }
        } else if !depth_gate_result.gap_repair_plan.enabled {
            active_subtask_focus = None;
        }

        let coverage_gap_present = queued_subtask_estimate_verify > 0
            || !depth_gate_result.coverage_gate.passed
            || !depth_gate_result.dimension_gap_titles.is_empty();
        let coverage_retry_variant = if depth_gate_result.gap_repair_plan.enabled {
            pick_pm_subtask_gap_retry_variant_for_attempt(
                &plan,
                &gap_titles_for_retry,
                attempt.saturating_add(1),
            )
        } else {
            None
        };
        let coverage_repair_target_selected = coverage_retry_variant.is_some()
            && (!strict_subtask_closure_enabled || strict_focus_for_next.is_some());
        let coverage_repair_has_fresh_route =
            coverage_retry_variant.as_ref().is_some_and(|variant| {
                pm_variant_has_fresh_route(
                    variant,
                    &plan_enabled_routes,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                    &used_retrieval_keys,
                )
            });
        let coverage_repair_actionable = pm_coverage_repair_is_actionable(
            coverage_gap_present,
            depth_gate_result.gap_repair_plan.enabled,
            coverage_repair_target_selected,
            coverage_repair_has_fresh_route,
            attempt,
            max_attempts,
        );
        let llm_review_retry_variant = llm_expert_review.as_ref().and_then(|review| {
            if attempt >= max_attempts || !review.recommends_targeted_research() {
                return None;
            }
            review.next_queries.iter().find_map(|query| {
                pm_variant_has_fresh_route(
                    query,
                    &plan_enabled_routes,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                    &used_retrieval_keys,
                )
                .then(|| query.clone())
            })
        });
        let llm_review_repair_actionable = llm_review_retry_variant.is_some();
        let targeted_repair_actionable = coverage_repair_actionable || llm_review_repair_actionable;

        let deep_loop_action = deep_loop_output
            .as_ref()
            .map(|output| output.decision.action);
        let llm_review_recommends_finalize = llm_expert_review
            .as_ref()
            .is_some_and(PmLlmExpertReview::recommends_finalize);
        let llm_review_recommends_rewrite = llm_expert_review
            .as_ref()
            .is_some_and(PmLlmExpertReview::recommends_rewrite);
        let deep_loop_retrieval_exhausted = deep_loop_enabled
            && matches!(
                deep_loop_action,
                Some(
                    PmDeepResearchAction::ContinueResearch | PmDeepResearchAction::AskClarification
                )
            )
            // A generic loop-generated branch is not enough reason to spend a
            // second retrieval wave. Continue only for a deterministic
            // subtask repair or a semantic review with a specific fresh query.
            && !targeted_repair_actionable;
        if deep_loop_retrieval_exhausted {
            let exhausted_detail = serde_json::json!({
                "event": "pm.deep_loop.retrieval_exhausted",
                "loopState": "converge_without_repeating_retrieval",
                "attempt": attempt,
                "usedRetrievalKeyCount": used_retrieval_keys.len(),
                "usedRetrievalVariantCount": used_retrieval_variants.len(),
                "decision": deep_loop_output.as_ref().map(|output| output.decision.to_json()),
            });
            on_stage(
                "deep_loop",
                "running",
                attempt,
                Some(exhausted_detail.clone()),
            );
            record_pm_audit_event(
                state.telemetry_db(),
                tenant_id,
                user_id,
                &run_id,
                "pm.deep_loop.retrieval_exhausted",
                "warn",
                "PM deep research loop exhausted fresh retrieval queries and will converge through final editor",
                Some(&exhausted_detail),
            )
            .await;
        }
        let deep_loop_should_finalize =
            matches!(deep_loop_action, Some(PmDeepResearchAction::Finalize))
                || llm_review_recommends_finalize;
        let deep_loop_should_rewrite =
            matches!(deep_loop_action, Some(PmDeepResearchAction::Rewrite))
                || llm_review_recommends_rewrite
                || deep_loop_retrieval_exhausted;
        let base_should_finish_attempt = if deep_loop_enabled {
            deep_loop_should_finalize || deep_loop_should_rewrite || attempt >= max_attempts
        } else {
            quality.passed || attempt >= max_attempts
        };
        let deep_loop_convergence_required = deep_loop_enabled
            && (orchestration_started.elapsed().as_secs() >= deep_loop_max_wall_secs
                || no_new_evidence_repeats >= deep_loop_no_new_evidence_limit
                || attempt >= max_attempts);
        let should_finish_attempt = pm_deep_loop_should_finish_attempt(
            base_should_finish_attempt,
            targeted_repair_actionable,
            deep_loop_convergence_required,
        );

        if coverage_gap_present {
            let coverage_detail = serde_json::json!({
                "attempt": attempt,
                "maxAttempts": max_attempts,
                "remainingAttempts": max_attempts.saturating_sub(attempt),
                "plannedSubtaskCount": planned_subtask_count_verify,
                "executedSubtaskCount": executed_subtask_count_verify,
                "queuedSubtaskEstimate": queued_subtask_estimate_verify,
                "knowledgeCoverageRatio": knowledge_coverage_ratio_verify,
                "subtaskCoveragePassed": depth_gate_result.coverage_gate.passed,
                "subtaskGapCount": depth_gate_result.subtask_gap_titles.len(),
                "dimensionGapCount": depth_gate_result.dimension_gap_titles.len(),
                "subtaskGaps": depth_gate_result.subtask_gap_titles.clone(),
                "dimensionGaps": depth_gate_result.dimension_gap_titles.clone(),
                "qualityPassed": quality.passed,
                "qualityLevel": quality.quality_level,
                "maxCallsPerSource": source_quota_limit,
                "routeUsageCounts": route_usage_counts.clone(),
                "routeBlocklist": route_blocklist.clone(),
                "repairActionable": coverage_repair_actionable,
                "llmReviewRepairActionable": llm_review_repair_actionable,
                "repairTarget": strict_focus_for_next.clone(),
                "repairVariant": coverage_retry_variant.clone(),
                "willContinue": !should_finish_attempt,
            });
            if should_finish_attempt {
                tracing::warn!(
                    run_id = %run_id,
                    session_id = %session_id,
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    attempt = attempt,
                    planned_subtasks = planned_subtask_count_verify,
                    executed_subtasks = executed_subtask_count_verify,
                    queued_subtasks = queued_subtask_estimate_verify,
                    knowledge_coverage_ratio = knowledge_coverage_ratio_verify,
                    subtask_gap_count = depth_gate_result.subtask_gap_titles.len(),
                    dimension_gap_count = depth_gate_result.dimension_gap_titles.len(),
                    "pm knowledge coverage remained incomplete at terminal synthesis"
                );
                record_pm_audit_event(
                    state.telemetry_db(),
                    tenant_id,
                    user_id,
                    &run_id,
                    "pm_knowledge_coverage_warning",
                    "warn",
                    "terminal synthesis retained unresolved knowledge coverage gaps",
                    Some(&coverage_detail),
                )
                .await;
            } else {
                tracing::info!(
                    run_id = %run_id,
                    session_id = %session_id,
                    tenant_id = %tenant_id,
                    user_id = %user_id,
                    attempt = attempt,
                    planned_subtasks = planned_subtask_count_verify,
                    executed_subtasks = executed_subtask_count_verify,
                    queued_subtasks = queued_subtask_estimate_verify,
                    knowledge_coverage_ratio = knowledge_coverage_ratio_verify,
                    repair_actionable = coverage_repair_actionable,
                    "pm knowledge coverage gaps detected; research will continue"
                );
                record_pm_audit_event(
                    state.telemetry_db(),
                    tenant_id,
                    user_id,
                    &run_id,
                    "pm_knowledge_coverage_progress",
                    "info",
                    "knowledge coverage gaps detected; a subsequent research attempt is scheduled",
                    Some(&coverage_detail),
                )
                .await;
            }
        }

        if should_finish_attempt {
            let (mut final_turn, mut final_quality) = if quality.passed || deep_loop_should_finalize
            {
                (turn, quality)
            } else {
                pick_preferred_pm_result(turn, quality, &best_turn, &best_quality)
            };
            if let Some(loop_output) = deep_loop_output.as_ref() {
                if deep_loop_should_rewrite
                    && pm_turn_route_allows_deep_strategy(&plan, user_message)
                {
                    if !final_quality
                        .missing
                        .iter()
                        .any(|item| item == "deep_loop_requested_llm_rewrite")
                    {
                        final_quality
                            .missing
                            .push("deep_loop_requested_llm_rewrite".to_string());
                    }
                    let suggestion =
                        "Deep Research Loop requested an LLM rewrite; preserved the synthesized answer and sent it through the final editor instead of replacing it with a deterministic strategy package."
                            .to_string();
                    if !final_quality
                        .suggestions
                        .iter()
                        .any(|item| item == &suggestion)
                    {
                        final_quality.suggestions.push(suggestion);
                    }
                }
                let remaining_for_editor_secs = runtime_budget
                    .pipeline_timeout_secs
                    .saturating_sub(orchestration_started.elapsed().as_secs());
                final_turn = merge_pm_turn_with_observed_tool_calls(
                    final_turn,
                    &effective_evidence_admission_report.accepted_tool_calls,
                );
                emit_pm_answer_snapshot(answer_delta.as_ref(), "final_candidate", &final_turn.text);
                let (edited_turn, final_editor_trace) = run_pm_llm_final_editor_if_needed(
                    manager.clone(),
                    tenant_id,
                    user_id,
                    model,
                    user_message,
                    &plan,
                    final_turn,
                    &final_quality,
                    llm_expert_review
                        .as_ref()
                        .or(retained_llm_expert_review.as_ref()),
                    remaining_for_editor_secs,
                )
                .await;
                final_turn = edited_turn;
                final_quality = evaluate_pm_answer_quality(&final_turn);
                apply_pm_evidence_admission_gate(
                    &mut final_quality,
                    &final_turn.text,
                    effective_evidence_admission_report,
                );
                apply_pm_contract_gate(&mut final_quality, &final_turn.text, &runtime_budget);
                apply_pm_conflict_gate(&mut final_quality);
                apply_pm_report_strategy_quality_gate(
                    &mut final_quality,
                    &plan,
                    user_message,
                    &final_turn.text,
                );
                let loop_final_detail = serde_json::json!({
                    "event": "pm.deep_loop.finalized",
                    "loopState": loop_output.state.as_str(),
                    "decision": loop_output.decision.to_json(),
                    "scores": loop_output.scores,
                    "expertReviewScore": loop_output.expert_review_score,
                    "researchBranchQueue": loop_output.research_branch_queue,
                    "hypothesisEvidenceGraph": loop_output.hypothesis_evidence_graph,
                    "goldenEvalHints": loop_output.golden_eval_hints,
                    "llmExpertReview": llm_expert_review
                        .as_ref()
                        .or(retained_llm_expert_review.as_ref())
                        .map(PmLlmExpertReview::to_json),
                    "llmExpertReviewTrace": if llm_expert_review_completed {
                        retained_llm_expert_review_trace.clone()
                    } else {
                        llm_expert_review_trace.clone()
                    },
                    "llmFinalEditor": final_editor_trace.clone(),
                    "degraded": loop_output.degraded,
                    "strategyPackage": loop_output.strategy_package.as_ref().map(|p| p.to_json()),
                });
                on_stage(
                    "deep_loop",
                    "completed",
                    attempt,
                    Some(loop_final_detail.clone()),
                );
                record_pm_audit_event(
                    state.telemetry_db(),
                    tenant_id,
                    user_id,
                    &run_id,
                    "pm.deep_loop.finalized",
                    "info",
                    "PM deep research loop finalized",
                    Some(&loop_final_detail),
                )
                .await;
            }
            on_stage(
                "synthesize",
                pm_synthesize_stage_status(&final_quality),
                attempt,
                Some(serde_json::json!({
                    "answerLength": final_turn.text.chars().count(),
                    "qualityGatePassed": final_quality.passed,
                    "deliverable": final_quality.deliverable,
                    "qualityLevel": final_quality.quality_level,
                    "deepLoop": deep_loop_output.as_ref().map(|output| output.to_json()),
                    "llmExpertReview": llm_expert_review_trace,
                })),
            );
            for meta in &subtask_runtime_metas {
                if attempt_subtask_candidate_counts.contains_key(&meta.key) {
                    continue;
                }
                let _ = upsert_pm_subtask_run(
                    state.telemetry_db(),
                    &PmSubtaskRunUpsertPayload {
                        run_id: run_id.clone(),
                        task_id: cancel_task_id.map(std::string::ToString::to_string),
                        tenant_id: tenant_id.to_string(),
                        user_id: user_id.to_string(),
                        session_id: session_id.to_string(),
                        subtask_key: meta.key.clone(),
                        subtask_id: meta.subtask_id.clone(),
                        title: meta.title.clone(),
                        goal: meta.goal.clone(),
                        deliverable: meta.deliverable.clone(),
                        required_evidence_type: meta.required_evidence_type.clone(),
                        priority: meta.priority.clone(),
                        status: "skipped".to_string(),
                        probe_candidate_count: 0,
                        probe_completed_count: 0,
                        citation_count: 0,
                        domain_count: 0,
                        tool_call_count: 0,
                        quality_score: None,
                        error_code: None,
                        error_message: None,
                        detail: Some(serde_json::json!({
                            "attempt": attempt,
                            "reason": "not_selected_by_probe_budget",
                            "probeCandidateMax": probe_candidate_cap,
                        })),
                    },
                )
                .await;
            }
            return finalize_pm_orchestration_result(
                state.telemetry_db(),
                tenant_id,
                &run_id,
                session_id,
                final_turn,
                final_quality,
            )
            .await;
        }

        let next_attempt = attempt + 1;
        let has_depth_gap = quality.missing.iter().any(|item| {
            item.starts_with("subtask_depth_gap:")
                || item.starts_with("dimension_gap:")
                || item.starts_with("subtask_probe_gap:")
        });
        let strategy = if has_depth_gap {
            PmRepairStrategy::SwitchQuery
        } else {
            pm_retry_strategy(next_attempt)
        };
        let strategy_key = strategy.as_key();
        let (
            mut next_variant,
            mut next_route_id,
            mut next_route_channel,
            mut next_execution_channel,
        ) = pick_pm_attempt_preferences_for_strategy(
            &plan_query_variants,
            &plan_enabled_routes,
            strategy,
            next_attempt,
        );
        if is_pm_route_over_quota(
            &route_usage_counts,
            next_route_id.as_deref(),
            next_route_channel.as_deref(),
            source_quota_limit,
        ) || is_pm_route_blocked(
            &route_blocklist,
            next_route_id.as_deref(),
            next_route_channel.as_deref(),
        ) {
            let (
                quota_variant,
                quota_route_id,
                quota_route_channel,
                quota_execution_channel,
                quota_exhausted,
            ) = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
                &plan_query_variants,
                &plan_enabled_routes,
                next_attempt,
                &route_usage_counts,
                &route_blocklist,
                source_quota_limit,
                &used_retrieval_keys,
            );
            if quota_exhausted {
                if let Some(turn) = last_usable_turn.clone() {
                    let quality = degrade_pm_quality_with_reason(
                        last_usable_quality
                            .clone()
                            .unwrap_or_else(|| evaluate_pm_answer_quality(&turn)),
                        "source_quota_exhausted",
                        "All enabled sources reached per-source quota during repair planning.",
                    );
                    on_stage(
                        "synthesize",
                        "completed",
                        next_attempt,
                        Some(serde_json::json!({
                            "answerLength": turn.text.chars().count(),
                            "qualityGatePassed": false,
                            "reason": "source_quota_exhausted_partial_answer_kept"
                        })),
                    );
                    return finalize_pm_orchestration_result(
                        state.telemetry_db(),
                        tenant_id,
                        &run_id,
                        session_id,
                        turn,
                        quality,
                    )
                    .await;
                }
                if let Ok(turn) = run_pm_force_synthesize_fallback_turn_with_observed_tools(
                    manager.clone(),
                    session_id,
                    session_source,
                    user_message,
                    &probe_outcomes,
                    next_attempt,
                    &accumulated_observed_tool_calls,
                    answer_delta.clone(),
                )
                .await
                {
                    let quality = degrade_pm_quality_with_reason(
                        evaluate_pm_answer_quality(&turn),
                        "source_quota_exhausted_force_synthesize",
                        "All enabled sources reached per-source quota during repair planning; forced synthesis.",
                    );
                    on_stage(
                        "synthesize",
                        "completed",
                        next_attempt,
                        Some(serde_json::json!({
                            "answerLength": turn.text.chars().count(),
                            "qualityGatePassed": false,
                            "reason": "source_quota_exhausted_force_synthesize"
                        })),
                    );
                    return finalize_pm_orchestration_result(
                        state.telemetry_db(),
                        tenant_id,
                        &run_id,
                        session_id,
                        turn,
                        quality,
                    )
                    .await;
                }
            } else {
                if next_variant.is_none() {
                    next_variant = quota_variant;
                }
                next_route_id = quota_route_id;
                next_route_channel = quota_route_channel;
                next_execution_channel = quota_execution_channel;
            }
        }
        if depth_gate_result.gap_repair_plan.enabled {
            if strict_subtask_closure_enabled {
                if let Some(focus) = strict_focus_for_next.clone() {
                    let key = normalize_claim_key(&focus);
                    let entry = subtask_repair_attempts.entry(key).or_insert(0);
                    *entry = entry.saturating_add(1);
                    active_subtask_focus = Some(focus);
                } else {
                    active_subtask_focus = None;
                }
            }
            if let Some(target_variant) = coverage_retry_variant.clone() {
                if pm_variant_has_fresh_route(
                    &target_variant,
                    &plan_enabled_routes,
                    &route_usage_counts,
                    &route_blocklist,
                    source_quota_limit,
                    &used_retrieval_keys,
                ) {
                    next_variant = Some(target_variant);
                } else {
                    on_stage(
                        "retry_repair",
                        "running",
                        next_attempt,
                        Some(serde_json::json!({
                            "strategy": "skip_repeated_subtask_gap_query",
                            "targetVariant": target_variant,
                            "usedRetrievalKeyCount": used_retrieval_keys.len(),
                        })),
                    );
                }
            }
        } else {
            active_subtask_focus = None;
        }
        if let Some(target_variant) = llm_review_retry_variant.clone() {
            next_variant = Some(target_variant);
        }
        let selected_deep_branch = if deep_loop_enabled
            && llm_review_retry_variant.is_none()
            && matches!(
                deep_loop_action,
                Some(PmDeepResearchAction::ContinueResearch)
            ) {
            deep_loop_output
                .as_ref()
                .and_then(|output| output.research_branch_queue.select_next_external_branch())
                .cloned()
        } else {
            None
        };
        if let Some(branch) = selected_deep_branch.as_ref() {
            if let Some(query) = branch.queries.iter().find(|query| {
                let query = query.trim();
                !query.is_empty()
                    && pm_variant_has_fresh_route(
                        query,
                        &plan_enabled_routes,
                        &route_usage_counts,
                        &route_blocklist,
                        source_quota_limit,
                        &used_retrieval_keys,
                    )
            }) {
                next_variant = Some(query.clone());
                active_subtask_focus = Some(branch.title.clone());
                let branch_detail = serde_json::json!({
                    "event": "pm.deep_loop.branch_selected",
                    "loopState": "branch_followup_research",
                    "branchId": branch.id,
                    "branchTitle": branch.title,
                    "lens": branch.lens,
                    "priority": branch.priority,
                    "query": query,
                    "attempt": next_attempt,
                    "costGuard": {
                        "onlyWhenDeepLoopContinue": true,
                        "simpleTurnsBypassRetrieval": true,
                        "usesExistingRetryAttempt": true
                    }
                });
                on_stage(
                    "deep_loop",
                    "running",
                    next_attempt,
                    Some(branch_detail.clone()),
                );
                record_pm_audit_event(
                    state.telemetry_db(),
                    tenant_id,
                    user_id,
                    &run_id,
                    "pm.deep_loop.branch_selected",
                    "info",
                    "PM deep research branch selected for next retrieval attempt",
                    Some(&branch_detail),
                )
                .await;
            }
        }
        on_stage(
            "retry_repair",
            "running",
            next_attempt,
            Some(serde_json::json!({
            "message": "正在补齐未达标子任务证据",
            "strategy": strategy_key,
            "nextVariant": next_variant.clone(),
            "nextRoute": next_route_id.clone(),
            "nextRouteChannel": next_route_channel.clone(),
            "nextExecutionChannel": next_execution_channel.clone(),
            "targetSubtask": active_subtask_focus.clone(),
                "deepResearchBranch": selected_deep_branch.as_ref().map(|branch| serde_json::json!({
                    "id": branch.id,
                    "title": branch.title,
                    "lens": branch.lens,
                    "priority": branch.priority,
                })),
                "llmExpertReviewDecision": llm_expert_review.as_ref().map(|review| review.decision.as_str()),
                "closureQueueSize": pending_subtask_repair_queue.len(),
            })),
        );
        let retry_started = Instant::now();
        let retry_probe_context_outcomes =
            if evidence_admission_report.accepted_probe_outcomes.is_empty() {
                &[][..]
            } else {
                evidence_admission_report.accepted_probe_outcomes.as_slice()
            };
        let probe_context = build_pm_probe_repair_context(retry_probe_context_outcomes);
        let previous_answer_for_retry = if probe_context.is_empty() {
            turn.text.clone()
        } else {
            format!("{}\n\n{}", turn.text, probe_context)
        };
        current_message = wrap_pm_research_prompt(
            session_source,
            build_pm_retry_prompt(
                user_message,
                &previous_answer_for_retry,
                &quality,
                strategy,
                next_attempt,
                next_variant.as_deref(),
                next_route_id.as_deref(),
                next_route_channel.as_deref(),
                next_execution_channel.as_deref(),
                &runtime_budget,
                if next_execution_channel
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                    || next_route_channel
                        .as_deref()
                        .is_some_and(|x| x.eq_ignore_ascii_case("browser"))
                {
                    runtime_budget.source_slot_browser_secs
                } else {
                    runtime_budget.source_slot_search_secs
                },
                &merge_blocked_domains(
                    blocked_domains_from_usage(&domain_usage_counts, domain_quota_limit),
                    &open_domain_circuit_keys,
                ),
            ),
        );
        current_attempt_strategy = Some(strategy);
        on_stage(
            "retry_repair",
            "completed",
            next_attempt,
            Some(serde_json::json!({
                "durationMs": retry_started.elapsed().as_millis(),
                "strategy": strategy_key,
            })),
        );
        attempt = next_attempt;
    }
}

async fn prepare_pm_orchestration_plan(
    manager: Arc<AgentSessionManager>,
    db: &sqlx::SqlitePool,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    runtime_budget: PmTimeoutBudget,
    resume_checkpoint: Option<&PmResumeCheckpoint>,
    session_mcp_servers: &[String],
    session_skills: &[String],
    on_stage: &mut PmStageCallback<'_>,
) -> Result<PmPreparedOrchestrationPlan, GatewayError> {
    let resume_stage = resume_checkpoint.and_then(|cp| cp.stage.clone());
    let resume_attempt = resume_checkpoint.map(|cp| cp.attempt.max(1)).unwrap_or(1);
    let resume_detail = resume_checkpoint.and_then(|cp| cp.detail.clone());
    let resume_skip_preflight = resume_stage
        .as_deref()
        .is_some_and(|stage| stage != "preflight");
    let resume_skip_understand_task_plan = resume_stage.as_deref().is_some_and(|stage| {
        stage == "planner"
            || stage == "retrieve"
            || stage == "verify"
            || stage == "retry_repair"
            || stage == "synthesize"
    });
    let resume_skip_planner = resume_stage.as_deref().is_some_and(|stage| {
        stage == "retrieve" || stage == "verify" || stage == "retry_repair" || stage == "synthesize"
    });

    let announced_understand_running = !resume_skip_understand_task_plan;

    let mut preflight_for_routing: Option<PmStartupPreflightOutcome> = None;
    if resume_skip_preflight {
        on_stage(
            "preflight",
            "completed",
            1,
            Some(serde_json::json!({
                "resumed": true,
                "previousTaskId": resume_checkpoint.as_ref().map(|cp| cp.previous_task_id.as_str()),
                "fromStage": resume_stage.clone(),
                "fromAttempt": resume_attempt,
                "message": "resume skipped repeated preflight checks",
            })),
        );
    } else {
        on_stage("preflight", "running", 1, None);
        let preflight_started = Instant::now();
        let preflight = run_pm_startup_preflight(
            manager.clone(),
            user_id,
            tenant_id,
            Some(session_id),
            model,
            true,
            &runtime_budget,
        )
        .await;
        if !preflight.model_probe_skipped {
            upsert_pm_provider_health(
                db,
                tenant_id,
                model,
                "model",
                preflight.model_stream_ok,
                preflight.model_latency_ms,
                preflight.model_error.as_deref(),
            )
            .await;
        }
        if !preflight.retrieval_probe_skipped {
            upsert_pm_provider_health(
                db,
                tenant_id,
                "retrieval.search",
                "search",
                preflight.retrieval_search_ok,
                preflight.retrieval_search_latency_ms,
                preflight.retrieval_search_error.as_deref(),
            )
            .await;
        }
        let mut preflight_detail = preflight.to_stage_detail(true);
        if let Some(obj) = preflight_detail.as_object_mut() {
            obj.insert(
                "durationMs".to_string(),
                serde_json::json!(preflight_started.elapsed().as_millis()),
            );
        }
        if !preflight.passed(true) {
            on_stage("preflight", "failed", 1, Some(preflight_detail));
            return Err(GatewayError::Internal(preflight.user_facing_error(true)));
        }
        on_stage("preflight", "completed", 1, Some(preflight_detail));
        preflight_for_routing = Some(preflight);
    }

    let planner_started = Instant::now();
    let mut plan = if resume_skip_planner {
        resume_checkpoint
            .and_then(|cp| cp.planner_plan.clone())
            .unwrap_or_else(|| {
                build_pm_stage_plan(user_message, &session_mcp_servers, &session_skills)
            })
    } else {
        build_pm_stage_plan(user_message, &session_mcp_servers, &session_skills)
    };
    let (learned_scores, route_health_scores) = tokio::join!(
        load_pm_route_scores(db, tenant_id),
        load_pm_route_health_scores(db, tenant_id)
    );
    rank_pm_plan_routes(
        &mut plan,
        preflight_for_routing.as_ref(),
        &learned_scores,
        &route_health_scores,
        user_message,
    );
    let inject_historical_hints = pm_flag_enabled("PM_INCLUDE_HISTORICAL_HINTS_IN_PROMPT", false);
    let historical_hints = if inject_historical_hints {
        load_pm_historical_evidence_hints(db, tenant_id, user_message).await
    } else {
        Vec::new()
    };
    if inject_historical_hints && !historical_hints.is_empty() {
        if let Some(obj) = plan.as_object_mut() {
            obj.insert(
                "historicalEvidenceHints".to_string(),
                serde_json::Value::Array(historical_hints.clone()),
            );
        }
    }
    let mut retrieve_budget = runtime_budget;
    let allow_tighter_source_slot_from_contract =
        pm_flag_enabled("PM_ALLOW_TIGHTER_SOURCE_SLOT_FROM_CONTRACT", false);
    let apply_exec_constraints_budget_tightening =
        pm_flag_enabled("PM_APPLY_EXEC_CONSTRAINT_BUDGETS", false);
    let source_slot_min_effective_secs =
        pm_env_u64("PM_SOURCE_SLOT_MIN_EFFECTIVE_SECS", 90).max(30);
    let strict_route_mode = pm_flag_enabled("PM_ROUTE_STRICT_MODE", true);
    let mut exec_constraints_stage_detail: Option<serde_json::Value> = None;
    if resume_skip_understand_task_plan {
        on_stage(
            "understand",
            "completed",
            1,
            Some(serde_json::json!({
                "resumed": true,
                "fromStage": resume_stage.clone(),
                "fromAttempt": resume_attempt,
                "message": "resume skipped repeated understand stage",
            })),
        );
        on_stage(
            "task_plan",
            "completed",
            1,
            Some(serde_json::json!({
                "resumed": true,
                "fromStage": resume_stage.clone(),
                "fromAttempt": resume_attempt,
                "message": "resume skipped repeated task planning stage",
            })),
        );
    } else {
        let preface_started = Instant::now();
        if !announced_understand_running {
            on_stage("understand", "running", 1, None);
        }
        let preface_prompt = wrap_pm_research_prompt(
            session_source,
            build_pm_understand_plan_prompt(user_message, &plan, &runtime_budget),
        );
        let mut preface_text = String::new();
        let mut preface_thinking: Option<String> = None;
        let mut preface_error: Option<String> = None;
        let mut preface_options = agent_gateway::AgentTurnOptions {
            blocked_tools: pm_blocked_non_search_research_tools(),
            prefer_native_web_search: false,
            suppress_native_web_search: true,
            reasoning_budget: agent_gateway::InternalReasoningBudget::Standard,
            ..agent_gateway::AgentTurnOptions::default()
        };
        preface_options.system_instructions.push(
            "Internal PM routing/planning turn. Do not call tools, search, browse, fetch URLs, or inspect resources. Classify and plan only from the user request and provided hints."
                .to_string(),
        );
        let preface_timeout_secs = pm_effective_preface_turn_timeout_secs(&plan);
        match run_pm_internal_turn_with_timeout_cleanup_and_options(
            manager.clone(),
            user_id,
            tenant_id,
            Some(model),
            Some(session_source),
            preface_prompt,
            preface_timeout_secs,
            "preface turn",
            preface_options,
        )
        .await
        {
            Ok(preface_turn) => {
                preface_thinking = preface_turn
                    .thinking
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.chars().take(3200).collect::<String>());
                preface_text = preface_turn.text.trim().to_string();
                if preface_text.is_empty() {
                    if !preface_turn.tool_calls.is_empty() {
                        preface_error = Some("preface_tool_only_no_text".to_string());
                    } else {
                        preface_error = Some("preface_empty_text".to_string());
                    }
                }
            }
            Err(error) => {
                preface_error = Some(error.to_string());
            }
        }

        if preface_text.is_empty() {
            preface_text = build_pm_preface_fallback(user_message, &plan);
        }
        let mut task_graph = extract_pm_task_graph(&preface_text);
        let mut fallback_task_graph_used = false;
        if task_graph.is_none() && pm_flag_enabled("PM_ENABLE_FALLBACK_TASK_GRAPH", true) {
            task_graph = build_pm_fallback_task_graph(user_message, &plan);
            fallback_task_graph_used = task_graph.is_some();
        }
        if let Some(graph) = task_graph.as_ref() {
            apply_pm_task_graph_to_plan(&mut plan, graph);
        }
        let turn_route = extract_pm_turn_route(&preface_text)
            .unwrap_or_else(|| build_pm_fallback_turn_route(user_message, &plan));
        let turn_route = guard_pm_report_strategy_route(turn_route, &mut plan, user_message);
        apply_pm_turn_route_to_plan(&mut plan, &turn_route);
        let skip_execution_contracts =
            pm_route_can_skip_execution_contracts(&turn_route, user_message);
        let mut task_graph_contract_repaired = false;
        let mut task_graph_contract_repair_attempts = 0usize;
        let mut task_graph_contract_repair_failures = Vec::<String>::new();
        let mut contract_repaired = false;
        let mut contract_repair_attempts = 0usize;
        let mut exec_constraints_contract_repair_failures = Vec::<String>::new();

        if skip_execution_contracts {
            exec_constraints_stage_detail = Some(serde_json::json!({
                "skipped": true,
                "reason": "turn_router_selected_direct_answer_without_search",
                "turnRoute": turn_route.to_json(),
            }));
        } else {
            let model_contract_repair_enabled =
                pm_flag_enabled("PM_ENABLE_MODEL_CONTRACT_REPAIR", false);
            let initial_task_graph_issue = detect_pm_task_graph_issue(&preface_text);
            let task_graph_issue = if model_contract_repair_enabled {
                let (issue, repaired, repair_attempts, repair_failures) =
                    repair_task_graph_with_retries(
                        manager.clone(),
                        session_id,
                        session_source,
                        user_id,
                        tenant_id,
                        model,
                        user_message,
                        &mut preface_text,
                        initial_task_graph_issue,
                    )
                    .await;
                task_graph_contract_repaired = repaired;
                task_graph_contract_repair_attempts = repair_attempts;
                task_graph_contract_repair_failures = repair_failures;
                issue
            } else if initial_task_graph_issue.is_some() {
                if let Some(fallback_graph) = build_pm_fallback_task_graph(user_message, &plan) {
                    apply_pm_task_graph_to_plan(&mut plan, &fallback_graph);
                    task_graph = Some(fallback_graph);
                    task_graph_contract_repaired = true;
                }
                None
            } else {
                None
            };

            let mut contract_issue = None;
            let mut exec_constraints_source = "model_valid";
            let mut exec_constraints = if model_contract_repair_enabled {
                let initial_contract_issue =
                    detect_exec_constraints_issue(&preface_text, &runtime_budget);
                let (issue, repaired, repair_attempts, repair_failures) =
                    repair_exec_constraints_with_retries(
                        manager.clone(),
                        session_id,
                        session_source,
                        user_id,
                        tenant_id,
                        model,
                        user_message,
                        &mut preface_text,
                        &runtime_budget,
                        &plan
                            .get("sourceRoutes")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|item| item.get("routeId").and_then(|v| v.as_str()))
                                    .map(std::string::ToString::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                        strict_route_mode,
                        initial_contract_issue,
                    )
                    .await;
                contract_issue = issue;
                contract_repaired = repaired;
                contract_repair_attempts = repair_attempts;
                exec_constraints_contract_repair_failures = repair_failures;
                if repaired {
                    exec_constraints_source = "model_repaired";
                }
                match extract_pm_exec_constraints(&preface_text, &runtime_budget) {
                    Ok(exec_constraints) => Some(exec_constraints),
                    Err(_) => {
                        contract_issue = None;
                        contract_repaired = true;
                        exec_constraints_source = "server_policy_after_model_repair";
                        Some(build_pm_deterministic_exec_constraints(
                            &plan,
                            &runtime_budget,
                        ))
                    }
                }
            } else {
                match extract_pm_exec_constraints(&preface_text, &runtime_budget) {
                    Ok(exec_constraints) => Some(exec_constraints),
                    Err(_) => {
                        contract_repaired = true;
                        exec_constraints_source = "server_policy_fallback";
                        Some(build_pm_deterministic_exec_constraints(
                            &plan,
                            &runtime_budget,
                        ))
                    }
                }
            };

            if let Some(exec_constraints) = exec_constraints.as_mut() {
                exec_constraints.source_slot_budget_secs = exec_constraints
                    .source_slot_budget_secs
                    .max(source_slot_min_effective_secs);
                apply_pm_exec_constraints_to_plan(&mut plan, exec_constraints);
                let source_slot_cap = exec_constraints.source_slot_budget_secs.max(10);
                let source_slot_effective_cap = source_slot_cap.max(source_slot_min_effective_secs);
                if apply_exec_constraints_budget_tightening {
                    retrieve_budget.retrieve_max_tool_calls = retrieve_budget
                        .retrieve_max_tool_calls
                        .min(exec_constraints.tool_budget_per_attempt.max(1));
                    retrieve_budget.pipeline_timeout_secs = retrieve_budget
                        .pipeline_timeout_secs
                        .min(exec_constraints.pipeline_timeout_secs.max(60));
                }
                if allow_tighter_source_slot_from_contract
                    && apply_exec_constraints_budget_tightening
                {
                    retrieve_budget.source_slot_search_secs = retrieve_budget
                        .source_slot_search_secs
                        .min(source_slot_effective_cap);
                    retrieve_budget.source_slot_browser_secs = retrieve_budget
                        .source_slot_browser_secs
                        .min(source_slot_effective_cap.max(15));
                    retrieve_budget.source_slot_api_fetch_secs = retrieve_budget
                        .source_slot_api_fetch_secs
                        .min(source_slot_effective_cap);
                }
                exec_constraints_stage_detail = Some(serde_json::json!({
                    "routeAllowlistCount": exec_constraints.route_allowlist.len(),
                    "routePriorityCount": exec_constraints.route_priority.len(),
                    "stopConditions": exec_constraints.stop_conditions,
                    "contractSource": exec_constraints_source,
                    "taskGraphContractRepaired": task_graph_contract_repaired,
                    "taskGraphContractRepairAttempts": task_graph_contract_repair_attempts,
                    "taskGraphContractRepairFailures": task_graph_contract_repair_failures.clone(),
                    "execConstraintsContractRepairFailures": exec_constraints_contract_repair_failures.clone(),
                    "sourceSlotBudgetSecsRequested": source_slot_cap,
                    "sourceSlotBudgetFloorSecs": source_slot_min_effective_secs,
                    "sourceSlotBudgetSecsEffectiveCap": source_slot_effective_cap,
                    "budgetConvergenceApplied": apply_exec_constraints_budget_tightening,
                    "sourceSlotBudgetTighteningApplied": allow_tighter_source_slot_from_contract,
                    "sourceSlotBudgetSecs": retrieve_budget.source_slot_search_secs,
                    "toolBudgetPerAttempt": retrieve_budget.retrieve_max_tool_calls,
                    "pipelineTimeoutSecs": retrieve_budget.pipeline_timeout_secs,
                }));
            } else if preface_error.is_none() {
                let issue =
                    contract_issue.unwrap_or_else(|| "missing executable constraints".to_string());
                preface_error = Some(format!("exec_constraints_invalid:{issue}"));
            }
            if preface_error.is_none() {
                if let Some(issue) = task_graph_issue {
                    preface_error = Some(format!("task_graph_invalid:{issue}"));
                }
            }
        }
        if matches!(turn_route.turn_class, PmTurnClass::PmReportStrategy) {
            apply_pm_report_strategy_plan(&mut plan, user_message);
            enrich_pm_report_strategy_with_semantic_extraction(
                manager.clone(),
                session_id,
                session_source,
                user_message,
                &mut plan,
                on_stage,
            )
            .await;
            rank_pm_plan_routes(
                &mut plan,
                preflight_for_routing.as_ref(),
                &learned_scores,
                &route_health_scores,
                user_message,
            );
        }

        let preface_visible_text = extract_pm_preface_visible_text(&preface_text);
        let route_human_summary = pm_turn_route_human_summary(&turn_route, user_message);
        let preview = preface_visible_text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .filter(|line| !line.is_empty())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| route_human_summary.clone());
        let plan_preview = preface_text
            .lines()
            .map(str::trim)
            .find(|line| {
                line.starts_with("1.")
                    || line.starts_with("1)")
                    || line.starts_with("1、")
                    || line.starts_with("- ")
                    || line.starts_with("* ")
                    || line.starts_with("• ")
            })
            .filter(|line| !line.starts_with("TURN_ROUTE"))
            .unwrap_or(route_human_summary.as_str())
            .to_string();
        let understand_task_graph = task_graph.clone().unwrap_or(serde_json::Value::Null);
        let understand_exec_constraints = plan
            .get("execConstraints")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let understand_plan_snapshot = plan.clone();

        if let Some(error_text) = preface_error {
            tracing::warn!(
                session_id = %session_id,
                "pm preface degraded to fallback: {}",
                error_text
            );
            on_stage(
                "understand",
                "completed",
                1,
                Some(serde_json::json!({
                            "error": error_text,
                            "fallbackUsed": true,
                            "degraded": true,
                            "taskGraphFallbackUsed": fallback_task_graph_used,
                            "taskGraphContractRepaired": task_graph_contract_repaired,
                            "taskGraphContractRepairAttempts": task_graph_contract_repair_attempts,
                            "taskGraphContractRepairFailures": task_graph_contract_repair_failures,
                            "contractRepaired": contract_repaired,
                            "contractRepairAttempts": contract_repair_attempts,
                            "execConstraintsContractRepairFailures": exec_constraints_contract_repair_failures,
                            "durationMs": preface_started.elapsed().as_millis(),
                        "thinking": preface_thinking,
                        "preview": preview,
                    "humanSummary": route_human_summary,
                    "timeoutSecs": preface_timeout_secs,
                    "prefaceText": preface_visible_text,
                    "prefaceRawText": preface_text,
                    "turnRoute": turn_route.to_json(),
                    "taskGraph": understand_task_graph,
                    "execConstraints": understand_exec_constraints,
                    "planSnapshot": understand_plan_snapshot,
                })),
            );
        } else {
            on_stage(
                "understand",
                "completed",
                1,
                Some(serde_json::json!({
                    "durationMs": preface_started.elapsed().as_millis(),
                    "taskGraphFallbackUsed": fallback_task_graph_used,
                    "taskGraphContractRepaired": task_graph_contract_repaired,
                    "taskGraphContractRepairAttempts": task_graph_contract_repair_attempts,
                    "taskGraphContractRepairFailures": task_graph_contract_repair_failures,
                    "contractRepaired": contract_repaired,
                    "contractRepairAttempts": contract_repair_attempts,
                    "execConstraintsContractRepairFailures": exec_constraints_contract_repair_failures,
                    "thinking": preface_thinking,
                    "preview": preview,
                    "humanSummary": route_human_summary,
                    "timeoutSecs": preface_timeout_secs,
                    "prefaceText": preface_visible_text,
                    "prefaceRawText": preface_text,
                    "turnRoute": turn_route.to_json(),
                    "taskGraph": understand_task_graph,
                    "execConstraints": understand_exec_constraints,
                    "planSnapshot": understand_plan_snapshot,
                })),
            );
        }

        let understand_duration_ms = preface_started.elapsed().as_millis();
        let task_plan_started = Instant::now();
        let task_plan_selected_route_ids = plan
            .get("selectedRouteIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let task_plan_selected_route_count = task_plan_selected_route_ids.len();
        let task_plan_query_variant_count = plan
            .get("queryVariants")
            .and_then(|v| v.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);
        let task_plan_subtask_count = plan
            .get("taskGraph")
            .and_then(|v| v.get("subtasks"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.len())
            .unwrap_or(0);
        let task_plan_constraints_applied = plan.get("execConstraints").is_some();

        on_stage("task_plan", "running", 1, None);
        let task_plan_duration_ms = task_plan_started.elapsed().as_millis().max(1);
        on_stage(
            "task_plan",
            "completed",
            1,
            Some(serde_json::json!({
                "durationMs": task_plan_duration_ms,
                "understandDurationMs": understand_duration_ms,
                "preview": plan_preview,
                "humanSummary": route_human_summary,
                "selectedRouteIds": task_plan_selected_route_ids,
                "selectedRouteCount": task_plan_selected_route_count,
                "queryVariantCount": task_plan_query_variant_count,
                "subtaskCount": task_plan_subtask_count,
                "constraintsApplied": task_plan_constraints_applied,
                "turnRoute": plan.get("turnRoute").cloned().unwrap_or(serde_json::Value::Null),
                "taskGraph": plan.get("taskGraph").cloned().unwrap_or(serde_json::Value::Null),
                "execConstraints": plan.get("execConstraints").cloned().unwrap_or(serde_json::Value::Null),
                "parallelism": plan.get("parallelism").cloned().unwrap_or(serde_json::Value::Null),
                "queryVariants": plan.get("queryVariants").cloned().unwrap_or(serde_json::Value::Null),
                "sourceRoutes": plan.get("sourceRoutes").cloned().unwrap_or(serde_json::Value::Null),
                "planSnapshot": plan.clone(),
            })),
        );
    }

    if exec_constraints_stage_detail.is_none() {
        if let Some(exec_obj) = plan.get("execConstraints").and_then(|v| v.as_object()) {
            let source_slot_cap = exec_obj
                .get("sourceSlotBudgetSecs")
                .and_then(|v| v.as_u64())
                .unwrap_or(retrieve_budget.source_slot_search_secs)
                .max(10);
            let source_slot_effective_cap = source_slot_cap.max(source_slot_min_effective_secs);
            let tool_budget = exec_obj
                .get("toolBudgetPerAttempt")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(retrieve_budget.retrieve_max_tool_calls)
                .max(1);
            let pipeline_cap = exec_obj
                .get("pipelineTimeoutSecs")
                .and_then(|v| v.as_u64())
                .unwrap_or(retrieve_budget.pipeline_timeout_secs)
                .max(60);
            if apply_exec_constraints_budget_tightening {
                retrieve_budget.retrieve_max_tool_calls =
                    retrieve_budget.retrieve_max_tool_calls.min(tool_budget);
                retrieve_budget.pipeline_timeout_secs =
                    retrieve_budget.pipeline_timeout_secs.min(pipeline_cap);
            }
            if allow_tighter_source_slot_from_contract && apply_exec_constraints_budget_tightening {
                retrieve_budget.source_slot_search_secs = retrieve_budget
                    .source_slot_search_secs
                    .min(source_slot_effective_cap);
                retrieve_budget.source_slot_browser_secs = retrieve_budget
                    .source_slot_browser_secs
                    .min(source_slot_effective_cap.max(15));
                retrieve_budget.source_slot_api_fetch_secs = retrieve_budget
                    .source_slot_api_fetch_secs
                    .min(source_slot_effective_cap);
            }
            exec_constraints_stage_detail = Some(serde_json::json!({
                "resumedFromPlan": true,
                "sourceSlotBudgetSecsRequested": source_slot_cap,
                "sourceSlotBudgetFloorSecs": source_slot_min_effective_secs,
                "sourceSlotBudgetSecsEffectiveCap": source_slot_effective_cap,
                "budgetConvergenceApplied": apply_exec_constraints_budget_tightening,
                "sourceSlotBudgetTighteningApplied": allow_tighter_source_slot_from_contract,
                "sourceSlotBudgetSecs": retrieve_budget.source_slot_search_secs,
                "toolBudgetPerAttempt": retrieve_budget.retrieve_max_tool_calls,
                "pipelineTimeoutSecs": retrieve_budget.pipeline_timeout_secs,
                "stopConditions": exec_obj
                    .get("stopConditions")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>())
            }));
        }
    }

    let planner_visible_debug = pm_flag_enabled("PM_QUALITY_VISIBLE_DEBUG", true);
    let planner_task_graph_detail = plan.get("taskGraph").map(|graph| {
        serde_json::json!({
            "intent": graph.get("intent").and_then(|v| v.as_str()).unwrap_or("research"),
            "decompositionMode": graph
                .get("decompositionMode")
                .and_then(|v| v.as_str())
                .unwrap_or("light"),
            "subtaskCount": graph
                .get("subtasks")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len())
            .unwrap_or(0),
        })
    });
    let planner_source_route_count = plan
        .get("sourceRoutes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let planner_enabled_route_count = plan
        .get("sourceRoutes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|item| {
                    item.get("enabled")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    let planner_query_variant_count = plan
        .get("queryVariants")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let planner_mode = plan
        .get("mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("auto");

    if resume_skip_planner {
        let mut planner_detail = serde_json::json!({
            "durationMs": planner_started.elapsed().as_millis(),
            "mode": planner_mode,
            "resumed": true,
            "fromStage": resume_stage.clone(),
            "fromAttempt": resume_attempt,
            "historicalHintsInjected": inject_historical_hints,
            "routeLearningCount": learned_scores.len(),
            "historicalHintCount": historical_hints.len(),
            "sourceRouteCount": planner_source_route_count,
            "enabledRouteCount": planner_enabled_route_count,
            "queryVariantCount": planner_query_variant_count,
            "turnRoute": plan.get("turnRoute").cloned().unwrap_or(serde_json::Value::Null),
        });
        if let Some(obj) = planner_detail.as_object_mut() {
            obj.insert(
                "taskGraph".to_string(),
                planner_task_graph_detail
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "execConstraints".to_string(),
                exec_constraints_stage_detail
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "routeScoreBreakdown".to_string(),
                plan.get("routeScoreBreakdown")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert("plan".to_string(), plan.clone());
            if planner_visible_debug {
                obj.insert("debugEnabled".to_string(), serde_json::json!(true));
            }
        }
        on_stage("planner", "completed", 1, Some(planner_detail));
    } else {
        on_stage("planner", "running", 1, None);
        let mut planner_detail = serde_json::json!({
            "durationMs": planner_started.elapsed().as_millis(),
            "mode": planner_mode,
            "historicalHintsInjected": inject_historical_hints,
            "routeLearningCount": learned_scores.len(),
            "historicalHintCount": historical_hints.len(),
            "sourceRouteCount": planner_source_route_count,
            "enabledRouteCount": planner_enabled_route_count,
            "queryVariantCount": planner_query_variant_count,
            "turnRoute": plan.get("turnRoute").cloned().unwrap_or(serde_json::Value::Null),
        });
        if let Some(obj) = planner_detail.as_object_mut() {
            obj.insert(
                "taskGraph".to_string(),
                planner_task_graph_detail.unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "execConstraints".to_string(),
                exec_constraints_stage_detail
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert(
                "routeScoreBreakdown".to_string(),
                plan.get("routeScoreBreakdown")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            );
            obj.insert("plan".to_string(), plan.clone());
            if planner_visible_debug {
                obj.insert("debugEnabled".to_string(), serde_json::json!(true));
            }
        }
        on_stage("planner", "completed", 1, Some(planner_detail));
    }

    Ok(PmPreparedOrchestrationPlan {
        plan,
        runtime_budget: retrieve_budget,
        resume_detail,
        resume_skip_planner,
        resume_attempt,
    })
}

async fn run_pm_routed_shared_chat_tool_loop(
    state: &AppState,
    _db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    run_id: &str,
    session_id: &str,
    user_message: &str,
    model: &str,
    route: PmTurnRoute,
    manager: Arc<AgentSessionManager>,
    memory_instruction: Option<String>,
    on_stage: &mut PmStageCallback<'_>,
    answer_delta: Option<PmAnswerDeltaCallback>,
) -> Result<(TurnResult, PmAnswerQualityDto), GatewayError> {
    let search_enabled = matches!(
        route.search_policy,
        PmSearchPolicy::Allowed | PmSearchPolicy::Required
    );
    let timeout_secs = if search_enabled {
        pm_force_synth_turn_timeout_secs().max(300)
    } else {
        pm_direct_answer_turn_timeout_secs().max(120)
    };
    let reasoning_budget_override = pm_route_reasoning_budget_override(&route);
    let use_scratch_session = pm_shared_chat_should_use_scratch_session(&route, user_message);
    let mode = route.engine.as_str();
    let human_summary = if search_enabled {
        "已进入共享 Chat Tool Loop：模型会根据问题、历史、附件和联网工具自主完成回答。"
    } else {
        "已进入共享 Chat Tool Loop：本轮主要基于问题、历史和附件上下文回答，不强制联网。"
    };
    on_stage(
        "retrieve",
        if search_enabled {
            "running"
        } else {
            "completed"
        },
        1,
        Some(serde_json::json!({
            "mode": mode,
            "route": "pm_turn_router_shared_chat_tool_loop",
            "turnRoute": route.to_json(),
            "humanSummary": human_summary,
            "message": human_summary,
            "searchMode": if search_enabled { "on" } else { "off" },
            "searchPolicy": route.search_policy.as_str(),
            "filePolicy": route.file_policy.as_str(),
            "sharedChatTurnEngine": true,
            "scratchSession": use_scratch_session,
            "timeoutSecs": timeout_secs,
            "reasoningBudgetOverride": format!("{:?}", reasoning_budget_override).to_ascii_lowercase(),
        })),
    );
    on_stage(
        "synthesize",
        "running",
        1,
        Some(serde_json::json!({
            "mode": mode,
            "route": "pm_turn_router_shared_chat_tool_loop",
            "turnRoute": route.to_json(),
            "message": "正在由共享 Chat Tool Loop 生成回答",
            "searchMode": if search_enabled { "on" } else { "off" },
            "timeoutSecs": timeout_secs,
            "reasoningBudgetOverride": format!("{:?}", reasoning_budget_override).to_ascii_lowercase(),
            "sharedChatTurnEngine": true,
            "scratchSession": use_scratch_session,
        })),
    );
    let mut turn_options = pm_shared_chat_turn_options_for_message(search_enabled, user_message);
    if use_scratch_session {
        turn_options.memory_mode = ChatMemoryMode::Off;
    }
    let turn_result = if use_scratch_session {
        let mut memory_instructions = Vec::new();
        memory_instructions.extend([
            pm_lightweight_chat_system_instruction(&route),
            pm_shared_chat_tool_loop_system_instruction(&route),
        ]);
        let plan = plan_chat_turn(ChatTurnEngineInput {
            state,
            tenant_id,
            user_id,
            session_id,
            model,
            message: user_message,
            turn_options,
            memory_instructions,
            has_documents: pm_user_message_has_document_context(user_message),
            mark_memory_pollution: search_enabled,
            reasoning_budget_override: Some(reasoning_budget_override),
        })
        .await;
        let trace = serde_json::json!({
            "engine": "shared_chat_turn_engine",
            "searchMode": plan.search_mode.as_str(),
            "reasoningBudget": format!("{:?}", plan.reasoning_budget).to_ascii_lowercase(),
            "trace": plan.trace,
            "scratchSession": true,
        });
        let mut options = plan.options;
        options
            .blocked_tools
            .extend(pm_blocked_non_search_research_tools());
        let (result, partial) = run_pm_shared_chat_turn_on_scratch_session(
            manager.clone(),
            tenant_id,
            user_id,
            session_id,
            model,
            user_message,
            options,
            timeout_secs,
            answer_delta.clone(),
        )
        .await;
        (result.map(|turn| (turn, trace)), partial)
    } else {
        let memory_instructions = memory_instruction.into_iter().collect::<Vec<_>>();
        let mut memory_instructions = memory_instructions;
        memory_instructions.extend([
            pm_lightweight_chat_system_instruction(&route),
            pm_shared_chat_tool_loop_system_instruction(&route),
        ]);
        let plan = plan_chat_turn(ChatTurnEngineInput {
            state,
            tenant_id,
            user_id,
            session_id,
            model,
            message: user_message,
            turn_options,
            memory_instructions,
            has_documents: pm_user_message_has_document_context(user_message),
            mark_memory_pollution: search_enabled,
            reasoning_budget_override: Some(reasoning_budget_override),
        })
        .await;
        let trace = serde_json::json!({
            "engine": "shared_chat_turn_engine",
            "searchMode": plan.search_mode.as_str(),
            "reasoningBudget": format!("{:?}", plan.reasoning_budget).to_ascii_lowercase(),
            "trace": plan.trace,
        });
        let mut options = plan.options;
        options
            .blocked_tools
            .extend(pm_blocked_non_search_research_tools());
        if let Some(answer_delta) = answer_delta.clone() {
            let (result, partial) = run_pm_user_visible_answer_streaming_turn_preserving_partial(
                manager.clone(),
                session_id.to_string(),
                user_message.to_string(),
                timeout_secs,
                "pm shared chat tool loop turn",
                options,
                move |delta| answer_delta("shared_chat", delta),
            )
            .await;
            (result.map(|turn| (turn, trace.clone())), partial)
        } else {
            (
                run_pm_turn_with_timeout_cleanup_and_options(
                    manager.clone(),
                    session_id.to_string(),
                    user_message.to_string(),
                    timeout_secs,
                    "pm shared chat tool loop turn",
                    options,
                )
                .await
                .map(|turn| (turn, trace)),
                String::new(),
            )
        }
    };
    let (turn_result, partial_text) = turn_result;
    let (mut turn, quality, fallback_reason, engine_trace) = match turn_result {
        Ok((turn, trace)) => {
            let mut quality = evaluate_pm_answer_quality(&turn);
            quality.passed = !turn.text.trim().is_empty();
            quality.deliverable = !turn.text.trim().is_empty();
            quality.quality_level = if quality.citation_count > 0 || !turn.tool_calls.is_empty() {
                "high".to_string()
            } else {
                "partial".to_string()
            };
            quality.conflict_reason =
                "PmTurnRouter used shared ChatTurnEngine Codex-like tool loop".to_string();
            (turn, quality, None, Some(trace))
        }
        Err(error) => {
            let reason = error.to_string();
            // Streaming cancellation used to discard all deltas captured before the
            // 300s guard. A partial answer is still materially more useful than a
            // bare runtime error, so deliver it as a clearly marked partial result
            // before trying a second model call.
            if !partial_text.trim().is_empty() {
                let partial_turn =
                    build_pm_preserved_partial_turn(session_id, model, partial_text, &[]);
                let mut quality = evaluate_pm_answer_quality(&partial_turn);
                quality.passed = true;
                quality.deliverable = true;
                quality.quality_level = "partial".to_string();
                quality.conflict_reason =
                    "shared chat tool loop stopped after a timeout; partial output preserved"
                        .to_string();
                (
                    partial_turn,
                    quality,
                    Some(format!("{reason}; partial output preserved")),
                    None,
                )
            } else if let Some(fallback_turn) = run_pm_shared_chat_model_fallback(
                manager.clone(),
                tenant_id,
                user_id,
                session_id,
                model,
                user_message,
                &route,
                &reason,
                answer_delta.clone(),
            )
            .await
            {
                let mut quality = evaluate_pm_answer_quality(&fallback_turn);
                quality.passed = !fallback_turn.text.trim().is_empty();
                quality.deliverable = !fallback_turn.text.trim().is_empty();
                quality.quality_level = "partial".to_string();
                quality.conflict_reason =
                    "shared chat tool loop failed; delivered transient model-authored fallback"
                        .to_string();
                (fallback_turn, quality, Some(reason), None)
            } else {
                let fallback_text =
                    build_pm_direct_answer_timeout_fallback(user_message, &reason, timeout_secs);
                let mut quality = build_pm_direct_answer_quality();
                quality.passed = false;
                quality.deliverable = !fallback_text.trim().is_empty();
                quality.quality_level = "partial".to_string();
                quality.conflict_reason =
                    "shared chat tool loop and model fallback failed; delivered local first-party fallback"
                        .to_string();
                (
                    TurnResult {
                        session_id: session_id.to_string(),
                        text: fallback_text,
                        thinking: None,
                        tool_calls: Vec::new(),
                        usage: TokenUsageRecord {
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            total_tokens: 0,
                            estimated_cost_usd: 0.0,
                            model: model.to_string(),
                        },
                        compacted: None,
                        iterations: 1,
                        metadata: None,
                        hot_reloaded: false,
                    },
                    quality,
                    Some(reason),
                    None,
                )
            }
        }
    };
    turn.text = normalize_shared_chat_visible_language(&turn.text, user_message);
    let search_call_summary = pm_shared_chat_search_call_summary(&turn.tool_calls);
    on_stage(
        "retrieve",
        "completed",
        1,
        Some(serde_json::json!({
            "mode": mode,
            "route": "pm_turn_router_shared_chat_tool_loop",
            "turnRoute": route.to_json(),
            "toolCallCount": turn.tool_calls.len(),
            "citationCount": quality.citation_count,
            "domainCount": quality.domain_count,
            "fallbackReason": fallback_reason,
            "searchCallSummary": search_call_summary,
            "sharedChatTurnEngine": true,
            "chatTurnEngine": engine_trace,
        })),
    );
    on_stage(
        "verify",
        "completed",
        1,
        Some(serde_json::json!({
            "mode": mode,
            "turnRoute": route.to_json(),
            "passed": quality.passed,
            "deliverable": quality.deliverable,
            "qualityGateSkipped": true,
            "citationCount": quality.citation_count,
            "domainCount": quality.domain_count,
            "fallbackReason": fallback_reason,
            "sharedChatTurnEngine": true,
            "chatTurnEngine": engine_trace,
        })),
    );
    on_stage(
        "synthesize",
        "completed",
        1,
        Some(serde_json::json!({
            "mode": mode,
            "turnRoute": route.to_json(),
            "answerLength": turn.text.chars().count(),
            "qualityGatePassed": quality.passed,
            "fallbackReason": fallback_reason,
            "sharedChatTurnEngine": true,
            "chatTurnEngine": engine_trace,
        })),
    );
    finalize_pm_orchestration_result(
        state.telemetry_db(),
        tenant_id,
        run_id,
        session_id,
        turn,
        quality,
    )
    .await
}

async fn build_pm_search_doctor_detail(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> serde_json::Value {
    let snapshot =
        crate::routes::search_orchestrator_runtime::build_unified_search_capability_snapshot(
            state,
            tenant_id,
            Some(model),
            true,
            true,
        )
        .await;
    serde_json::to_value(snapshot.orchestrator)
        .unwrap_or_else(|_| serde_json::json!({ "orchestrator": "PmSearchOrchestrator" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_domain::turn_router::{PmAnswerContract, PmDomainScope, PmFilePolicy, PmReasoningDepth};

    fn route(
        turn_class: PmTurnClass,
        domain_scope: PmDomainScope,
        search_need: PmSearchNeed,
        answer_contract: PmAnswerContract,
        complexity_score: u8,
    ) -> PmTurnRoute {
        let engine = if matches!(answer_contract, PmAnswerContract::PmDecisionPackage)
            || matches!(
                turn_class,
                PmTurnClass::PmStrategy | PmTurnClass::PmReportStrategy
            )
            || matches!(search_need, PmSearchNeed::DeepResearch)
        {
            PmRouteEngine::AosDeepResearch
        } else {
            PmRouteEngine::ChatToolLoop
        };
        let search_policy = match search_need {
            PmSearchNeed::None => PmSearchPolicy::Disabled,
            PmSearchNeed::FreshFact | PmSearchNeed::DeepResearch => PmSearchPolicy::Required,
            PmSearchNeed::EvidenceAugmented => PmSearchPolicy::Allowed,
        };
        PmTurnRoute {
            engine,
            search_policy,
            file_policy: PmFilePolicy::Auto,
            reasoning_depth: if matches!(engine, PmRouteEngine::AosDeepResearch)
                || complexity_score >= 65
            {
                PmReasoningDepth::Deep
            } else {
                PmReasoningDepth::Standard
            },
            turn_class,
            domain_scope,
            search_need,
            answer_contract,
            complexity_score,
            reason: "test".to_string(),
        }
    }

    fn enabled_test_route(id: &str, channel: &str) -> PmEnabledRoute {
        PmEnabledRoute {
            route_id: id.to_string(),
            channel: channel.to_string(),
            execution_channel: "search".to_string(),
        }
    }

    fn probe_candidate(subtask: Option<&str>, variant: &str) -> PmProbeCandidate {
        PmProbeCandidate {
            variant: variant.to_string(),
            route: None,
            subtask_key: subtask.map(str::to_string),
            subtask_id: subtask.map(str::to_string),
            subtask_title: subtask.map(str::to_string),
            subtask_goal: None,
            subtask_deliverable: None,
            subtask_required_evidence_type: Some("external".to_string()),
            subtask_priority: Some("high".to_string()),
        }
    }

    #[test]
    fn first_adaptive_probe_wave_covers_unique_subtasks_before_extra_probes() {
        let candidates = vec![
            probe_candidate(Some("s1"), "s1-q1"),
            probe_candidate(Some("s1"), "s1-q2"),
            probe_candidate(Some("s2"), "s2-q1"),
            probe_candidate(Some("s3"), "s3-q1"),
        ];

        let selected = pm_select_adaptive_probe_wave(candidates, 1, 3, false, 1);
        let keys = selected
            .iter()
            .filter_map(pm_probe_candidate_subtask_key)
            .collect::<Vec<_>>();

        assert_eq!(keys, vec!["s1", "s2", "s3"]);
    }

    #[test]
    fn focused_adaptive_probe_repair_runs_only_one_candidate() {
        let candidates = vec![
            probe_candidate(Some("s1"), "s1-repair-1"),
            probe_candidate(Some("s1"), "s1-repair-2"),
        ];

        let selected = pm_select_adaptive_probe_wave(candidates, 2, 8, true, 1);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].variant, "s1-repair-1");
        assert!(pm_probe_kernel_should_run(true, 2, 2, selected.len()));
        assert!(pm_probe_progress_message(0, selected.len()).contains("定向补齐"));
    }

    #[test]
    fn adaptive_probe_wave_keeps_unscoped_fallback_candidates() {
        let candidates = vec![
            probe_candidate(None, "fallback-1"),
            probe_candidate(None, "fallback-2"),
        ];

        let selected = pm_select_adaptive_probe_wave(candidates, 1, 2, false, 1);

        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn fresh_attempt_picker_skips_repeated_query_route_pair() {
        let query_variants = vec!["rewarded ads case study".to_string()];
        let enabled_routes = vec![
            enabled_test_route("native_model_search", "native"),
            enabled_test_route("web.search.general", "web_search"),
        ];
        let route_usage_counts = HashMap::new();
        let route_blocklist = HashSet::new();
        let mut used = HashSet::new();
        used.insert(
            pm_retrieval_attempt_key(
                Some("rewarded ads case study"),
                Some("native_model_search"),
                Some("native"),
            )
            .expect("attempt key"),
        );

        let picked = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
            &query_variants,
            &enabled_routes,
            1,
            &route_usage_counts,
            &route_blocklist,
            4,
            &used,
        );

        assert_eq!(picked.0.as_deref(), Some("rewarded ads case study"));
        assert_eq!(picked.1.as_deref(), Some("web.search.general"));
        assert!(!picked.4);
    }

    #[test]
    fn fresh_attempt_picker_reports_exhausted_after_all_pairs_used() {
        let query_variants = vec!["rewarded ads case study".to_string()];
        let enabled_routes = vec![enabled_test_route("native_model_search", "native")];
        let route_usage_counts = HashMap::new();
        let route_blocklist = HashSet::new();
        let mut used = HashSet::new();
        used.insert(
            pm_retrieval_attempt_key(
                Some("rewarded ads case study"),
                Some("native_model_search"),
                Some("native"),
            )
            .expect("attempt key"),
        );

        let picked = pm_pick_fresh_attempt_preferences_with_source_quota_and_blocked(
            &query_variants,
            &enabled_routes,
            1,
            &route_usage_counts,
            &route_blocklist,
            4,
            &used,
        );

        assert!(picked.4);
    }

    #[test]
    fn shared_chat_engine_allows_general_live_lookup() {
        let route = route(
            PmTurnClass::LiveLookup,
            PmDomainScope::General,
            PmSearchNeed::FreshFact,
            PmAnswerContract::SourceGroundedAnswer,
            20,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
    }

    #[test]
    fn live_lookup_routes_use_shared_search_chat_not_grounded_pipeline() {
        let route = route(
            PmTurnClass::LiveLookup,
            PmDomainScope::General,
            PmSearchNeed::FreshFact,
            PmAnswerContract::SourceGroundedAnswer,
            20,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Required);
    }

    #[test]
    fn non_deep_live_lookup_skips_pm_research_contract_repairs() {
        let route = route(
            PmTurnClass::LiveLookup,
            PmDomainScope::General,
            PmSearchNeed::FreshFact,
            PmAnswerContract::SourceGroundedAnswer,
            20,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert!(pm_route_can_skip_execution_contracts(
            &route,
            "北京天气怎么样"
        ));
    }

    #[test]
    fn deep_research_keeps_pm_research_contract_repairs() {
        let route = route(
            PmTurnClass::PmStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            82,
        );
        assert!(!pm_route_should_use_shared_chat_engine(&route));
        assert!(!pm_route_can_skip_execution_contracts(
            &route,
            "帮我做市场和竞品策略研究"
        ));
    }

    #[test]
    fn generic_aos_research_cannot_be_misclassified_as_first_party_report_strategy() {
        let question = "深度研究 2026 年企业级 AI Agent 在权限隔离、长期记忆和可恢复执行上的主流做法。至少比较三类方案，指出证据冲突和适用边界，最后给 AOS 开源版一份按优先级排序的实施建议。";
        let mut plan = build_pm_stage_plan(question, &[], &[]);
        let routed = route(
            PmTurnClass::PmReportStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            88,
        );

        let guarded = guard_pm_report_strategy_route(routed, &mut plan, question);

        assert_eq!(guarded.turn_class, PmTurnClass::PmStrategy);
        assert!(guarded
            .reason
            .contains("report_strategy_rejected_without_first_party_metric_evidence"));
        assert!(!pm_is_report_strategy_mode(&plan));
        assert!(plan.get("reportStrategy").is_none());
    }

    #[test]
    fn real_first_party_metric_report_keeps_report_strategy() {
        let question = "我们是 B2B SaaS 自助 onboarding 产品，过去 30 天 trial 用户 18,420，activation 31%，MRR $120k，churn 7.2%，CAC $86。目标是提升 activation、降低 churn、提升 MRR，但 support tickets 不能上升，销售人工介入不能上升。按用户场景分层：solo trial activation 18%，team trial activation 44%，enterprise trial activation 27%。之前试过 mandatory demo wall，activation 下降，self-serve 转化变差。当前已有 email onboarding、in-app checklist、template gallery。我的诉求是基于这份报告给产品运营策略和实验方案。";
        let mut plan = build_pm_stage_plan(question, &[], &[]);
        let routed = route(
            PmTurnClass::PmReportStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            90,
        );

        let guarded = guard_pm_report_strategy_route(routed, &mut plan, question);

        assert_eq!(guarded.turn_class, PmTurnClass::PmReportStrategy);
    }

    #[test]
    fn general_research_routes_use_shared_tool_loop() {
        let route = route(
            PmTurnClass::GeneralResearch,
            PmDomainScope::General,
            PmSearchNeed::EvidenceAugmented,
            PmAnswerContract::GeneralResearchAnswer,
            45,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Allowed);
    }

    #[test]
    fn stable_simple_answer_still_uses_shared_chat_loop_without_search() {
        let route = route(
            PmTurnClass::SimpleAnswer,
            PmDomainScope::General,
            PmSearchNeed::None,
            PmAnswerContract::ShortAnswer,
            12,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
    }

    #[test]
    fn first_party_data_analysis_uses_shared_chat_loop_without_web_retrieval() {
        let route = route(
            PmTurnClass::SimpleAnswer,
            PmDomainScope::Unknown,
            PmSearchNeed::None,
            PmAnswerContract::ShortAnswer,
            40,
        );

        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
    }

    #[test]
    fn document_context_enables_attached_file_workspace_for_direct_chat() {
        let message =
            "分析附件csv\n\n[附件文档上下文]\n### [file:1] data.csv\nA,B\n1,2\n[/附件文档上下文]";
        let options = pm_shared_chat_turn_options_for_message(false, message);
        let file_context = options.file_context.expect("file context");
        assert_eq!(file_context.mode, ChatFileContextMode::AllAttached);
        assert!(!file_context.strict_grounding);
    }

    #[test]
    fn simple_answer_with_internal_exec_constraints_still_uses_shared_chat_loop() {
        let route = route(
            PmTurnClass::SimpleAnswer,
            PmDomainScope::General,
            PmSearchNeed::None,
            PmAnswerContract::ShortAnswer,
            12,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
    }

    #[test]
    fn csv_document_analysis_uses_shared_chat_without_web_retrieval() {
        let route = route(
            PmTurnClass::SimpleAnswer,
            PmDomainScope::ProductOps,
            PmSearchNeed::None,
            PmAnswerContract::ShortAnswer,
            40,
        );
        let message = "附件csv的数据给我分析下哪个组好\n\n[附件文档上下文]\n### [file:1] data.csv\ncol_a,col_b\n1,2\n[/附件文档上下文]";

        assert!(pm_user_message_has_document_context(message));
        assert!(pm_route_should_use_shared_chat_engine(&route));
        assert_eq!(route.search_policy, PmSearchPolicy::Disabled);
    }

    #[test]
    fn direct_answer_timeout_floor_matches_long_form_generation() {
        assert!(pm_direct_answer_turn_timeout_secs() >= PM_FORCE_SYNTH_TURN_TIMEOUT_DEFAULT_SECS);
    }

    #[test]
    fn shared_chat_soft_fallback_timeout_not_clamped_below_direct_answer_budget() {
        let timeout_secs = pm_direct_answer_turn_timeout_secs();
        assert!(timeout_secs >= PM_FORCE_SYNTH_TURN_TIMEOUT_DEFAULT_SECS);
    }

    #[test]
    fn shared_chat_engine_blocks_pm_strategy_and_report_routes() {
        let strategy = route(
            PmTurnClass::PmStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            80,
        );
        let report = route(
            PmTurnClass::PmReportStrategy,
            PmDomainScope::ProductOps,
            PmSearchNeed::DeepResearch,
            PmAnswerContract::PmDecisionPackage,
            90,
        );
        assert!(!pm_route_should_use_shared_chat_engine(&strategy));
        assert!(!pm_route_should_use_shared_chat_engine(&report));
    }

    #[test]
    fn deep_preface_respects_the_configured_timeout() {
        let plan = serde_json::json!({
            "reportStrategyHint": {
                "advisory": true,
                "matched": false,
                "score": 3,
                "reasons": ["strategy_request", "specific_business_context"]
            }
        });
        assert_eq!(
            pm_effective_preface_turn_timeout_secs(&plan),
            pm_preface_turn_timeout_secs()
        );
    }

    #[test]
    fn deterministic_exec_constraints_keep_only_enabled_routes_and_runtime_budgets() {
        let plan = serde_json::json!({
            "sourceRoutes": [
                {"routeId": "native", "enabled": true},
                {"routeId": "browser", "enabled": false},
                {"routeId": "configured", "enabled": true}
            ]
        });
        let budget = PmTimeoutBudget {
            pipeline_timeout_secs: 600,
            max_attempts: 3,
            retrieve_max_tool_calls: 8,
            max_calls_per_source: 2,
            source_slot_search_secs: 90,
            source_slot_browser_secs: 120,
            source_slot_api_fetch_secs: 60,
            preflight_model_timeout_secs: 30,
            preflight_probe_timeout_secs: 10,
            preflight_overall_timeout_secs: 30,
            retry_step_budget_secs: 45,
            retry_total_budget_secs: 120,
        };

        let constraints = build_pm_deterministic_exec_constraints(&plan, &budget);

        assert_eq!(
            constraints.route_allowlist,
            vec!["configured".to_string(), "native".to_string()]
        );
        assert_eq!(constraints.route_priority, constraints.route_allowlist);
        assert_eq!(constraints.source_slot_budget_secs, 90);
        assert_eq!(constraints.tool_budget_per_attempt, 8);
        assert_eq!(constraints.pipeline_timeout_secs, 600);
    }

    #[test]
    fn shared_chat_engine_allows_product_ops_general_research_when_not_deep() {
        let route = route(
            PmTurnClass::GeneralResearch,
            PmDomainScope::ProductOps,
            PmSearchNeed::EvidenceAugmented,
            PmAnswerContract::GeneralResearchAnswer,
            55,
        );
        assert!(pm_route_should_use_shared_chat_engine(&route));
    }

    #[test]
    fn shared_chat_search_call_summary_counts_layers_from_tool_records() {
        let tool_calls = vec![
            agent_gateway::ToolCallRecord {
                index: 0,
                tool_name: "WebSearch".to_string(),
                source: "builtin".to_string(),
                source_name: "native_model_search".to_string(),
                input: "{}".to_string(),
                output: "{}".to_string(),
                is_error: false,
                duration_ms: 1,
            },
            agent_gateway::ToolCallRecord {
                index: 1,
                tool_name: "mcp__search__query".to_string(),
                source: "mcp".to_string(),
                source_name: "search".to_string(),
                input: "{}".to_string(),
                output: "{}".to_string(),
                is_error: false,
                duration_ms: 1,
            },
        ];
        let summary = pm_shared_chat_search_call_summary(&tool_calls);
        assert_eq!(summary.get("native").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(summary.get("mcp").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn coverage_repair_blocks_early_finalize_when_fresh_target_is_available() {
        assert!(pm_coverage_repair_is_actionable(
            true, true, true, true, 1, 3
        ));
    }

    #[test]
    fn coverage_repair_does_not_force_useless_attempts_after_exhaustion() {
        assert!(!pm_coverage_repair_is_actionable(
            true, true, true, false, 1, 3
        ));
        assert!(!pm_coverage_repair_is_actionable(
            true, true, true, true, 3, 3
        ));
        assert!(!pm_coverage_repair_is_actionable(
            true, true, false, true, 1, 3
        ));
    }

    #[test]
    fn deep_research_convergence_overrides_an_actionable_coverage_repair() {
        assert!(pm_deep_loop_should_finish_attempt(true, true, true));
        assert!(!pm_deep_loop_should_finish_attempt(true, true, false));
        assert!(pm_deep_loop_should_finish_attempt(true, false, false));
    }

    #[test]
    fn unavailable_probe_chain_converges_without_guessing_fetch_urls() {
        let unavailable = PmProbeOutcome {
            variant: "market evidence".to_string(),
            route_id: Some("web.search.general".to_string()),
            route_channel: Some("search".to_string()),
            subtask_key: None,
            subtask_id: None,
            subtask_title: None,
            subtask_goal: None,
            subtask_deliverable: None,
            subtask_required_evidence_type: None,
            subtask_priority: None,
            elapsed_ms: Some(1),
            turn: None,
            diagnostic_turn: None,
            quality: None,
            error: Some(
                "unified search returned no sufficient source-backed evidence; used_layer=none; native_attempts=0; mcp_attempts=0; configured_provider_attempts=0; rag_local_attempts=0"
                    .to_string(),
            ),
        };
        assert!(pm_probe_outcomes_confirm_no_retrieval_route(&[
            unavailable.clone()
        ]));
        assert!(pm_probe_outcomes_confirm_retrieval_discovery_exhausted(&[
            unavailable.clone()
        ]));

        let mut attempted = unavailable.clone();
        attempted.error = Some(
            "used_layer=none; native_attempts=1; mcp_attempts=0; configured_provider_attempts=0; rag_local_attempts=0"
                .to_string(),
        );
        assert!(!pm_probe_outcomes_confirm_no_retrieval_route(&[
            attempted.clone()
        ]));
        assert!(pm_probe_outcomes_confirm_retrieval_discovery_exhausted(&[
            attempted.clone()
        ]));

        let mut local_attempted = unavailable.clone();
        local_attempted.error = Some(
            "used_layer=none; native_attempts=0; mcp_attempts=0; configured_provider_attempts=0; rag_local_attempts=1"
                .to_string(),
        );
        assert!(!pm_probe_outcomes_confirm_no_retrieval_route(&[
            local_attempted.clone()
        ]));
        assert!(pm_probe_outcomes_confirm_retrieval_discovery_exhausted(&[
            local_attempted
        ]));

        attempted.turn = Some(TurnResult {
            session_id: "session".to_string(),
            text: "source-backed result".to_string(),
            thinking: None,
            tool_calls: Vec::new(),
            usage: TokenUsageRecord {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                model: "test-model".to_string(),
            },
            compacted: None,
            iterations: 1,
            metadata: None,
            hot_reloaded: false,
        });
        assert!(!pm_probe_outcomes_confirm_retrieval_discovery_exhausted(&[
            attempted
        ]));

        let mut unrelated_failure = unavailable;
        unrelated_failure.error = Some("provider authentication failed".to_string());
        assert!(!pm_probe_outcomes_confirm_retrieval_discovery_exhausted(&[
            unrelated_failure
        ]));
    }

    #[test]
    fn planned_query_variants_are_deduplicated_and_capped() {
        let plan = serde_json::json!({
            "queryVariants": [
                "alpha", " beta ", "alpha", "", "gamma", "delta", "epsilon", "zeta",
                "eta", "theta", "iota", "kappa", "lambda"
            ]
        });

        let variants = normalized_pm_plan_query_variants(&plan, "fallback query");

        assert!(variants.len() <= 5);
        assert_eq!(
            variants
                .iter()
                .filter(|item| item.as_str() == "alpha")
                .count(),
            1
        );
        assert!(variants.iter().all(|item| !item.trim().is_empty()));
    }
}
