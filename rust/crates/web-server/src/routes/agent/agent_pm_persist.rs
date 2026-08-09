use super::*;

fn pm_relation_counters(relation: &str) -> (i64, i64, i64) {
    match relation {
        "supports" => (1, 0, 0),
        "contradicts" => (0, 1, 0),
        _ => (0, 0, 1),
    }
}

struct PmEvidenceGraphEdge {
    tenant_id: String,
    session_id: String,
    claim_key: String,
    claim_text: String,
    url_hash: String,
    url: String,
    domain: Option<String>,
    relation: String,
    source_tool: Option<String>,
    source_route: Option<String>,
    evidence_excerpt: Option<String>,
    confidence: f64,
}

async fn upsert_pm_evidence_graph_edges(
    db: &sqlx::SqlitePool,
    rows: &[PmEvidenceGraphEdge],
) -> Result<(), sqlx::Error> {
    for chunk in rows.chunks(100) {
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "INSERT INTO pm_research_evidence_graph
                (tenant_id, session_id, claim_key, claim_text, url_hash, url, domain, relation,
                 source_tool, source_route, evidence_excerpt, run_count, support_count,
                 contradict_count, unresolved_count, avg_confidence, last_seen_at) ",
        );
        query.push_values(chunk, |mut values, row| {
            let (support, contradict, unresolved) = pm_relation_counters(&row.relation);
            values
                .push_bind(&row.tenant_id)
                .push_bind(&row.session_id)
                .push_bind(&row.claim_key)
                .push_bind(&row.claim_text)
                .push_bind(&row.url_hash)
                .push_bind(&row.url)
                .push_bind(row.domain.as_deref())
                .push_bind(&row.relation)
                .push_bind(row.source_tool.as_deref())
                .push_bind(row.source_route.as_deref())
                .push_bind(row.evidence_excerpt.as_deref())
                .push_bind(1i64)
                .push_bind(support)
                .push_bind(contradict)
                .push_bind(unresolved)
                .push_bind(row.confidence.clamp(0.0, 1.0))
                .push("CURRENT_TIMESTAMP");
        });
        query.push(
            " ON CONFLICT DO UPDATE SET session_id = excluded.session_id,
                claim_text = excluded.claim_text, domain = COALESCE(excluded.domain, domain),
                source_tool = COALESCE(excluded.source_tool, source_tool),
                source_route = COALESCE(excluded.source_route, source_route),
                evidence_excerpt = COALESCE(excluded.evidence_excerpt, evidence_excerpt),
                run_count = run_count + 1,
                support_count = support_count + excluded.support_count,
                contradict_count = contradict_count + excluded.contradict_count,
                unresolved_count = unresolved_count + excluded.unresolved_count,
                avg_confidence = avg_confidence * 0.8 + excluded.avg_confidence * 0.2,
                last_seen_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
        );
        query.build().execute(db).await?;
    }
    Ok(())
}

pub(super) async fn persist_pm_evidence_graph(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    session_id: &str,
    turn: &TurnResult,
    quality: &PmAnswerQualityDto,
) -> Result<(), sqlx::Error> {
    let tool_hits = build_pm_tool_evidence_hits(&turn.tool_calls);
    let mut url_hit_map = std::collections::HashMap::<String, PmToolEvidenceHit>::new();
    for hit in tool_hits {
        url_hit_map.entry(hit.url.clone()).or_insert(hit);
    }
    let mut rows = Vec::new();

    for node in &quality.evidence_tree {
        let normalized = normalize_claim_key(&node.claim);
        if normalized.is_empty() {
            continue;
        }
        let claim_key = sha256_hex(&normalized);
        let relation = if node.status == "confirmed" {
            "supports"
        } else {
            "unresolved"
        };
        let base_conf = if relation == "supports" { 0.78 } else { 0.42 };
        for leaf in &node.evidences {
            if leaf.url.trim().is_empty() {
                continue;
            }
            let hit = url_hit_map.get(&leaf.url);
            let source_tool = hit.map(|h| h.source_tool.clone());
            let source_route = hit.map(|h| h.source_route.clone());
            let evidence_excerpt = if leaf.excerpt.trim().is_empty() {
                hit.map(|h| h.excerpt.clone())
            } else {
                Some(leaf.excerpt.clone())
            };
            rows.push(PmEvidenceGraphEdge {
                tenant_id: tenant_id.to_string(),
                session_id: session_id.to_string(),
                claim_key: claim_key.clone(),
                claim_text: node.claim.clone(),
                url_hash: sha256_hex(leaf.url.trim().to_ascii_lowercase().as_str()),
                url: leaf.url.clone(),
                domain: if leaf.domain.trim().is_empty() {
                    None
                } else {
                    Some(leaf.domain.clone())
                },
                relation: relation.to_string(),
                source_tool,
                source_route,
                evidence_excerpt,
                confidence: base_conf,
            });
        }
    }

    for edge in &quality.conflict_graph.edges {
        let topic = edge.topic.trim();
        if topic.is_empty() {
            continue;
        }
        let claim_text = format!(
            "Conflict topic: {}; verdict: {}; {} vs {}",
            topic, edge.verdict, edge.source_left, edge.source_right
        );
        let claim_key = sha256_hex(&format!("conflict::{}", normalize_claim_key(&claim_text)));
        let relation = if edge.relation == "contradicts" {
            "contradicts"
        } else if edge.relation == "corroborates" {
            "supports"
        } else {
            "unresolved"
        };
        for url in &edge.urls {
            if url.trim().is_empty() {
                continue;
            }
            let hit = url_hit_map.get(url);
            let source_tool = hit.map(|h| h.source_tool.clone());
            let source_route = hit.map(|h| h.source_route.clone());
            let excerpt = hit
                .map(|h| h.excerpt.clone())
                .or(Some(edge.verdict.clone()));
            rows.push(PmEvidenceGraphEdge {
                tenant_id: tenant_id.to_string(),
                session_id: session_id.to_string(),
                claim_key: claim_key.clone(),
                claim_text: claim_text.clone(),
                url_hash: sha256_hex(url.trim().to_ascii_lowercase().as_str()),
                url: url.clone(),
                domain: extract_url_domain(url),
                relation: relation.to_string(),
                source_tool,
                source_route,
                evidence_excerpt: excerpt,
                confidence: edge.confidence,
            });
        }
    }
    upsert_pm_evidence_graph_edges(db, &rows).await
}

