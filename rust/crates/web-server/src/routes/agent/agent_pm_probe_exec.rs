use super::*;

pub(super) fn should_fast_fail_after_tool_errors(turn: &TurnResult) -> bool {
    if turn.tool_calls.is_empty() {
        return false;
    }
    let error_calls: Vec<&agent_gateway::ToolCallRecord> =
        turn.tool_calls.iter().filter(|tc| tc.is_error).collect();
    if error_calls.is_empty() {
        return false;
    }
    let all_network_failures = error_calls.iter().all(|tc| {
        let out = tc.output.to_ascii_lowercase();
        out.contains("error sending request")
            || out.contains("dns")
            || out.contains("timed out")
            || out.contains("timeout")
            || out.contains("connection refused")
            || out.contains("connection reset")
            || out.contains("could not resolve host")
    });
    all_network_failures && error_calls.len() == turn.tool_calls.len()
}

fn pm_tool_name_variants(raw: &str) -> Vec<String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return Vec::new();
    }
    let mut out = vec![lower.clone()];
    let short = lower
        .rsplit([':', '/', '.', '#'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    if !short.is_empty() && short != lower {
        out.push(short.to_string());
    }
    out
}

fn is_pm_disallowed_research_tool_name(tool_name: &str) -> bool {
    pm_tool_name_variants(tool_name).into_iter().any(|name| {
        matches!(
            name.as_str(),
            "toolsearch"
                | "listmcpresources"
                | "listmcpresourcetemplates"
                | "readmcpresource"
                | "list_mcp_resources"
                | "list_mcp_resource_templates"
                | "read_mcp_resource"
        ) || matches!(
            name.as_str(),
            "playwright"
                | "puppeteer"
                | "crawl4ai"
                | "crawl4aitool"
                | "bash"
                | "shell"
                | "sh"
                | "python"
                | "python3"
                | "read_file"
                | "write_file"
                | "edit_file"
                | "glob_search"
                | "grep_search"
        )
    })
}

pub(super) fn pm_blocked_non_search_research_tools() -> Vec<String> {
    [
        "bash",
        "shell",
        "sh",
        "python",
        "python3",
        "read_file",
        "write_file",
        "edit_file",
        "glob_search",
        "grep_search",
        "playwright",
        "puppeteer",
        "crawl4ai",
        "crawl4aitool",
    ]
    .iter()
    .map(|tool| (*tool).to_string())
    .collect()
}

pub(super) fn collect_pm_disallowed_research_tools(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> Vec<String> {
    let mut out = std::collections::BTreeSet::<String>::new();
    for tc in tool_calls {
        if is_pm_disallowed_research_tool_name(&tc.tool_name) {
            out.insert(tc.tool_name.clone());
        }
    }
    out.into_iter().collect()
}

pub(super) fn build_pm_probe_repair_context(outcomes: &[PmProbeOutcome]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for outcome in outcomes {
        let subtask_brief = outcome
            .subtask_title
            .as_deref()
            .or(outcome.subtask_id.as_deref())
            .unwrap_or("-");
        let route_brief = outcome
            .route_id
            .as_deref()
            .or(outcome.route_channel.as_deref())
            .unwrap_or("auto_route");
        if let Some(quality) = &outcome.quality {
            let citations = quality
                .citations
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>();
            lines.push(format!(
                "- subtask: {} | variant: {} | route: {} | tools={} citations={} domains={} alignment={} | sample_urls={}",
                subtask_brief,
                outcome.variant,
                route_brief,
                quality.tool_call_count,
                quality.citation_count,
                quality.domain_count,
                quality.claim_alignment_ok,
                citations.join(", ")
            ));
        } else if let Some(err) = &outcome.error {
            lines.push(format!(
                "- subtask: {} | variant: {} | route: {} | error={}",
                subtask_brief, outcome.variant, route_brief, err
            ));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("Probe outcomes:\n{}", lines.join("\n"))
    }
}

fn pm_tool_call_dedupe_key(tc: &agent_gateway::ToolCallRecord) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        tc.tool_name,
        tc.source,
        tc.source_name,
        tc.is_error,
        tc.input,
        tc.output.chars().take(260).collect::<String>()
    )
}

pub(super) fn merge_pm_tool_calls_unique(
    base: &[agent_gateway::ToolCallRecord],
    extra: &[agent_gateway::ToolCallRecord],
) -> Vec<agent_gateway::ToolCallRecord> {
    if extra.is_empty() {
        return base.to_vec();
    }
    let mut merged = base.to_vec();
    let mut seen = HashSet::<String>::new();
    for tc in &merged {
        seen.insert(pm_tool_call_dedupe_key(tc));
    }
    for tc in extra {
        if seen.insert(pm_tool_call_dedupe_key(tc)) {
            merged.push(tc.clone());
        }
    }
    merged
}

pub(super) fn merge_pm_turn_with_observed_tool_calls(
    mut turn: TurnResult,
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> TurnResult {
    if observed_tool_calls.is_empty() {
        return turn;
    }
    turn.tool_calls = merge_pm_tool_calls_unique(&turn.tool_calls, observed_tool_calls);
    turn
}

pub(super) fn build_pm_observed_tool_context(
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> String {
    if observed_tool_calls.is_empty() {
        return String::new();
    }
    let success_count = observed_tool_calls.iter().filter(|tc| !tc.is_error).count();
    let error_count = observed_tool_calls.len().saturating_sub(success_count);
    let mut lines = Vec::new();
    lines.push(format!(
        "Observed tool execution before failure: total={} success={} error={}",
        observed_tool_calls.len(),
        success_count,
        error_count
    ));
    let evidence_hits = build_pm_tool_evidence_hits(observed_tool_calls);
    let sample_urls = evidence_hits
        .iter()
        .take(8)
        .map(|hit| hit.url.clone())
        .collect::<Vec<_>>();
    if !sample_urls.is_empty() {
        lines.push(format!("Observed URLs: {}", sample_urls.join(", ")));
    }
    for tc in observed_tool_calls.iter().take(8) {
        let excerpt = first_non_empty_line(&tc.output);
        let excerpt = if excerpt.is_empty() {
            truncate_for_log(&tc.output, 180)
        } else {
            truncate_for_log(&excerpt, 180)
        };
        lines.push(format!(
            "- tool={} source={}:{} status={} excerpt={}",
            tc.tool_name,
            tc.source,
            tc.source_name,
            if tc.is_error { "error" } else { "ok" },
            excerpt
        ));
    }
    lines.join("\n")
}

pub(super) fn merge_pm_probe_turns(
    probe_outcomes: &[PmProbeOutcome],
    fallback: Option<&TurnResult>,
) -> Option<TurnResult> {
    let mut success_outcomes: Vec<&PmProbeOutcome> = probe_outcomes
        .iter()
        .filter(|outcome| outcome.turn.is_some())
        .collect();
    if success_outcomes.is_empty() {
        return fallback.cloned();
    }
    success_outcomes.sort_by(|a, b| {
        let score_a = a
            .quality
            .as_ref()
            .map(score_pm_probe_quality)
            .unwrap_or(i64::MIN);
        let score_b = b
            .quality
            .as_ref()
            .map(score_pm_probe_quality)
            .unwrap_or(i64::MIN);
        score_b.cmp(&score_a)
    });
    let mut selected_outcomes: Vec<&PmProbeOutcome> = Vec::new();
    let mut selected_subtasks = HashSet::<String>::new();
    for outcome in &success_outcomes {
        let Some(subtask_key) = outcome
            .subtask_key
            .as_deref()
            .or(outcome.subtask_id.as_deref())
            .or(outcome.subtask_title.as_deref())
            .map(|raw| raw.trim().to_ascii_lowercase())
        else {
            continue;
        };
        if subtask_key.is_empty() || !selected_subtasks.insert(subtask_key) {
            continue;
        }
        selected_outcomes.push(*outcome);
        if selected_outcomes.len() >= 6 {
            break;
        }
    }
    if selected_outcomes.len() < 3 {
        for outcome in &success_outcomes {
            if selected_outcomes
                .iter()
                .any(|existing| std::ptr::eq(*existing, *outcome))
            {
                continue;
            }
            selected_outcomes.push(*outcome);
            if selected_outcomes.len() >= 6 {
                break;
            }
        }
    }
    if selected_outcomes.is_empty() {
        selected_outcomes = success_outcomes.iter().take(3).copied().collect();
    }

    let top = selected_outcomes
        .first()
        .and_then(|outcome| outcome.turn.as_ref())?;
    let mut merged_tool_calls = top.tool_calls.clone();
    let mut merged_text_sections = Vec::<String>::new();
    let mut seen_text_blocks = HashSet::<String>::new();

    for outcome in selected_outcomes {
        if let Some(turn) = &outcome.turn {
            let trimmed = turn.text.trim();
            if !trimmed.is_empty() {
                let key = trimmed
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if seen_text_blocks.insert(key) {
                    let route = outcome
                        .route_id
                        .as_deref()
                        .or(outcome.route_channel.as_deref())
                        .unwrap_or("auto_route");
                    let subtask = outcome
                        .subtask_title
                        .as_deref()
                        .or(outcome.subtask_id.as_deref())
                        .unwrap_or("unscoped");
                    let variant = outcome.variant.trim();
                    merged_text_sections.push(format!(
                        "### Probe Source [{route}] / Subtask [{subtask}] / Variant\n{}\n\n{}",
                        truncate_for_log(variant, 180),
                        trimmed
                    ));
                }
            }
            merged_tool_calls = merge_pm_tool_calls_unique(&merged_tool_calls, &turn.tool_calls);
        }
    }

    let mut merged = top.clone();
    if !merged_text_sections.is_empty() {
        merged.text = format!(
            "{}\n\n{}",
            top.text.trim(),
            merged_text_sections.join("\n\n")
        );
    }
    merged.tool_calls = merged_tool_calls;
    Some(merged)
}

pub(super) async fn run_pm_probe_turn(
    state: AppState,
    _manager: Arc<AgentSessionManager>,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    candidate: PmProbeCandidate,
    original_question: &str,
    native_runtime: Option<crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime>,
    prepared_context: Option<
        Arc<crate::routes::search_orchestrator_runtime::UnifiedSearchPreparedContext>,
    >,
) -> PmProbeOutcome {
    let variant = candidate.variant;
    let subtask_key = candidate.subtask_key.clone();
    let subtask_id = candidate.subtask_id.clone();
    let subtask_title = candidate.subtask_title.clone();
    let subtask_goal = candidate.subtask_goal.clone();
    let route_id = candidate
        .route
        .as_ref()
        .and_then(|value| value.get("routeId"))
        .and_then(|value| value.as_str())
        .map(std::string::ToString::to_string);
    let route_channel = candidate
        .route
        .as_ref()
        .and_then(|value| value.get("channel"))
        .and_then(|value| value.as_str())
        .map(std::string::ToString::to_string);

    let unified_started = Instant::now();
    match run_pm_unified_search_probe(
        &state,
        user_id,
        tenant_id,
        model,
        &variant,
        original_question,
        subtask_title.as_deref(),
        subtask_goal.as_deref(),
        native_runtime,
        prepared_context,
    )
    .await
    {
        Ok(turn) => {
            let quality = evaluate_pm_answer_quality(&turn);
            PmProbeOutcome {
                variant,
                route_id,
                route_channel,
                subtask_key,
                subtask_id,
                subtask_title,
                subtask_goal,
                subtask_deliverable: candidate.subtask_deliverable.clone(),
                subtask_required_evidence_type: candidate.subtask_required_evidence_type.clone(),
                subtask_priority: candidate.subtask_priority.clone(),
                elapsed_ms: Some(
                    u64::try_from(unified_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
                turn: Some(turn),
                diagnostic_turn: None,
                quality: Some(quality),
                error: None,
            }
        }
        Err((error, diagnostic_turn)) => PmProbeOutcome {
            variant,
            route_id,
            route_channel,
            subtask_key,
            subtask_id,
            subtask_title,
            subtask_goal,
            subtask_deliverable: candidate.subtask_deliverable.clone(),
            subtask_required_evidence_type: candidate.subtask_required_evidence_type.clone(),
            subtask_priority: candidate.subtask_priority.clone(),
            elapsed_ms: Some(
                u64::try_from(unified_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            ),
            turn: None,
            diagnostic_turn,
            quality: None,
            error: Some(error),
        },
    }
}

async fn run_pm_unified_search_probe(
    state: &AppState,
    user_id: &str,
    tenant_id: &str,
    model: &str,
    query_variant: &str,
    original_question: &str,
    subtask_title: Option<&str>,
    subtask_goal: Option<&str>,
    native_runtime: Option<crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime>,
    prepared_context: Option<
        Arc<crate::routes::search_orchestrator_runtime::UnifiedSearchPreparedContext>,
    >,
) -> Result<TurnResult, (String, Option<TurnResult>)> {
    let query = query_variant.trim();
    if query.is_empty() {
        return Err((
            "unified search skipped: empty query variant".to_string(),
            None,
        ));
    }
    let scenario = if pm_domain::report_strategy::detect_pm_report_strategy_signal(
        original_question,
    )
    .matched
    {
        "pm_report_strategy_probe_evidence"
    } else {
        "pm_deep_research_probe_evidence"
    };
    let result = crate::routes::search_orchestrator_runtime::execute_unified_search(
        state,
        crate::routes::search_orchestrator_runtime::UnifiedSearchRequest {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            scenario: scenario.to_string(),
            query: query.to_string(),
            first_party_available: !original_question.trim().is_empty(),
            native_runtime,
            max_results: 5,
            rag_local_available: true,
            prepared_context,
        },
    )
    .await;

    let trace_counts = pm_unified_search_trace_counts(&result);
    tracing::info!(
        tenant_id = %tenant_id,
        user_id = %user_id,
        model = %model,
        scenario = %result.scenario,
        query = %crate::routes::search_orchestrator_runtime::normalize_unified_search_query(query),
        used_layer = result.used_layer.as_deref().unwrap_or("none"),
        available = result.available,
        native_attempts = trace_counts.native,
        mcp_attempts = trace_counts.mcp,
        configured_provider_attempts = trace_counts.configured_provider,
        rag_local_attempts = trace_counts.rag_local,
        degraded_reason = result.degraded_reason.as_deref().unwrap_or(""),
        "pm probe unified search completed"
    );

    let diagnostic_turn =
        build_pm_unified_search_diagnostic_turn(&result, query, subtask_title, subtask_goal, model);

    if !result.available || result.items.is_empty() {
        return Err((
            format!(
            "unified search returned no sufficient source-backed evidence; used_layer={}; native_attempts={}; mcp_attempts={}; configured_provider_attempts={}; rag_local_attempts={}; reason={}",
            result.used_layer.as_deref().unwrap_or("none"),
            trace_counts.native,
            trace_counts.mcp,
            trace_counts.configured_provider,
            trace_counts.rag_local,
            result
                .degraded_reason
                .as_deref()
                .unwrap_or("external search unavailable or insufficient")
            ),
            Some(diagnostic_turn),
        ));
    }

    let text = build_pm_unified_search_probe_text(&result, query, subtask_title, subtask_goal);
    let tool_call = build_pm_unified_search_tool_call(&result);
    Ok(TurnResult {
        session_id: format!("pm-unified-search-{}", uuid::Uuid::new_v4()),
        text,
        thinking: None,
        tool_calls: vec![tool_call],
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
        hot_reloaded: result.hot_reload_supported,
    })
}

pub(crate) async fn resolve_pm_native_search_runtime(
    state: &AppState,
    tenant_id: &str,
    model: &str,
) -> Option<crate::routes::search_orchestrator_runtime::UnifiedNativeSearchRuntime> {
    crate::routes::search_orchestrator_runtime::resolve_unified_native_search_runtime(
        state, tenant_id, model,
    )
    .await
}

#[derive(Default)]
pub(super) struct PmUnifiedSearchTraceCounts {
    pub(super) native: usize,
    pub(super) mcp: usize,
    pub(super) configured_provider: usize,
    pub(super) rag_local: usize,
}

pub(super) fn pm_unified_search_trace_counts(
    result: &crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
) -> PmUnifiedSearchTraceCounts {
    let mut counts = PmUnifiedSearchTraceCounts::default();
    for trace in &result.traces {
        match trace.layer.as_str() {
            "native_model_search" => counts.native += 1,
            "mcp_search" => counts.mcp += 1,
            "configured_search_provider" => counts.configured_provider += 1,
            "rag_local" => counts.rag_local += 1,
            _ => {}
        }
    }
    counts
}

fn build_pm_unified_search_probe_text(
    result: &crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
    query: &str,
    subtask_title: Option<&str>,
    subtask_goal: Option<&str>,
) -> String {
    let mut lines = Vec::<String>::new();
    lines.push("Unified Search Probe Evidence".to_string());
    lines.push(format!(
        "Used layer: {}",
        result.used_layer.as_deref().unwrap_or("degraded")
    ));
    lines.push(format!("Query: {}", query.trim()));
    if let Some(title) = subtask_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Subtask: {title}"));
    }
    if let Some(goal) = subtask_goal
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Subtask goal: {goal}"));
    }
    lines.push(String::new());
    lines.push("Evidence facts:".to_string());
    for item in result.items.iter().take(8) {
        let title = item.title.trim();
        let excerpt = item
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Source-backed result returned no excerpt.");
        if let Some(url) = item.url.as_deref().filter(|value| !value.trim().is_empty()) {
            lines.push(format!("- {title}: {excerpt} ({url})"));
        } else {
            lines.push(format!("- {title}: {excerpt}"));
        }
    }
    if let Some(reason) = result
        .degraded_reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(String::new());
        lines.push(format!("Degraded note: {reason}"));
    }
    lines.join("\n")
}

fn build_pm_unified_search_diagnostic_text(
    result: &crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
    query: &str,
    subtask_title: Option<&str>,
    subtask_goal: Option<&str>,
) -> String {
    let mut lines = Vec::<String>::new();
    lines.push("Unified Search Diagnostic Notes (non-citable)".to_string());
    lines.push(format!(
        "Used layer: {}",
        result.used_layer.as_deref().unwrap_or("degraded")
    ));
    lines.push(format!("Query: {}", query.trim()));
    if let Some(title) = subtask_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Subtask: {title}"));
    }
    if let Some(goal) = subtask_goal
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Subtask goal: {goal}"));
    }
    lines.push(
        "These notes are not source-backed citations. Use them only as weak research hints; do not cite them as external sources."
            .to_string(),
    );
    if let Some(reason) = result
        .degraded_reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Why not citable: {reason}"));
    }
    if !result.items.is_empty() {
        lines.push(String::new());
        lines.push("Non-citable snippets:".to_string());
    }
    for item in result.items.iter().take(8) {
        let title = item.title.trim();
        let excerpt = item
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("No excerpt.");
        let source = item
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("no URL");
        lines.push(format!(
            "- {title}: {} [source: {source}; relevance={:.2}]",
            truncate_for_log(excerpt, 500),
            item.relevance_score.unwrap_or_default()
        ));
    }
    lines.join("\n")
}