pub(super) fn score_pm_probe_quality(quality: &PmAnswerQualityDto) -> i64 {
    let pass_bonus = if quality.passed { 500 } else { 0 };
    let align_bonus = if quality.claim_alignment_ok { 120 } else { 0 };
    pass_bonus
        + align_bonus
        + (quality.tool_call_count as i64 * 15)
        + (quality.citation_count as i64 * 20)
        + (quality.domain_count as i64 * 30)
}

pub(super) fn build_pm_tool_summary_value(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> serde_json::Value {
    let mut by_name = std::collections::BTreeMap::<String, usize>::new();
    let mut by_name_error = std::collections::BTreeMap::<String, usize>::new();
    let mut samples = Vec::new();
    let mut summary_urls = Vec::<String>::new();
    let mut seen_urls = std::collections::HashSet::<String>::new();
    let mut error_count = 0usize;
    let mut filter_before_urls = Vec::<String>::new();
    let mut filter_after_urls = Vec::<String>::new();
    let mut filter_dropped_urls = Vec::<String>::new();
    let mut seen_filter_before_urls = std::collections::HashSet::<String>::new();
    let mut seen_filter_after_urls = std::collections::HashSet::<String>::new();
    let mut seen_filter_dropped_urls = std::collections::HashSet::<String>::new();
    let mut filter_before_count = 0usize;
    let mut filter_after_count = 0usize;
    let mut filter_dropped_count = 0usize;
    let mut filter_call_count = 0usize;
    let mut search_layer_attempts = std::collections::BTreeMap::<String, usize>::new();
    let mut search_layer_errors = std::collections::BTreeMap::<String, usize>::new();
    let mut search_layer_success = std::collections::BTreeMap::<String, usize>::new();
    for tc in tool_calls {
        *by_name.entry(tc.tool_name.clone()).or_insert(0) += 1;
        if tc.is_error {
            error_count += 1;
            *by_name_error.entry(tc.tool_name.clone()).or_insert(0) += 1;
        }
        if let Some(diff) = parse_web_search_quality_filter_diff(tc) {
            filter_call_count += 1;
            filter_before_count = filter_before_count.saturating_add(diff.before_count);
            filter_after_count = filter_after_count.saturating_add(diff.after_count);
            filter_dropped_count = filter_dropped_count.saturating_add(diff.dropped_count);
            for url in diff.before_urls {
                if filter_before_urls.len() >= 20 {
                    break;
                }
                if seen_filter_before_urls.insert(url.clone()) {
                    filter_before_urls.push(url);
                }
            }
            for url in diff.after_urls {
                if filter_after_urls.len() >= 20 {
                    break;
                }
                if seen_filter_after_urls.insert(url.clone()) {
                    filter_after_urls.push(url);
                }
            }
            for url in diff.dropped_urls {
                if filter_dropped_urls.len() >= 20 {
                    break;
                }
                if seen_filter_dropped_urls.insert(url.clone()) {
                    filter_dropped_urls.push(url);
                }
            }
        }
        if tc.tool_name.eq_ignore_ascii_case("WebSearch") {
            let provider_trace_entries = parse_provider_trace_entries(tc);
            let mut classified_trace_count = 0usize;
            for trace in &provider_trace_entries {
                let Some(layer) = classify_pm_search_layer_from_trace_entry(trace) else {
                    continue;
                };
                classified_trace_count += 1;
                *search_layer_attempts.entry(layer.to_string()).or_insert(0) += 1;
                if pm_search_trace_entry_is_error(trace) {
                    *search_layer_errors.entry(layer.to_string()).or_insert(0) += 1;
                } else if pm_search_trace_entry_is_success(trace) {
                    *search_layer_success.entry(layer.to_string()).or_insert(0) += 1;
                }
            }
            if classified_trace_count == 0 {
                if let Some(layer) = classify_pm_search_layer_from_tool_call(tc) {
                    *search_layer_attempts.entry(layer.to_string()).or_insert(0) += 1;
                    if tc.is_error {
                        *search_layer_errors.entry(layer.to_string()).or_insert(0) += 1;
                    } else {
                        *search_layer_success.entry(layer.to_string()).or_insert(0) += 1;
                    }
                } else {
                    *search_layer_attempts
                        .entry("configured_search_provider".to_string())
                        .or_insert(0) += provider_trace_entries.len().max(1);
                    if provider_trace_entries.is_empty() {
                        if tc.is_error {
                            *search_layer_errors
                                .entry("configured_search_provider".to_string())
                                .or_insert(0) += 1;
                        } else {
                            *search_layer_success
                                .entry("configured_search_provider".to_string())
                                .or_insert(0) += 1;
                        }
                    }
                    for trace in provider_trace_entries {
                        if pm_search_trace_entry_is_error(&trace) {
                            *search_layer_errors
                                .entry("configured_search_provider".to_string())
                                .or_insert(0) += 1;
                        } else if pm_search_trace_entry_is_success(&trace) {
                            *search_layer_success
                                .entry("configured_search_provider".to_string())
                                .or_insert(0) += 1;
                        }
                    }
                }
            }
        } else if tc.tool_name.eq_ignore_ascii_case("WebFetch") {
            *search_layer_attempts
                .entry("web_fetch_tool".to_string())
                .or_insert(0) += 1;
            if tc.is_error {
                *search_layer_errors
                    .entry("web_fetch_tool".to_string())
                    .or_insert(0) += 1;
            } else {
                *search_layer_success
                    .entry("web_fetch_tool".to_string())
                    .or_insert(0) += 1;
            }
        } else if let Some(layer) = classify_pm_search_layer_from_tool_call(tc) {
            *search_layer_attempts.entry(layer.to_string()).or_insert(0) += 1;
            if tc.is_error {
                *search_layer_errors.entry(layer.to_string()).or_insert(0) += 1;
            } else {
                *search_layer_success.entry(layer.to_string()).or_insert(0) += 1;
            }
        }
    }
    for tc in tool_calls.iter().take(6) {
        let source_label = if tc.source_name.trim().is_empty() {
            tc.source.clone()
        } else {
            format!("{}:{}", tc.source, tc.source_name)
        };
        let mut sample_urls = extract_http_urls(&tc.output);
        sample_urls.extend(extract_http_urls(&tc.input));
        sample_urls = sample_urls
            .into_iter()
            .filter(|url| is_pm_high_signal_source_url(url))
            .collect();
        sample_urls.sort();
        sample_urls.dedup();
        for url in sample_urls.iter().take(4) {
            if seen_urls.insert(url.clone()) {
                summary_urls.push(url.clone());
            }
            if summary_urls.len() >= 24 {
                break;
            }
        }
        samples.push(serde_json::json!({
            "idx": tc.index,
            "tool": tc.tool_name,
            "source": source_label,
            "isError": tc.is_error,
            "durationMs": tc.duration_ms,
            "input": truncate_for_log(&tc.input, 140),
            "output": truncate_for_log(&tc.output, 160),
            "urls": sample_urls,
        }));
    }
    let mut summary = serde_json::json!({
        "count": tool_calls.len(),
        "errorCount": error_count,
        "byName": by_name,
        "byNameError": by_name_error,
        "urls": summary_urls,
        "samples": samples,
    });
    if filter_call_count > 0 {
        if let Some(obj) = summary.as_object_mut() {
            obj.insert(
                "urlFilterDiff".to_string(),
                serde_json::json!({
                    "calls": filter_call_count,
                    "beforeCount": filter_before_count,
                    "afterCount": filter_after_count,
                    "droppedCount": filter_dropped_count,
                    "beforeUrlsSample": filter_before_urls,
                    "afterUrlsSample": filter_after_urls,
                    "droppedUrlsSample": filter_dropped_urls,
                }),
            );
        }
    }
    if !search_layer_attempts.is_empty() {
        if let Some(obj) = summary.as_object_mut() {
            let rows = search_layer_attempts
                .iter()
                .map(|(layer, attempts)| {
                    serde_json::json!({
                        "layer": layer,
                        "status": if search_layer_errors.get(layer).copied().unwrap_or(0) > 0
                            && search_layer_success.get(layer).copied().unwrap_or(0) == 0 {
                            "failed"
                        } else {
                            "completed"
                        },
                        "attempts": attempts,
                        "successCount": search_layer_success.get(layer).copied().unwrap_or(0),
                        "errorCount": search_layer_errors.get(layer).copied().unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>();
            obj.insert(
                "searchUsage".to_string(),
                serde_json::json!({ "traces": rows }),
            );
        }
    }
    summary
}

fn classify_pm_search_layer_from_trace_entry(trace: &str) -> Option<&'static str> {
    let lower = trace.to_ascii_lowercase();
    if lower.contains("native_model_search") {
        return Some("native_model_search");
    }
    if lower.contains("mcp_search") {
        return Some("mcp_search");
    }
    if lower.contains("configured_search_provider") {
        return Some("configured_search_provider");
    }
    if lower.contains("rag_local") {
        return Some("rag_local");
    }
    None
}

fn pm_search_trace_entry_is_error(trace: &str) -> bool {
    let lower = trace.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("timeout")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("rate limit")
        || lower.contains("status=failed")
        || lower.contains("status=timeout")
}

fn pm_search_trace_entry_is_success(trace: &str) -> bool {
    let lower = trace.to_ascii_lowercase();
    lower.contains("ok")
        || lower.contains("hit")
        || lower.contains("success")
        || lower.contains("status=ok")
        || lower.contains("status=completed")
}

fn classify_pm_search_layer_from_tool_call(
    tc: &agent_gateway::ToolCallRecord,
) -> Option<&'static str> {
    let haystack =
        format!("{} {} {}", tc.tool_name, tc.source, tc.source_name).to_ascii_lowercase();
    if haystack.contains("native_model_search")
        || haystack.contains("web_search_preview")
        || haystack.contains("native web")
    {
        return Some("native_model_search");
    }
    if haystack.contains("mcp")
        && (haystack.contains("search")
            || haystack.contains("browser")
            || haystack.contains("fetch"))
    {
        return Some("mcp_search");
    }
    if haystack.contains("rag") || haystack.contains("local_evidence") {
        return Some("rag_local");
    }
    None
}

pub(super) fn classify_pm_tool_error_code(output: &str) -> Option<String> {
    let lower = output.to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Some("timeout".to_string());
    }
    if lower.contains("dns") || lower.contains("resolve host") {
        return Some("dns_error".to_string());
    }
    if lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("error sending request")
        || lower.contains("connect error")
    {
        return Some("connection_error".to_string());
    }
    if lower.contains("403") || lower.contains("forbidden") {
        return Some("http_403".to_string());
    }
    if lower.contains("401") || lower.contains("unauthorized") {
        return Some("http_401".to_string());
    }
    if lower.contains("429") || lower.contains("rate limit") {
        return Some("http_429".to_string());
    }
    if lower.contains("5xx")
        || lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
    {
        return Some("http_5xx".to_string());
    }
    if lower.contains("validation error") {
        return Some("validation_error".to_string());
    }
    Some("tool_error".to_string())
}

pub(super) fn classify_pm_runtime_error_code(err_text: &str) -> &'static str {
    let lower = err_text.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "timeout"
    } else if lower.contains("all api keys failed") || lower.contains("empty response from model") {
        "model_empty_response"
    } else if lower.contains("network") || lower.contains("request") || lower.contains("dns") {
        "network_error"
    } else if lower.contains("tool-only") || lower.contains("tool_only") {
        "tool_only"
    } else {
        "runtime_error"
    }
}