fn build_pm_unified_search_diagnostic_turn(
    result: &crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
    query: &str,
    subtask_title: Option<&str>,
    subtask_goal: Option<&str>,
    model: &str,
) -> TurnResult {
    let text = build_pm_unified_search_diagnostic_text(result, query, subtask_title, subtask_goal);
    let tool_call = build_pm_unified_search_tool_call(result);
    TurnResult {
        session_id: format!("pm-unified-search-diagnostic-{}", uuid::Uuid::new_v4()),
        text,
        thinking: None,
        tool_calls: vec![tool_call],
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
        hot_reloaded: result.hot_reload_supported,
    }
}

pub(super) fn build_pm_unified_search_tool_call(
    result: &crate::routes::search_orchestrator_runtime::UnifiedSearchResult,
) -> agent_gateway::ToolCallRecord {
    let content = result
        .items
        .iter()
        .take(10)
        .map(|item| {
            let excerpt = item.excerpt.clone().unwrap_or_default();
            serde_json::json!({
                "title": item.title,
                "url": item.url,
                "snippet": excerpt,
                "content": excerpt,
                "contentChars": item.excerpt.as_deref().map(|value| value.chars().count()).unwrap_or(0),
                "sourceType": item.source_type,
                "sourceName": item.source_name,
                "relevanceScore": item.relevance_score,
                "confidence": item.confidence,
            })
        })
        .collect::<Vec<_>>();
    let provider_trace = result
        .traces
        .iter()
        .map(|trace| {
            format!(
                "{}: status={} results={} reason={}",
                trace.layer,
                trace.status,
                trace.result_count,
                trace.fallback_reason.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    let output = serde_json::json!({
        "query": result.query,
        "results": [{
            "tool_use_id": "pm_unified_search",
            "content": content,
        }],
        "providerTrace": provider_trace,
        "orchestratorTrace": crate::routes::search_orchestrator_runtime::unified_search_result_to_trace(result),
    });
    agent_gateway::ToolCallRecord {
        index: 0,
        tool_name: "WebSearch".to_string(),
        source: "builtin".to_string(),
        source_name: result
            .used_layer
            .as_deref()
            .unwrap_or("unified_search_orchestrator")
            .to_string(),
        input: serde_json::json!({
            "query": result.query,
            "scenario": result.scenario,
            "orchestrator": "unified_search",
        })
        .to_string(),
        output: serde_json::to_string(&output).unwrap_or_else(|_| "{}".to_string()),
        is_error: false,
        duration_ms: result
            .traces
            .iter()
            .map(|trace| trace.latency_ms)
            .sum::<u128>()
            .try_into()
            .unwrap_or(u64::MAX),
    }
}