fn parse_pm_http_status(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    if let Some(idx) = lower.find("http status") {
        let tail = &lower[idx..];
        for token in tail.split(|c: char| !c.is_ascii_digit()) {
            if token.len() == 3 {
                if let Ok(v) = token.parse::<i64>() {
                    if (100..=599).contains(&v) {
                        return Some(v);
                    }
                }
            }
        }
    }
    for token in lower.split(|c: char| !c.is_ascii_digit()) {
        if token.len() == 3 {
            if let Ok(v) = token.parse::<i64>() {
                if (400..=599).contains(&v) {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn parse_json_value(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(trimmed).ok()
}

fn extract_first_url_from_json_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(raw) => normalize_http_url_candidate(raw),
        serde_json::Value::Array(items) => items.iter().find_map(extract_first_url_from_json_value),
        serde_json::Value::Object(map) => {
            for key in [
                "url",
                "link",
                "href",
                "source_url",
                "sourceUrl",
                "uri",
                "citation_url",
                "citationUrl",
            ] {
                if let Some(url) = map.get(key).and_then(extract_first_url_from_json_value) {
                    return Some(url);
                }
            }
            map.values().find_map(extract_first_url_from_json_value)
        }
        _ => None,
    }
}

fn pick_primary_tool_url_from_structured_output(
    tc: &agent_gateway::ToolCallRecord,
    text: &str,
) -> Option<String> {
    let json = parse_json_value(text)?;
    if tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        if let Some(results) = json.get("results").and_then(serde_json::Value::as_array) {
            for item in results {
                let Some(content) = item.get("content").and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                for hit in content {
                    if let Some(url) = hit
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .and_then(normalize_http_url_candidate)
                    {
                        return Some(url);
                    }
                }
            }
        }
    }
    extract_first_url_from_json_value(&json)
}

fn parse_provider_trace_entries(tc: &agent_gateway::ToolCallRecord) -> Vec<String> {
    let mut out = Vec::<String>::new();
    if tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        if let Some(json) = parse_json_value(&tc.output) {
            if let Some(items) = json
                .get("providerTrace")
                .and_then(serde_json::Value::as_array)
            {
                for entry in items {
                    let Some(text) = entry.as_str() else {
                        continue;
                    };
                    let normalized = text.trim();
                    if !normalized.is_empty() {
                        out.push(normalized.to_string());
                    }
                }
            }
        }
        if out.is_empty() {
            let lower = tc.output.to_ascii_lowercase();
            if let Some(idx) = lower.find("probe trace:") {
                let original_tail = tc.output.get(idx + "probe trace:".len()..).unwrap_or("");
                for part in original_tail.split('|') {
                    let normalized = part.trim();
                    if !normalized.is_empty() {
                        out.push(normalized.to_string());
                    }
                }
            }
        }
    }
    out.truncate(24);
    out
}

fn pick_primary_provider_from_trace(entries: &[String]) -> Option<String> {
    entries.iter().find_map(|entry| {
        let head = entry
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if head.is_empty() {
            None
        } else {
            Some(head)
        }
    })
}

#[derive(Debug, Clone)]
struct PmWebSearchLedgerHit {
    url: String,
    domain: Option<String>,
    title: Option<String>,
    snippet: Option<String>,
    relevance_score: Option<f64>,
    content_chars: Option<u64>,
    content_source: Option<String>,
}

#[derive(Debug, Clone)]
struct PmWebSearchEnrichmentFailure {
    url: String,
    reason: String,
    error_code: Option<String>,
    http_status: Option<i64>,
}

#[derive(Debug, Clone)]
struct PmWebSearchQualityFilterDiff {
    before_count: usize,
    after_count: usize,
    dropped_count: usize,
    before_urls: Vec<String>,
    after_urls: Vec<String>,
    dropped_urls: Vec<String>,
}

fn parse_web_search_quality_filter_diff(
    tc: &agent_gateway::ToolCallRecord,
) -> Option<PmWebSearchQualityFilterDiff> {
    if !tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        return None;
    }
    let json = parse_json_value(&tc.output)?;
    let diff = json.get("qualityFilter")?;
    let before_count = diff
        .get("beforeCount")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    let after_count = diff
        .get("afterCount")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    let dropped_count = diff
        .get("droppedCount")
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    let before_urls = parse_web_search_quality_filter_urls(diff.get("beforeUrlsSample"));
    let after_urls = parse_web_search_quality_filter_urls(diff.get("afterUrlsSample"));
    let dropped_urls = parse_web_search_quality_filter_urls(diff.get("droppedUrlsSample"));
    if before_count == 0
        && after_count == 0
        && dropped_count == 0
        && before_urls.is_empty()
        && after_urls.is_empty()
        && dropped_urls.is_empty()
    {
        return None;
    }
    Some(PmWebSearchQualityFilterDiff {
        before_count,
        after_count,
        dropped_count,
        before_urls,
        after_urls,
        dropped_urls,
    })
}

fn parse_web_search_quality_filter_urls(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let mut seen = std::collections::HashSet::<String>::new();
    let Some(items) = value.and_then(serde_json::Value::as_array) else {
        return out;
    };
    for item in items {
        let Some(url) = item
            .as_str()
            .and_then(normalize_http_url_candidate)
            .filter(|url| seen.insert(url.clone()))
        else {
            continue;
        };
        out.push(url);
        if out.len() >= 20 {
            break;
        }
    }
    out
}

fn parse_web_search_ledger_hits(tc: &agent_gateway::ToolCallRecord) -> Vec<PmWebSearchLedgerHit> {
    if !tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        return Vec::new();
    }
    let Some(json) = parse_json_value(&tc.output) else {
        return Vec::new();
    };
    let Some(results) = json.get("results").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut hits = Vec::<PmWebSearchLedgerHit>::new();
    for item in results {
        let Some(content_rows) = item.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for row in content_rows {
            let Some(url) = row
                .get("url")
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_http_url_candidate)
            else {
                continue;
            };
            let domain = row
                .get("domain")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .or_else(|| extract_url_domain(&url));
            let title = row
                .get("title")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(truncate_for_log(trimmed, 240))
                    }
                });
            let snippet = row
                .get("snippet")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(truncate_for_log(trimmed, 320))
                    }
                });
            let relevance_score = row
                .get("relevanceScore")
                .and_then(serde_json::Value::as_f64);
            let content_chars = row.get("contentChars").and_then(serde_json::Value::as_u64);
            let content_source = row
                .get("contentSource")
                .and_then(serde_json::Value::as_str)
                .and_then(|raw| {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });
            hits.push(PmWebSearchLedgerHit {
                url,
                domain,
                title,
                snippet,
                relevance_score,
                content_chars,
                content_source,
            });
        }
    }
    let mut dedup = std::collections::HashSet::<String>::new();
    let mut out = Vec::<PmWebSearchLedgerHit>::new();
    for hit in hits {
        if dedup.insert(hit.url.clone()) {
            out.push(hit);
        }
        if out.len() >= 64 {
            break;
        }
    }
    out
}

fn parse_web_search_enrichment_failures(
    tc: &agent_gateway::ToolCallRecord,
) -> Vec<PmWebSearchEnrichmentFailure> {
    if !tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        return Vec::new();
    }
    let Some(json) = parse_json_value(&tc.output) else {
        return Vec::new();
    };
    let Some(trace) = json
        .get("enrichmentTrace")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut failures = Vec::<PmWebSearchEnrichmentFailure>::new();
    for item in trace {
        let Some(text) = item.as_str() else {
            continue;
        };
        let normalized = text.trim();
        if !normalized.starts_with("enrich: ") || !normalized.contains(" failed (") {
            continue;
        }
        let payload = normalized.trim_start_matches("enrich: ");
        let Some((url_part, reason_part)) = payload.split_once(" failed (") else {
            continue;
        };
        let Some(url) = normalize_http_url_candidate(url_part.trim()) else {
            continue;
        };
        let reason = reason_part.trim_end_matches(')').trim().to_string();
        let error_code = classify_pm_tool_error_code(&reason);
        let http_status = parse_pm_http_status(&reason);
        failures.push(PmWebSearchEnrichmentFailure {
            url,
            reason,
            error_code,
            http_status,
        });
        if failures.len() >= 64 {
            break;
        }
    }
    failures
}

fn pm_tool_ledger_detail_call_seq(base_call_seq: usize, detail_index: usize) -> usize {
    base_call_seq.saturating_add(detail_index.min(99))
}

fn pm_tool_ledger_call_seq(slot_seq: usize, tool_index: usize) -> usize {
    slot_seq
        .saturating_mul(10_000)
        .saturating_add(tool_index.saturating_add(1).saturating_mul(100))
}

pub(super) fn pick_primary_tool_url(tc: &agent_gateway::ToolCallRecord) -> Option<String> {
    pick_primary_tool_url_from_structured_output(tc, &tc.output)
        .or_else(|| extract_http_urls(&tc.output).into_iter().next())
        .or_else(|| pick_primary_tool_url_from_structured_output(tc, &tc.input))
        .or_else(|| extract_http_urls(&tc.input).into_iter().next())
}

fn pick_source_url_from_tool_calls(tool_calls: &[agent_gateway::ToolCallRecord]) -> Option<String> {
    for tc in tool_calls {
        if let Some(url) = pick_primary_tool_url(tc) {
            return Some(url);
        }
    }
    None
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct PmSourceToolLedgerBatch {
    pub(crate) source_slot: PmSourceSlotUpsertPayload,
    pub(crate) ledger_rows: Vec<PmToolCallLedgerRow>,
}

pub(super) fn build_pm_source_slot_and_tool_ledger(
    run_id: &str,
    attempt: usize,
    route_key: Option<&str>,
    route_channel: Option<&str>,
    variant: Option<&str>,
    status: &str,
    elapsed_ms: Option<u64>,
    error_code: Option<&str>,
    error_message: Option<&str>,
    detail: Option<&serde_json::Value>,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> PmSourceToolLedgerBatch {
    let source_url = pick_source_url_from_tool_calls(tool_calls);
    let source_slot = PmSourceSlotUpsertPayload {
        run_id: run_id.to_string(),
        stage_attempt_id: None,
        slot_seq: attempt,
        route_key: route_key.map(std::string::ToString::to_string),
        channel: route_channel.map(std::string::ToString::to_string),
        variant: variant.map(std::string::ToString::to_string),
        source_key: route_key
            .or(route_channel)
            .map(std::string::ToString::to_string),
        source_url,
        status: status.to_string(),
        tool_call_count: tool_calls.len(),
        elapsed_ms,
        error_code: error_code.map(std::string::ToString::to_string),
        error_message: error_message.map(std::string::ToString::to_string),
        detail: detail.cloned(),
    };

    let mut ledger_rows = Vec::new();
    let raw_max_chars = pm_env_usize("PM_TOOL_LEDGER_RAW_MAX_CHARS", 65_536).clamp(4_096, 524_288);
    for (index, tc) in tool_calls.iter().enumerate() {
        let call_seq = pm_tool_ledger_call_seq(attempt, index);
        let url = pick_primary_tool_url(tc);
        let domain = url.as_deref().and_then(extract_url_domain);
        let provider_trace_entries = parse_provider_trace_entries(tc);
        let provider = pick_primary_provider_from_trace(&provider_trace_entries);
        let provider_trace = if provider_trace_entries.is_empty() {
            None
        } else {
            Some(truncate_for_log(&provider_trace_entries.join(" | "), 2048))
        };
        let error_code_for_call = if tc.is_error {
            classify_pm_tool_error_code(&tc.output)
        } else {
            None
        };
        let error_message_for_call = if tc.is_error {
            Some(truncate_for_log(&tc.output, 512))
        } else {
            None
        };
        let route_key_owned = route_key.map(std::string::ToString::to_string);
        let channel_owned = route_channel.map(std::string::ToString::to_string);
        let provider_owned = provider.clone();
        let provider_trace_owned = provider_trace.clone();

        ledger_rows.push(PmToolCallLedgerRow {
            run_id: run_id.to_string(),
            stage_attempt_id: None,
            source_slot_id: None,
            call_seq,
            tool_name: tc.tool_name.clone(),
            tool_use_id: None,
            input_preview: Some(truncate_for_log(&tc.input, 4_096)),
            output_preview: Some(truncate_for_log(&tc.output, 4_096)),
            input_raw: Some(truncate_for_log(&tc.input, raw_max_chars)),
            output_raw: Some(truncate_for_log(&tc.output, raw_max_chars)),
            is_error: tc.is_error,
            error_code: error_code_for_call,
            error_message: error_message_for_call,
            http_status: parse_pm_http_status(&tc.output),
            latency_ms: Some(tc.duration_ms),
            route_key: route_key_owned.clone(),
            channel: channel_owned.clone(),
            provider,
            provider_trace,
            url,
            domain,
        });

        if tc.tool_name.eq_ignore_ascii_case("WebSearch") {
            let mut detail_index = 1usize;
            for hit in parse_web_search_ledger_hits(tc) {
                if detail_index >= 99 {
                    break;
                }
                let detail_seq = pm_tool_ledger_detail_call_seq(call_seq, detail_index);
                detail_index = detail_index.saturating_add(1);
                let hit_url = hit.url.clone();
                let hit_domain = hit.domain.clone();
                let detail_preview = format!(
                    "url={} domain={} score={} content_chars={} content_source={} title={} snippet={}",
                    hit_url,
                    hit_domain.clone().unwrap_or_default(),
                    hit.relevance_score
                        .map(|value| format!("{value:.2}"))
                        .unwrap_or_default(),
                    hit.content_chars
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    hit.content_source.clone().unwrap_or_default(),
                    hit.title.clone().unwrap_or_default(),
                    hit.snippet.clone().unwrap_or_default(),
                );
                let detail_raw = serde_json::json!({
                    "url": hit_url,
                    "domain": hit_domain,
                    "title": hit.title,
                    "snippet": hit.snippet,
                    "relevanceScore": hit.relevance_score,
                    "contentChars": hit.content_chars,
                    "contentSource": hit.content_source,
                })
                .to_string();
                ledger_rows.push(PmToolCallLedgerRow {
                    run_id: run_id.to_string(),
                    stage_attempt_id: None,
                    source_slot_id: None,
                    call_seq: detail_seq,
                    tool_name: "WebSearch.hit".to_string(),
                    tool_use_id: None,
                    input_preview: Some(truncate_for_log(&tc.input, 240)),
                    output_preview: Some(truncate_for_log(&detail_preview, 768)),
                    input_raw: None,
                    output_raw: Some(detail_raw),
                    is_error: false,
                    error_code: None,
                    error_message: None,
                    http_status: None,
                    latency_ms: None,
                    route_key: route_key_owned.clone(),
                    channel: channel_owned.clone(),
                    provider: provider_owned.clone(),
                    provider_trace: provider_trace_owned.clone(),
                    url: Some(hit_url),
                    domain: hit_domain,
                });
            }

            for failure in parse_web_search_enrichment_failures(tc) {
                if detail_index >= 99 {
                    break;
                }
                let detail_seq = pm_tool_ledger_detail_call_seq(call_seq, detail_index);
                detail_index = detail_index.saturating_add(1);
                let domain = extract_url_domain(&failure.url);
                ledger_rows.push(PmToolCallLedgerRow {
                    run_id: run_id.to_string(),
                    stage_attempt_id: None,
                    source_slot_id: None,
                    call_seq: detail_seq,
                    tool_name: "WebSearch.enrich".to_string(),
                    tool_use_id: None,
                    input_preview: Some(truncate_for_log(&tc.input, 240)),
                    output_preview: Some(truncate_for_log(&failure.reason, 768)),
                    input_raw: None,
                    output_raw: None,
                    is_error: true,
                    error_code: failure.error_code.clone(),
                    error_message: Some(truncate_for_log(&failure.reason, 512)),
                    http_status: failure.http_status,
                    latency_ms: None,
                    route_key: route_key_owned.clone(),
                    channel: channel_owned.clone(),
                    provider: provider_owned.clone(),
                    provider_trace: provider_trace_owned.clone(),
                    url: Some(failure.url.clone()),
                    domain,
                });
            }
        }
    }
    PmSourceToolLedgerBatch {
        source_slot,
        ledger_rows,
    }
}

pub(crate) async fn persist_pm_source_tool_ledger_batch_direct(
    db: &sqlx::SqlitePool,
    batch: &mut PmSourceToolLedgerBatch,
) -> Result<(), sqlx::Error> {
    let source_slot_id =
        pm_orchestrator::persistence::upsert_pm_source_slot_result(db, &batch.source_slot).await?;
    for row in &mut batch.ledger_rows {
        row.source_slot_id = Some(source_slot_id);
    }
    pm_orchestrator::persistence::upsert_pm_tool_call_ledger_batch_result(db, &batch.ledger_rows)
        .await
}

pub(super) async fn persist_pm_source_slot_and_tool_ledger(
    telemetry: &PmTelemetrySink,
    run_id: &str,
    attempt: usize,
    route_key: Option<&str>,
    route_channel: Option<&str>,
    variant: Option<&str>,
    status: &str,
    elapsed_ms: Option<u64>,
    error_code: Option<&str>,
    error_message: Option<&str>,
    detail: Option<&serde_json::Value>,
    tool_calls: &[agent_gateway::ToolCallRecord],
) {
    let batch = build_pm_source_slot_and_tool_ledger(
        run_id,
        attempt,
        route_key,
        route_channel,
        variant,
        status,
        elapsed_ms,
        error_code,
        error_message,
        detail,
        tool_calls,
    );
    telemetry
        .enqueue(PmTelemetryEvent::SourceToolLedger { batch })
        .await;
}

pub(super) async fn persist_pm_claim_and_conflict_records(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
    quality: &PmAnswerQualityDto,
) {
    let mut conflicted_claim_keys = std::collections::BTreeSet::<String>::new();
    for row in &quality.conflict_matrix {
        let a = normalize_claim_key(&row.claim_a);
        if !a.is_empty() {
            conflicted_claim_keys.insert(a);
        }
        let b = normalize_claim_key(&row.claim_b);
        if !b.is_empty() {
            conflicted_claim_keys.insert(b);
        }
    }

    let mut claim_rows = Vec::new();
    for row in quality.claim_alignment.iter().take(64) {
        let normalized = normalize_claim_key(&row.claim);
        if normalized.is_empty() {
            continue;
        }
        let is_conflicted = conflicted_claim_keys.contains(&normalized);
        let verdict = if is_conflicted {
            "conflicted"
        } else if row.cited {
            "confirmed"
        } else {
            "unverified"
        };
        let confidence = if is_conflicted {
            quality.conflict_confidence.clamp(0.25, 0.7)
        } else if row.cited {
            (0.55 + quality.triad_coverage * 0.4).clamp(0.55, 0.95)
        } else {
            0.25
        };
        let first_url = row.urls.first().cloned();
        let reason = if is_conflicted {
            Some("cross-source conflict requires adjudication".to_string())
        } else if row.cited {
            Some("claim has at least one evidence URL".to_string())
        } else {
            Some("claim missing evidence URL triad".to_string())
        };
        claim_rows.push(PmClaimVerdictRow {
            tenant_id: tenant_id.to_string(),
            run_id: run_id.to_string(),
            claim_key: sha256_hex(&normalized),
            claim_text: row.claim.chars().take(1024).collect(),
            verdict: verdict.to_string(),
            confidence,
            evidence_excerpt: Some(row.evidence_excerpt.chars().take(1024).collect()),
            domain: first_url.as_deref().and_then(extract_url_domain),
            url: first_url,
            reason,
        });
    }
    upsert_pm_claim_verdict_batch(db, &claim_rows).await;

    let mut conflict_rows = Vec::new();
    for row in quality.conflict_matrix.iter().take(32) {
        let topic_raw = if row.topic.trim().is_empty() {
            format!("{} vs {}", row.source_a.trim(), row.source_b.trim())
        } else {
            row.topic.clone()
        };
        let topic_key = sha256_hex(&normalize_claim_key(&topic_raw));
        let mut support_urls = Vec::new();
        for edge in &quality.conflict_graph.edges {
            if edge.topic.trim().eq_ignore_ascii_case(topic_raw.trim()) {
                support_urls.extend(edge.urls.clone());
            }
        }
        support_urls.sort();
        support_urls.dedup();
        conflict_rows.push(PmConflictCaseRow {
            tenant_id: tenant_id.to_string(),
            run_id: run_id.to_string(),
            topic_key,
            topic: topic_raw.chars().take(512).collect(),
            source_a: Some(row.source_a.chars().take(255).collect()),
            claim_a: Some(row.claim_a.chars().take(2048).collect()),
            source_b: Some(row.source_b.chars().take(255).collect()),
            claim_b: Some(row.claim_b.chars().take(2048).collect()),
            verdict: Some(row.verdict.chars().take(255).collect()),
            confidence: quality.conflict_confidence.clamp(0.0, 1.0),
            reason: Some(quality.conflict_reason.chars().take(1024).collect()),
            support_urls: Some(serde_json::json!(support_urls)),
        });
    }
    upsert_pm_conflict_case_batch(db, &conflict_rows).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(tool_name: &str, input: &str, output: &str) -> agent_gateway::ToolCallRecord {
        agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: tool_name.to_string(),
            source: "builtin".to_string(),
            source_name: "".to_string(),
            input: input.to_string(),
            output: output.to_string(),
            is_error: false,
            duration_ms: 10,
        }
    }

    #[test]
    fn hierarchical_tool_ledger_sequences_fit_platform_integer_range() {
        // 12 attempts * 10k plus a subtask slot is the configured hard-cap shape.
        let slot_seq = 120_016;
        let first = pm_tool_ledger_call_seq(slot_seq, 0);
        let first_last_detail = pm_tool_ledger_detail_call_seq(first, 99);
        let second = pm_tool_ledger_call_seq(slot_seq, 1);

        assert!(first_last_detail <= i32::MAX as usize);
        assert!(first_last_detail < second);
        assert_eq!(second - first, 100);
    }

    #[test]
    fn pick_primary_tool_url_prefers_structured_web_search_hit() {
        let tc = tool_call(
            "WebSearch",
            "{}",
            r#"{
                "query":"test",
                "results":[
                    "Search results",
                    {"tool_use_id":"web_search_1","content":[{"title":"Example","url":"https://example.com/a"}]}
                ],
                "durationSeconds":1.2,
                "providerTrace":["brave: attempt 1/2 ok"]
            }"#,
        );
        assert_eq!(
            pick_primary_tool_url(&tc).as_deref(),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn parse_provider_trace_entries_reads_web_search_provider_trace() {
        let tc = tool_call(
            "WebSearch",
            "{}",
            r#"{
                "query":"test",
                "results":[],
                "durationSeconds":1.2,
                "providerTrace":["brave: attempt 1/2 ok","tavily: attempt 1/2 ok_but_no_hits"]
            }"#,
        );
        let trace = parse_provider_trace_entries(&tc);
        assert_eq!(
            trace,
            vec![
                "brave: attempt 1/2 ok".to_string(),
                "tavily: attempt 1/2 ok_but_no_hits".to_string()
            ]
        );
        assert_eq!(
            pick_primary_provider_from_trace(&trace).as_deref(),
            Some("brave")
        );
    }

    #[test]
    fn build_pm_tool_summary_value_includes_search_usage() {
        let tc = tool_call(
            "WebSearch",
            "{}",
            r#"{
                "query":"test",
                "results":[],
                "providerTrace":["brave: attempt 1/2 failed timeout","tavily: attempt 1/2 ok"]
            }"#,
        );
        let summary = build_pm_tool_summary_value(&[tc]);
        let traces = summary
            .get("searchUsage")
            .and_then(|v| v.get("traces"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let configured = traces
            .iter()
            .find(|row| {
                row.get("layer").and_then(|v| v.as_str()) == Some("configured_search_provider")
            })
            .expect("configured provider search usage should be present");
        assert_eq!(configured.get("attempts").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(
            configured.get("successCount").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            configured.get("errorCount").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[test]
    fn build_pm_tool_summary_value_keeps_url_signals_from_full_output() {
        let long_prefix = "x".repeat(220);
        let url = "https://example.com/evidence";
        let tc = tool_call(
            "WebSearch",
            r#"{"query":"test"}"#,
            &format!("{long_prefix} {url}"),
        );
        let summary = build_pm_tool_summary_value(&[tc]);
        let urls = summary
            .get("urls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(urls.iter().any(|v| v.as_str() == Some(url)));
        let sample_urls = summary
            .get("samples")
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("urls"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(sample_urls.iter().any(|v| v.as_str() == Some(url)));
    }

    #[test]
    fn build_pm_tool_summary_value_includes_web_search_quality_filter_diff() {
        let tc = tool_call(
            "WebSearch",
            r#"{"query":"test"}"#,
            r#"{
                "query":"test",
                "qualityFilter":{
                    "beforeCount":25,
                    "afterCount":8,
                    "droppedCount":17,
                    "beforeUrlsSample":["https://example.com/a","https://example.com/b"],
                    "afterUrlsSample":["https://example.com/b"],
                    "droppedUrlsSample":["https://example.com/a"]
                },
                "results":[]
            }"#,
        );
        let summary = build_pm_tool_summary_value(&[tc]);
        let diff = summary
            .get("urlFilterDiff")
            .and_then(|value| value.as_object())
            .expect("urlFilterDiff should be present");
        assert_eq!(diff.get("calls").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(diff.get("beforeCount").and_then(|v| v.as_u64()), Some(25));
        assert_eq!(diff.get("afterCount").and_then(|v| v.as_u64()), Some(8));
        assert_eq!(diff.get("droppedCount").and_then(|v| v.as_u64()), Some(17));
        let dropped = diff
            .get("droppedUrlsSample")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(dropped
            .iter()
            .any(|value| value.as_str() == Some("https://example.com/a")));
    }

    #[test]
    fn parse_web_search_ledger_hits_reads_structured_hits() {
        let tc = tool_call(
            "WebSearch",
            "{}",
            r#"{
                "query":"ad mediation",
                "results":[
                    "Search results",
                    {"tool_use_id":"web_search_1","content":[
                        {
                            "title":"Case A",
                            "url":"https://example.com/a",
                            "snippet":"uplift 15%",
                            "domain":"example.com",
                            "contentChars":1234,
                            "contentSource":"readability",
                            "relevanceScore":0.91
                        },
                        {
                            "title":"Case B",
                            "url":"https://example.org/b",
                            "snippet":"uplift 22%",
                            "relevanceScore":0.77
                        }
                    ]}
                ]
            }"#,
        );
        let hits = parse_web_search_ledger_hits(&tc);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].domain.as_deref(), Some("example.com"));
        assert_eq!(hits[0].content_chars, Some(1234));
        assert_eq!(hits[0].content_source.as_deref(), Some("readability"));
        assert_eq!(hits[1].url, "https://example.org/b");
    }

    #[test]
    fn parse_web_search_enrichment_failures_reads_failed_urls() {
        let tc = tool_call(
            "WebSearch",
            "{}",
            r#"{
                "query":"test",
                "enrichmentTrace":[
                    "enrich: https://a.example/page failed (http_status=403 Forbidden)",
                    "enrich: https://b.example/page failed (error sending request for url (https://b.example/page))",
                    "enrich: attempted=7 valid_pages=2 target=3 max_candidates=7"
                ],
                "results":[]
            }"#,
        );
        let failures = parse_web_search_enrichment_failures(&tc);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].url, "https://a.example/page");
        assert_eq!(failures[0].error_code.as_deref(), Some("http_403"));
        assert_eq!(failures[0].http_status, Some(403));
        assert_eq!(failures[1].url, "https://b.example/page");
        assert_eq!(failures[1].error_code.as_deref(), Some("connection_error"));
    }
}
