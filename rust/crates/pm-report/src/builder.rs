use std::collections::HashSet;

use crate::{
    contains_cjk, extract_http_urls, extract_named_json_object, extract_pm_visible_answer_text,
    is_pm_high_signal_source_url, normalize_http_url_candidate, PmAnswerQualityDto,
    PmReportArtifactDto,
};

pub fn contains_any_token(haystack: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| haystack.contains(token))
}

fn classify_pm_question_type(question: &str) -> &'static str {
    let q = question.trim().to_ascii_lowercase();
    if q.is_empty() {
        return "general_research";
    }
    if contains_any_token(
        &q,
        &[
            "政策",
            "监管",
            "法务",
            "合规",
            "条例",
            "法律",
            "policy",
            "regulation",
            "compliance",
            "legal",
        ],
    ) {
        return "policy_regulation";
    }
    if contains_any_token(
        &q,
        &[
            "竞品",
            "竞争",
            "对手",
            "格局",
            "benchmark",
            "competitor",
            "competition",
            "landscape",
        ],
    ) {
        return "competitive_landscape";
    }
    if contains_any_token(
        &q,
        &[
            "用户",
            "留存",
            "流失",
            "体验",
            "评论",
            "反馈",
            "user",
            "retention",
            "churn",
            "review",
            "feedback",
        ],
    ) {
        return "user_insight";
    }
    if contains_any_token(
        &q,
        &[
            "roi",
            "arpu",
            "aipu",
            "ltv",
            "cpi",
            "cpa",
            "cpm",
            "ecpm",
            "买量",
            "投放",
            "变现",
            "广告",
            "商业化",
            "revenue",
            "monetization",
            "ads",
            "growth",
        ],
    ) {
        return "growth_monetization";
    }
    if contains_any_token(
        &q,
        &[
            "市场", "规模", "增长", "趋势", "行业", "market", "size", "trend", "industry",
        ],
    ) {
        return "market_research";
    }
    "general_research"
}

fn pm_quant_module_enabled(question: &str, question_type: &str) -> bool {
    if question_type == "growth_monetization" {
        return true;
    }
    let q = question.trim().to_ascii_lowercase();
    contains_any_token(
        &q,
        &[
            "roi",
            "arpu",
            "aipu",
            "ltv",
            "cpi",
            "cpa",
            "cpm",
            "ecpm",
            "留存",
            "转化",
            "付费率",
            "填充率",
            "收益测算",
            "敏感性",
            "scenario",
            "sensitivity",
            "forecast",
        ],
    )
}

fn pm_compact_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pm_strip_urls(input: &str) -> String {
    let mut out = input.to_string();
    for url in extract_http_urls(input) {
        out = out.replace(&url, " ");
    }
    pm_compact_whitespace(&out)
}

fn pm_is_noise_text(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.contains("low_claim_evidence_alignment")
        || lower.contains("insufficient_claim_evidence_url_triads")
        || lower.contains("missing_tool_retrieval")
        || lower.contains("missing_citations")
        || lower.contains("missing_conflict_matrix")
        || lower.contains("depth gate:")
        || lower.contains("dimension coverage gap:")
        || lower.contains("subtask_depth_gap:")
        || lower.contains("subtask_probe_gap:")
        || lower.contains("dimension_gap:")
        || lower.contains("auto_depth_repair_applied")
        || lower.contains("contract_invalid:")
        || lower.contains("runtime error")
        || lower.contains("runtime execution failed")
        || lower.contains("runtime recovery failed")
        || lower.contains("retrieve source slot timed out")
        || lower.contains("timed out")
        || lower.contains("tool '")
        || lower.contains("webfetch")
        || lower.contains("prompt:")
        || lower.starts_with("retrieve_constraints")
        || lower.starts_with("retrieve_result")
        || lower.starts_with("repair_scope")
        || lower.starts_with("repair_result")
        || lower.starts_with("synthesis_meta")
        || lower.starts_with("report_json")
}

fn pm_sanitize_sentence(raw: &str, max_chars: usize, keep_urls: bool) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if pm_is_noise_text(trimmed) {
        return None;
    }
    let mut cleaned = pm_compact_whitespace(trimmed);
    if cleaned.is_empty() {
        return None;
    }
    if normalize_http_url_candidate(&cleaned).is_some() {
        return None;
    }
    if !keep_urls {
        cleaned = pm_strip_urls(&cleaned);
    }
    cleaned = cleaned
        .trim_matches(|ch: char| {
            ch == '{' || ch == '}' || ch == '[' || ch == ']' || ch == '(' || ch == ')' || ch == '|'
        })
        .trim()
        .to_string();
    if cleaned.is_empty() || pm_is_noise_text(&cleaned) {
        return None;
    }
    if cleaned.chars().count() < 6 {
        return None;
    }
    Some(cleaned.chars().take(max_chars).collect())
}

fn pm_sanitize_list(items: Vec<String>, max_items: usize, keep_urls: bool) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        let Some(cleaned) = pm_sanitize_sentence(&item, 220, keep_urls) else {
            continue;
        };
        if out.iter().any(|existing: &String| existing == &cleaned) {
            continue;
        }
        out.push(cleaned);
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn pm_sanitize_url_list(items: Vec<String>, max_items: usize) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if let Some(url) = normalize_http_url_candidate(&item) {
            if is_pm_high_signal_source_url(&url)
                && !out.iter().any(|existing: &String| existing == &url)
            {
                out.push(url);
            }
        } else {
            for url in extract_http_urls(&item) {
                if is_pm_high_signal_source_url(&url)
                    && !out.iter().any(|existing: &String| existing == &url)
                {
                    out.push(url);
                }
            }
        }
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn pm_humanize_metric_key(raw: &str) -> String {
    let mut out = String::new();
    for part in raw
        .split(['_', '-', ' ', '.'])
        .filter(|part| !part.trim().is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            for ch in chars {
                out.push(ch.to_ascii_lowercase());
            }
        }
    }
    if out.is_empty() {
        "Metric".to_string()
    } else {
        out
    }
}

fn pm_metric_label_for_key(key: &str, cjk_mode: bool) -> String {
    match key {
        "roi" => {
            if cjk_mode {
                "投资回报率".to_string()
            } else {
                "ROI".to_string()
            }
        }
        "roas" => "ROAS".to_string(),
        "cpi" => "CPI".to_string(),
        "cpa" => "CPA".to_string(),
        "cpm" => "CPM".to_string(),
        "ecpm" => "eCPM".to_string(),
        "ltv" => "LTV".to_string(),
        "arpu" => "ARPU".to_string(),
        "pay_rate" => {
            if cjk_mode {
                "付费率".to_string()
            } else {
                "Pay Rate".to_string()
            }
        }
        "fill_rate" => {
            if cjk_mode {
                "填充率".to_string()
            } else {
                "Fill Rate".to_string()
            }
        }
        "retention_d1" => "D1 Retention".to_string(),
        "retention_d7" => "D7 Retention".to_string(),
        "retention_d30" => "D30 Retention".to_string(),
        "revenue" => {
            if cjk_mode {
                "收入".to_string()
            } else {
                "Revenue".to_string()
            }
        }
        "users" => {
            if cjk_mode {
                "用户规模".to_string()
            } else {
                "Users".to_string()
            }
        }
        _ => pm_humanize_metric_key(key),
    }
}

fn pm_metric_direction_for_key(key: &str) -> &'static str {
    match key {
        "cpi" | "cpa" | "cpm" => "lower_better",
        "roi" | "roas" | "ecpm" | "ltv" | "arpu" | "pay_rate" | "fill_rate" | "retention_d1"
        | "retention_d7" | "retention_d30" | "revenue" | "users" => "higher_better",
        _ => "neutral",
    }
}

fn pm_detect_metric_key(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let rules: &[(&str, &[&str])] = &[
        ("roi", &["roi", "投资回报"]),
        ("roas", &["roas"]),
        ("cpi", &["cpi", "获客成本", "安装成本"]),
        ("cpa", &["cpa"]),
        ("cpm", &["cpm"]),
        ("ecpm", &["ecpm"]),
        ("ltv", &["ltv"]),
        ("arpu", &["arpu", "人均收入"]),
        ("pay_rate", &["pay rate", "付费率"]),
        ("fill_rate", &["fill rate", "填充率"]),
        ("retention_d1", &["d1", "次日留存"]),
        ("retention_d7", &["d7", "7日留存", "七日留存"]),
        ("retention_d30", &["d30", "30日留存"]),
        ("revenue", &["revenue", "收入", "营收"]),
        ("users", &["mau", "dau", "users", "用户"]),
    ];
    for (key, tokens) in rules {
        if tokens.iter().any(|token| {
            let token_lower = token.to_ascii_lowercase();
            lower.contains(&token_lower) || text.contains(token)
        }) {
            return Some((*key).to_string());
        }
    }
    None
}

fn pm_parse_numeric_token(raw: &str) -> Option<(f64, String, String)> {
    let cleaned = raw
        .trim()
        .trim_matches(|ch: char| {
            ch == ','
                || ch == ';'
                || ch == ':'
                || ch == '，'
                || ch == '。'
                || ch == '：'
                || ch == '('
                || ch == ')'
                || ch == '['
                || ch == ']'
                || ch == '{'
                || ch == '}'
                || ch == '「'
                || ch == '」'
                || ch == '"'
                || ch == '\''
        })
        .trim();
    if cleaned.is_empty() || !cleaned.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let display = cleaned.to_string();
    let mut numeric = cleaned.to_ascii_lowercase();
    let mut unit = String::new();

    if numeric.ends_with('%') {
        unit = "%".to_string();
        numeric.pop();
    }
    if numeric.starts_with('$') {
        if unit.is_empty() {
            unit = "USD".to_string();
        }
        numeric = numeric.trim_start_matches('$').to_string();
    }
    if numeric.starts_with("usd") {
        if unit.is_empty() {
            unit = "USD".to_string();
        }
        numeric = numeric.trim_start_matches("usd").to_string();
    }
    if numeric.ends_with("usd") {
        if unit.is_empty() {
            unit = "USD".to_string();
        }
        numeric.truncate(numeric.len().saturating_sub(3));
    }
    if numeric.starts_with("idr") {
        if unit.is_empty() {
            unit = "IDR".to_string();
        }
        numeric = numeric.trim_start_matches("idr").to_string();
    }
    if numeric.starts_with("rp") {
        if unit.is_empty() {
            unit = "IDR".to_string();
        }
        numeric = numeric.trim_start_matches("rp").to_string();
    }
    if numeric.ends_with("idr") {
        if unit.is_empty() {
            unit = "IDR".to_string();
        }
        numeric.truncate(numeric.len().saturating_sub(3));
    }
    if numeric.ends_with("cny") || numeric.ends_with("rmb") {
        if unit.is_empty() {
            unit = "CNY".to_string();
        }
        numeric.truncate(numeric.len().saturating_sub(3));
    }

    numeric = numeric.replace([',', '_'], "").trim().to_string();
    if numeric.is_empty() {
        return None;
    }

    let mut multiplier = 1.0f64;
    if numeric.ends_with('k') {
        multiplier = 1_000.0;
        numeric.pop();
    } else if numeric.ends_with('m') {
        multiplier = 1_000_000.0;
        numeric.pop();
    } else if numeric.ends_with('b') {
        multiplier = 1_000_000_000.0;
        numeric.pop();
    }
    numeric = numeric.trim().to_string();
    if numeric.is_empty() {
        return None;
    }

    let parsed = numeric.parse::<f64>().ok()?;
    let value = parsed * multiplier;
    if !value.is_finite() {
        return None;
    }
    Some((value, display, unit))
}

fn pm_extract_numeric_from_text(text: &str) -> Option<(f64, String, String)> {
    let mut best: Option<(f64, String, String, usize)> = None;
    for token in text.split_whitespace() {
        let Some((value, display, unit)) = pm_parse_numeric_token(token) else {
            continue;
        };
        let mut score = display.len();
        if !unit.is_empty() {
            score = score.saturating_add(4);
        }
        if display.contains('%') {
            score = score.saturating_add(2);
        }
        match best.as_ref() {
            Some((_, _, _, best_score)) if *best_score >= score => {}
            _ => {
                best = Some((value, display, unit, score));
            }
        }
    }
    best.map(|(value, display, unit, _)| (value, display, unit))
}

fn pm_build_metric_from_line(line: &str, source_urls: &[String]) -> Option<serde_json::Value> {
    let clean_line = pm_sanitize_sentence(line, 220, false)?;
    let key = pm_detect_metric_key(&clean_line)?;
    let (value, display, unit) = pm_extract_numeric_from_text(&clean_line)?;
    let cjk_mode = contains_cjk(&clean_line);
    let label = pm_metric_label_for_key(&key, cjk_mode);
    let source_url = source_urls.first().cloned().unwrap_or_default();
    let mut confidence = 0.58f64;
    if !source_url.is_empty() {
        confidence += 0.12;
    }
    if key != "metric" {
        confidence += 0.08;
    }
    confidence = confidence.clamp(0.0, 0.95);
    Some(serde_json::json!({
        "key": key,
        "label": label,
        "value": value,
        "display": display,
        "unit": unit,
        "direction": pm_metric_direction_for_key(&key),
        "period": "latest",
        "sourceUrl": source_url,
        "confidence": confidence,
    }))
}

fn pm_extract_metrics_from_lines(
    lines: &[String],
    source_urls: &[String],
    max_items: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::<serde_json::Value>::new();
    let mut seen = HashSet::<String>::new();
    for line in lines {
        let Some(metric) = pm_build_metric_from_line(line, source_urls) else {
            continue;
        };
        let key = metric
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("metric")
            .to_string();
        let display = metric
            .get("display")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let dedup_key = format!("{key}|{display}");
        if !seen.insert(dedup_key) {
            continue;
        }
        out.push(metric);
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn pm_extract_metrics_from_quant(
    quant_value: Option<&serde_json::Value>,
    source_urls: &[String],
    max_items: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::<serde_json::Value>::new();
    let mut seen = HashSet::<String>::new();
    let Some(quant) = quant_value.and_then(|v| v.as_object()) else {
        return out;
    };
    let Some(scenarios) = quant.get("scenarios").and_then(|v| v.as_array()) else {
        return out;
    };
    for (scenario_idx, scenario) in scenarios.iter().enumerate() {
        let Some(obj) = scenario.as_object() else {
            continue;
        };
        let scenario_name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .and_then(|raw| pm_sanitize_sentence(raw, 80, false))
            .unwrap_or_else(|| "scenario".to_string());
        let Some(metrics_obj) = obj.get("metrics").and_then(|v| v.as_object()) else {
            continue;
        };
        for (raw_key, raw_value) in metrics_obj {
            let key = pm_detect_metric_key(raw_key).unwrap_or_else(|| {
                let normalized = raw_key.trim().to_ascii_lowercase().replace([' ', '-'], "_");
                if normalized.is_empty() {
                    "metric".to_string()
                } else {
                    normalized
                }
            });
            let cjk_mode = contains_cjk(raw_key);
            let label = pm_metric_label_for_key(&key, cjk_mode);
            let (value, display, unit) = if let Some(number) = raw_value.as_f64() {
                (
                    number,
                    format!("{number:.2}"),
                    if key.contains("rate") || key.starts_with("retention_") {
                        "%".to_string()
                    } else {
                        String::new()
                    },
                )
            } else if let Some(number) = raw_value.as_i64() {
                (number as f64, number.to_string(), String::new())
            } else if let Some(number) = raw_value.as_u64() {
                (number as f64, number.to_string(), String::new())
            } else if let Some(raw_text) = raw_value.as_str() {
                match pm_extract_numeric_from_text(raw_text) {
                    Some(parsed) => parsed,
                    None => continue,
                }
            } else {
                continue;
            };
            if !value.is_finite() {
                continue;
            }
            let dedup_key = format!("{}|{}", key, display);
            if !seen.insert(dedup_key) {
                continue;
            }
            let source_url = if source_urls.is_empty() {
                String::new()
            } else {
                source_urls[scenario_idx % source_urls.len()].clone()
            };
            let mut confidence = 0.64f64;
            if !source_url.is_empty() {
                confidence += 0.12;
            }
            confidence = confidence.clamp(0.0, 0.96);
            out.push(serde_json::json!({
                "key": key,
                "label": label,
                "value": value,
                "display": display,
                "unit": unit,
                "direction": pm_metric_direction_for_key(&key),
                "period": scenario_name,
                "sourceUrl": source_url,
                "confidence": confidence,
            }));
            if out.len() >= max_items {
                return out;
            }
        }
    }
    out
}

fn pm_extract_timeseries_from_quant(
    quant_value: Option<&serde_json::Value>,
    source_urls: &[String],
    max_series: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::<serde_json::Value>::new();
    let Some(quant) = quant_value.and_then(|v| v.as_object()) else {
        return out;
    };
    let Some(scenarios) = quant.get("scenarios").and_then(|v| v.as_array()) else {
        return out;
    };
    for (scenario_idx, scenario) in scenarios.iter().enumerate() {
        let Some(obj) = scenario.as_object() else {
            continue;
        };
        let Some(metrics_obj) = obj.get("metrics").and_then(|v| v.as_object()) else {
            continue;
        };
        for (raw_key, raw_value) in metrics_obj {
            let Some(items) = raw_value.as_array() else {
                continue;
            };
            if items.len() < 2 {
                continue;
            }
            let key = pm_detect_metric_key(raw_key)
                .unwrap_or_else(|| raw_key.trim().to_ascii_lowercase().replace([' ', '-'], "_"));
            let cjk_mode = contains_cjk(raw_key);
            let label = pm_metric_label_for_key(&key, cjk_mode);
            let mut points = Vec::<serde_json::Value>::new();
            for (idx, item) in items.iter().take(16).enumerate() {
                if let Some(number) = item.as_f64() {
                    points.push(serde_json::json!({"x": idx + 1, "y": number}));
                    continue;
                }
                if let Some(number) = item.as_i64() {
                    points.push(serde_json::json!({"x": idx + 1, "y": number as f64}));
                    continue;
                }
                if let Some(number) = item.as_u64() {
                    points.push(serde_json::json!({"x": idx + 1, "y": number as f64}));
                    continue;
                }
                let Some(point_obj) = item.as_object() else {
                    continue;
                };
                let x = point_obj
                    .get("x")
                    .or_else(|| point_obj.get("date"))
                    .or_else(|| point_obj.get("period"))
                    .or_else(|| point_obj.get("time"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("n/a");
                let y = point_obj
                    .get("y")
                    .or_else(|| point_obj.get("value"))
                    .or_else(|| point_obj.get("metric"))
                    .and_then(|v| {
                        v.as_f64()
                            .or_else(|| v.as_i64().map(|i| i as f64))
                            .or_else(|| v.as_u64().map(|u| u as f64))
                    });
                if let Some(value) = y {
                    points.push(serde_json::json!({"x": x, "y": value}));
                }
            }
            if points.len() < 2 {
                continue;
            }
            let source_url = if source_urls.is_empty() {
                String::new()
            } else {
                source_urls[scenario_idx % source_urls.len()].clone()
            };
            out.push(serde_json::json!({
                "metricKey": key,
                "label": label,
                "points": points,
                "sourceUrl": source_url,
            }));
            if out.len() >= max_series {
                return out;
            }
        }
    }
    out
}

fn pm_build_source_trace(
    metrics: &[serde_json::Value],
    timeseries: &[serde_json::Value],
    max_items: usize,
) -> Vec<serde_json::Value> {
    let mut out = Vec::<serde_json::Value>::new();
    let mut seen = HashSet::<String>::new();
    for metric in metrics {
        let key = metric
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("metric")
            .to_string();
        let source_url = metric
            .get("sourceUrl")
            .and_then(|v| v.as_str())
            .and_then(normalize_http_url_candidate)
            .unwrap_or_default();
        if source_url.is_empty() || !is_pm_high_signal_source_url(&source_url) {
            continue;
        }
        let dedup_key = format!("{key}|{source_url}");
        if !seen.insert(dedup_key) {
            continue;
        }
        out.push(serde_json::json!({
            "metricKey": key,
            "sourceUrl": source_url,
            "note": "metric sample",
        }));
        if out.len() >= max_items {
            return out;
        }
    }
    for series in timeseries {
        let key = series
            .get("metricKey")
            .and_then(|v| v.as_str())
            .unwrap_or("metric")
            .to_string();
        let source_url = series
            .get("sourceUrl")
            .and_then(|v| v.as_str())
            .and_then(normalize_http_url_candidate)
            .unwrap_or_default();
        if source_url.is_empty() || !is_pm_high_signal_source_url(&source_url) {
            continue;
        }
        let dedup_key = format!("{key}|{source_url}");
        if !seen.insert(dedup_key) {
            continue;
        }
        out.push(serde_json::json!({
            "metricKey": key,
            "sourceUrl": source_url,
            "note": "timeseries sample",
        }));
        if out.len() >= max_items {
            return out;
        }
    }
    out
}

fn pm_build_metric_model(
    highlights: &[String],
    confirmed: &[String],
    breadth_scan: &[String],
    sources: &[String],
    quant_value: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut candidates = Vec::<String>::new();
    candidates.extend(highlights.iter().cloned());
    candidates.extend(confirmed.iter().cloned());
    candidates.extend(breadth_scan.iter().cloned());

    let mut metrics = pm_extract_metrics_from_quant(quant_value, sources, 8);
    if metrics.len() < 6 {
        let mut fallback = pm_extract_metrics_from_lines(&candidates, sources, 8);
        metrics.append(&mut fallback);
    }
    let mut dedup = HashSet::<String>::new();
    metrics.retain(|item| {
        let key = item
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("metric")
            .to_string();
        let display = item
            .get("display")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        dedup.insert(format!("{key}|{display}"))
    });
    metrics.truncate(8);

    let timeseries = pm_extract_timeseries_from_quant(quant_value, sources, 4);
    let source_trace = pm_build_source_trace(&metrics, &timeseries, 24);
    serde_json::json!({
        "metrics": metrics,
        "timeSeries": timeseries,
        "sourceTrace": source_trace,
        "coverage": {
            "structuredMetricCount": metrics.len(),
            "timeSeriesCount": timeseries.len(),
            "sourceTraceCount": source_trace.len(),
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn pm_compute_report_strategy(
    question_type: &str,
    metric_count: usize,
    timeseries_count: usize,
    source_count: usize,
    triad_count: usize,
    risk_count: usize,
    action_count: usize,
    quality: Option<&PmAnswerQualityDto>,
) -> serde_json::Value {
    let confidence_score = quality
        .map(|q| {
            ((q.triad_coverage * 0.45)
                + (q.conflict_confidence * 0.35)
                + ((q.domain_count.min(4) as f64 / 4.0) * 0.20))
                .clamp(0.0, 1.0)
        })
        .unwrap_or_else(|| {
            (0.35
                + (source_count.min(6) as f64 / 6.0) * 0.25
                + (triad_count.min(8) as f64 / 8.0) * 0.25
                + (metric_count.min(6) as f64 / 6.0) * 0.15)
                .clamp(0.0, 1.0)
        });
    let confidence_band = if confidence_score >= 0.72 {
        "high"
    } else if confidence_score >= 0.52 {
        "medium"
    } else {
        "low"
    };

    let density_score =
        metric_count * 3 + timeseries_count * 2 + source_count.min(20) + triad_count.min(20);
    let data_density = if density_score >= 26 {
        "high"
    } else if density_score >= 12 {
        "medium"
    } else {
        "low"
    };

    let layout = if question_type == "policy_regulation"
        || confidence_band == "low"
        || risk_count >= action_count.saturating_add(2)
    {
        "risk_first"
    } else if question_type == "growth_monetization" && data_density == "high" && metric_count >= 3
    {
        "metrics_first"
    } else if action_count >= 4 {
        "execution_first"
    } else {
        "balanced"
    };

    let section_order = match layout {
        "metrics_first" => vec!["overview", "insights", "action", "deep", "sources"],
        "risk_first" => vec!["insights", "overview", "deep", "action", "sources"],
        "execution_first" => vec!["action", "overview", "insights", "deep", "sources"],
        _ => vec!["overview", "insights", "deep", "action", "sources"],
    };

    let primary_focus = match question_type {
        "growth_monetization" => "growth_monetization",
        "user_insight" => "user_insight",
        "policy_regulation" => "policy_regulation",
        "competitive_landscape" => "competitive_landscape",
        "market_research" => "market_research",
        _ => "general_research",
    };

    serde_json::json!({
        "layout": layout,
        "dataDensity": data_density,
        "confidenceScore": confidence_score,
        "confidenceBand": confidence_band,
        "primaryFocus": primary_focus,
        "sectionOrder": section_order,
    })
}

fn pm_trim_list_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("• "))
    {
        return rest.trim();
    }
    let mut idx = 0usize;
    let bytes = trimmed.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0 && idx + 1 < bytes.len() && bytes[idx] == b'.' && bytes[idx + 1] == b' ' {
        return trimmed[idx + 2..].trim();
    }
    trimmed
}

fn pm_split_sentences(text: &str, max_items: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if matches!(ch, '.' | '!' | '?' | '。' | '！' | '？' | ';' | '；') {
            let sentence = pm_trim_list_prefix(&buf).trim();
            if let Some(cleaned) = pm_sanitize_sentence(sentence, 220, false) {
                if cleaned.chars().count() >= 12 {
                    out.push(cleaned);
                }
                if out.len() >= max_items {
                    return out;
                }
            }
            buf.clear();
        }
    }
    let tail = pm_trim_list_prefix(&buf).trim();
    if !tail.is_empty() && out.len() < max_items {
        if let Some(cleaned) = pm_sanitize_sentence(tail, 220, false) {
            out.push(cleaned);
        }
    }
    out
}

fn pm_collect_section_items(
    text: &str,
    heading_tokens: &[&str],
    stop_tokens: &[&str],
    max_items: usize,
) -> Vec<String> {
    let mut in_section = false;
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let is_heading = heading_tokens
            .iter()
            .any(|token| lower.contains(&token.to_ascii_lowercase()) || line.contains(token));
        if is_heading {
            in_section = true;
            continue;
        }
        if in_section {
            let is_stop = stop_tokens
                .iter()
                .any(|token| lower.contains(&token.to_ascii_lowercase()) || line.contains(token));
            if is_stop || line.starts_with('#') {
                break;
            }
            let body = pm_trim_list_prefix(line);
            if body.is_empty() {
                continue;
            }
            if let Some(cleaned) = pm_sanitize_sentence(body, 220, false) {
                out.push(cleaned);
            }
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

fn pm_pick_json_value<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a serde_json::Value> {
    for key in keys {
        if let Some(value) = obj.get(*key) {
            return Some(value);
        }
    }
    for (key, value) in obj {
        if keys
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
        {
            return Some(value);
        }
    }
    None
}

fn pm_json_to_string(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() {
        None
    } else {
        pm_sanitize_sentence(raw, 2800, false)
    }
}

fn pm_json_to_string_list(value: Option<&serde_json::Value>, max_items: usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(cleaned) = pm_sanitize_sentence(trimmed, 220, false) {
                        out.push(cleaned);
                    }
                }
                if out.len() >= max_items {
                    break;
                }
            }
        }
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                if let Some(cleaned) = pm_sanitize_sentence(trimmed, 220, false) {
                    out.push(cleaned);
                }
            }
        }
        _ => {}
    }
    out
}

fn pm_json_to_url_list(value: Option<&serde_json::Value>, max_items: usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let push_urls = |raw: &str, out: &mut Vec<String>| {
        if let Some(url) = normalize_http_url_candidate(raw) {
            if is_pm_high_signal_source_url(&url) {
                out.push(url);
            }
            return;
        }
        for url in extract_http_urls(raw) {
            if is_pm_high_signal_source_url(&url) {
                out.push(url);
            }
        }
    };
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    push_urls(text, &mut out);
                }
                if out.len() >= max_items {
                    break;
                }
            }
        }
        serde_json::Value::String(text) => {
            push_urls(text, &mut out);
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out.truncate(max_items);
    out
}

pub fn pm_escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn build_pm_report_artifact(
    user_question: Option<&str>,
    answer_text: &str,
    quality: Option<&PmAnswerQualityDto>,
) -> PmReportArtifactDto {
    let question = user_question.unwrap_or("").trim();
    let question_type = classify_pm_question_type(question).to_string();
    let quant_enabled = pm_quant_module_enabled(question, &question_type);
    let visible_text = extract_pm_visible_answer_text(answer_text);
    let raw_report = extract_named_json_object(answer_text, "REPORT_JSON");
    let mut summary = String::new();
    let mut highlights: Vec<String> = Vec::new();
    let mut confirmed: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut risks: Vec<String> = Vec::new();
    let mut actions: Vec<String> = Vec::new();
    let mut open_questions: Vec<String> = Vec::new();
    let mut breadth_scan: Vec<String> = Vec::new();
    let mut priority_deep_dives: Vec<serde_json::Value> = Vec::new();
    let mut counter_evidence_checks: Vec<serde_json::Value> = Vec::new();
    let mut action_plan_now: Vec<String> = Vec::new();
    let mut action_plan_next: Vec<String> = Vec::new();
    let mut action_plan_later: Vec<String> = Vec::new();
    let mut report_sources: Vec<String> = Vec::new();
    let mut report_evidence_triads: Vec<serde_json::Value> = Vec::new();

    if let Some(value) = raw_report.as_ref().and_then(|v| v.as_object()) {
        summary = pm_json_to_string(pm_pick_json_value(value, &["summary", "executiveSummary"]))
            .unwrap_or_default();
        highlights = pm_json_to_string_list(pm_pick_json_value(value, &["highlights"]), 8);
        if let Some(section_obj) =
            pm_pick_json_value(value, &["sections"]).and_then(|v| v.as_object())
        {
            confirmed = pm_json_to_string_list(pm_pick_json_value(section_obj, &["confirmed"]), 8);
            pending = pm_json_to_string_list(
                pm_pick_json_value(section_obj, &["pending", "toValidate"]),
                8,
            );
            risks = pm_json_to_string_list(pm_pick_json_value(section_obj, &["risks"]), 8);
            actions = pm_json_to_string_list(
                pm_pick_json_value(section_obj, &["actions", "recommendations"]),
                8,
            );
        }
        open_questions = pm_json_to_string_list(pm_pick_json_value(value, &["openQuestions"]), 8);
        report_sources = pm_json_to_url_list(pm_pick_json_value(value, &["sources"]), 24);
        if let Some(triads) =
            pm_pick_json_value(value, &["evidenceTriads"]).and_then(|v| v.as_array())
        {
            for row in triads.iter().take(24) {
                let Some(obj) = row.as_object() else {
                    continue;
                };
                let claim = obj
                    .get("claim")
                    .and_then(|v| v.as_str())
                    .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                    .unwrap_or_default();
                let evidence = obj
                    .get("evidence")
                    .and_then(|v| v.as_str())
                    .and_then(|raw| pm_sanitize_sentence(raw, 320, false))
                    .unwrap_or_else(|| claim.clone());
                let url = obj
                    .get("url")
                    .and_then(|v| v.as_str())
                    .and_then(normalize_http_url_candidate)
                    .filter(|url| is_pm_high_signal_source_url(url))
                    .unwrap_or_default();
                if claim.is_empty() && evidence.is_empty() && url.is_empty() {
                    continue;
                }
                report_evidence_triads.push(serde_json::json!({
                    "claim": claim,
                    "evidence": evidence,
                    "url": url,
                    "cited": !url.is_empty(),
                }));
            }
        }
        if let Some(layer_obj) =
            pm_pick_json_value(value, &["deepResearchLayers"]).and_then(|v| v.as_object())
        {
            breadth_scan =
                pm_json_to_string_list(pm_pick_json_value(layer_obj, &["breadthScan"]), 10);
            if let Some(items) =
                pm_pick_json_value(layer_obj, &["priorityDeepDives"]).and_then(|v| v.as_array())
            {
                for item in items.iter().take(6) {
                    let Some(item_obj) = item.as_object() else {
                        continue;
                    };
                    let topic = pm_json_to_string(pm_pick_json_value(item_obj, &["topic"]))
                        .unwrap_or_default();
                    let insights =
                        pm_json_to_string_list(pm_pick_json_value(item_obj, &["insights"]), 5);
                    let implication = pm_json_to_string(pm_pick_json_value(
                        item_obj,
                        &["implication", "businessImpact"],
                    ))
                    .unwrap_or_default();
                    let evidence_urls = pm_json_to_url_list(
                        pm_pick_json_value(item_obj, &["evidenceUrls", "sources"]),
                        4,
                    );
                    if topic.is_empty() && insights.is_empty() && implication.is_empty() {
                        continue;
                    }
                    priority_deep_dives.push(serde_json::json!({
                        "topic": topic,
                        "insights": insights,
                        "evidenceUrls": evidence_urls,
                        "implication": implication,
                    }));
                }
            }
            if let Some(items) =
                pm_pick_json_value(layer_obj, &["counterEvidenceChecks"]).and_then(|v| v.as_array())
            {
                for item in items.iter().take(8) {
                    let Some(item_obj) = item.as_object() else {
                        continue;
                    };
                    let topic = pm_json_to_string(pm_pick_json_value(item_obj, &["topic"]))
                        .unwrap_or_default();
                    let supporting =
                        pm_json_to_string(pm_pick_json_value(item_obj, &["supporting", "pro"]))
                            .unwrap_or_default();
                    let counter = pm_json_to_string(pm_pick_json_value(
                        item_obj,
                        &["counter", "con", "counterClaim"],
                    ))
                    .unwrap_or_default();
                    let verdict =
                        pm_json_to_string(pm_pick_json_value(item_obj, &["verdict", "decision"]))
                            .unwrap_or_default();
                    let confidence = pm_pick_json_value(item_obj, &["confidence"])
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.55);
                    let urls = pm_json_to_url_list(
                        pm_pick_json_value(item_obj, &["urls", "sources", "evidenceUrls"]),
                        4,
                    );
                    if topic.is_empty() && verdict.is_empty() {
                        continue;
                    }
                    counter_evidence_checks.push(serde_json::json!({
                        "topic": topic,
                        "supporting": supporting,
                        "counter": counter,
                        "verdict": verdict,
                        "confidence": confidence.clamp(0.0, 1.0),
                        "urls": urls,
                    }));
                }
            }
            if let Some(action_obj) =
                pm_pick_json_value(layer_obj, &["actionPlan"]).and_then(|v| v.as_object())
            {
                action_plan_now =
                    pm_json_to_string_list(pm_pick_json_value(action_obj, &["now"]), 6);
                action_plan_next =
                    pm_json_to_string_list(pm_pick_json_value(action_obj, &["next"]), 6);
                action_plan_later =
                    pm_json_to_string_list(pm_pick_json_value(action_obj, &["later"]), 6);
            }
        }
    }

    highlights = pm_sanitize_list(highlights, 8, false);
    confirmed = pm_sanitize_list(confirmed, 8, false);
    pending = pm_sanitize_list(pending, 8, false);
    risks = pm_sanitize_list(risks, 8, false);
    actions = pm_sanitize_list(actions, 8, false);
    open_questions = pm_sanitize_list(open_questions, 8, false);
    breadth_scan = pm_sanitize_list(breadth_scan, 10, false);
    action_plan_now = pm_sanitize_list(action_plan_now, 6, false);
    action_plan_next = pm_sanitize_list(action_plan_next, 6, false);
    action_plan_later = pm_sanitize_list(action_plan_later, 6, false);

    if !priority_deep_dives.is_empty() {
        let mut cleaned = Vec::new();
        for item in priority_deep_dives.iter().take(8) {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let topic = obj
                .get("topic")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                .unwrap_or_default();
            let insights = pm_sanitize_list(
                obj.get("insights")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                6,
                false,
            );
            let implication = obj
                .get("implication")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 240, false))
                .unwrap_or_default();
            let evidence_urls = pm_sanitize_url_list(
                obj.get("evidenceUrls")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                4,
            );
            if topic.is_empty() && insights.is_empty() && implication.is_empty() {
                continue;
            }
            cleaned.push(serde_json::json!({
                "topic": topic,
                "insights": insights,
                "evidenceUrls": evidence_urls,
                "implication": implication,
            }));
        }
        priority_deep_dives = cleaned;
    }
    if !counter_evidence_checks.is_empty() {
        let mut cleaned = Vec::new();
        for item in counter_evidence_checks.iter().take(8) {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let topic = obj
                .get("topic")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                .unwrap_or_default();
            let supporting = obj
                .get("supporting")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                .unwrap_or_default();
            let counter = obj
                .get("counter")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                .unwrap_or_default();
            let verdict = obj
                .get("verdict")
                .and_then(|v| v.as_str())
                .and_then(|raw| pm_sanitize_sentence(raw, 220, false))
                .unwrap_or_default();
            let confidence = obj
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.55)
                .clamp(0.0, 1.0);
            let urls = pm_sanitize_url_list(
                obj.get("urls")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                4,
            );
            if topic.is_empty() && supporting.is_empty() && counter.is_empty() && verdict.is_empty()
            {
                continue;
            }
            cleaned.push(serde_json::json!({
                "topic": topic,
                "supporting": supporting,
                "counter": counter,
                "verdict": verdict,
                "confidence": confidence,
                "urls": urls,
            }));
        }
        counter_evidence_checks = cleaned;
    }

    if summary.trim().is_empty() {
        summary = pm_split_sentences(&visible_text, 2).join(" ");
    }
    if highlights.is_empty() {
        highlights = pm_split_sentences(&visible_text, 6);
    }
    if confirmed.is_empty() {
        confirmed = pm_collect_section_items(
            &visible_text,
            &["已证实", "confirmed", "facts confirmed"],
            &[
                "待验证",
                "风险项",
                "建议动作",
                "pending",
                "risk",
                "action",
                "recommendation",
            ],
            8,
        );
    }
    if pending.is_empty() {
        pending = pm_collect_section_items(
            &visible_text,
            &["待验证", "pending", "to validate"],
            &["风险项", "建议动作", "risk", "action", "recommendation"],
            8,
        );
    }
    if risks.is_empty() {
        risks = pm_collect_section_items(
            &visible_text,
            &["风险项", "risk", "risks"],
            &["建议动作", "action", "recommendation"],
            8,
        );
    }
    if actions.is_empty() {
        actions = pm_collect_section_items(
            &visible_text,
            &["建议动作", "recommendation", "actions"],
            &["open question", "待验证", "pending"],
            8,
        );
    }
    if open_questions.is_empty() {
        open_questions = pm_collect_section_items(
            &visible_text,
            &["待验证", "open question", "open questions", "pending"],
            &["风险项", "建议动作", "risk", "action", "recommendation"],
            6,
        );
    }

    if summary.trim().is_empty() {
        summary = if contains_cjk(question) {
            "本轮输出已完成跨源整理，但仍建议结合业务上下文继续验证关键假设。".to_string()
        } else {
            "This report provides a cross-source synthesis and highlights what should be validated next."
                .to_string()
        };
    }
    if highlights.is_empty() {
        highlights.push(summary.clone());
    }
    if confirmed.is_empty() && !highlights.is_empty() {
        confirmed = highlights.iter().take(3).cloned().collect();
    }
    if actions.is_empty() {
        actions.push(if contains_cjk(question) {
            "优先验证高影响假设，补充一手数据后再做预算/投放决策。".to_string()
        } else {
            "Validate high-impact assumptions with primary data before committing budget decisions."
                .to_string()
        });
    }

    let mut evidence_triads: Vec<serde_json::Value> = report_evidence_triads;
    let mut sources: Vec<String> = report_sources;
    if let Some(quality) = quality {
        for row in quality.claim_alignment.iter().take(24) {
            let Some(claim) = pm_sanitize_sentence(&row.claim, 220, false) else {
                continue;
            };
            let evidence = pm_sanitize_sentence(&row.evidence_excerpt, 320, false)
                .unwrap_or_else(|| claim.clone());
            let row_urls = pm_sanitize_url_list(row.urls.clone(), 4);
            let Some(url) = row_urls.first() else {
                continue;
            };
            sources.push(url.clone());
            evidence_triads.push(serde_json::json!({
                "claim": claim,
                "evidence": evidence,
                "url": url,
                "cited": row.cited,
            }));
        }
        for url in quality.citations.iter().take(24) {
            if let Some(cleaned) = normalize_http_url_candidate(url) {
                if !is_pm_high_signal_source_url(&cleaned) {
                    continue;
                }
                sources.push(cleaned);
            }
        }
    }
    if evidence_triads.is_empty() {
        for url in extract_http_urls(&visible_text).into_iter().take(16) {
            if !is_pm_high_signal_source_url(&url) {
                continue;
            }
            sources.push(url.clone());
            evidence_triads.push(serde_json::json!({
                "claim": highlights.first().cloned().unwrap_or_default(),
                "evidence": summary.clone(),
                "url": url,
                "cited": true,
            }));
        }
    }
    if evidence_triads.is_empty() {
        evidence_triads.push(serde_json::json!({
            "claim": pm_sanitize_sentence(&summary, 220, false).unwrap_or_else(|| summary.clone()),
            "evidence": pm_sanitize_sentence(&summary, 320, false).unwrap_or_else(|| summary.clone()),
            "url": "",
            "cited": false,
        }));
    }
    sources = pm_sanitize_url_list(sources, 40);

    let conflict_matrix = quality
        .map(|q| {
            q.conflict_matrix
                .iter()
                .take(16)
                .filter_map(|row| {
                    let topic = pm_sanitize_sentence(&row.topic, 160, false).unwrap_or_default();
                    let claim_a = pm_sanitize_sentence(&row.claim_a, 220, false).unwrap_or_default();
                    let claim_b = pm_sanitize_sentence(&row.claim_b, 220, false).unwrap_or_default();
                    let verdict =
                        pm_sanitize_sentence(&row.verdict, 160, false).unwrap_or_default();
                    if topic.is_empty() && claim_a.is_empty() && claim_b.is_empty() && verdict.is_empty()
                    {
                        return None;
                    }
                    Some(serde_json::json!({
                        "topic": topic,
                        "sourceA": pm_compact_whitespace(&row.source_a).chars().take(90).collect::<String>(),
                        "claimA": claim_a,
                        "sourceB": pm_compact_whitespace(&row.source_b).chars().take(90).collect::<String>(),
                        "claimB": claim_b,
                        "verdict": verdict,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if breadth_scan.is_empty() {
        breadth_scan = highlights.iter().take(6).cloned().collect();
        if breadth_scan.len() < 4 {
            breadth_scan.extend(
                confirmed
                    .iter()
                    .take(6usize.saturating_sub(breadth_scan.len()))
                    .cloned(),
            );
        }
    }
    if priority_deep_dives.is_empty() {
        for (idx, topic) in confirmed.iter().take(4).enumerate() {
            let implication = actions
                .get(idx)
                .cloned()
                .or_else(|| pending.get(idx).cloned())
                .unwrap_or_else(|| {
                    if contains_cjk(question) {
                        "需要继续补充更高置信度证据验证商业影响。".to_string()
                    } else {
                        "Requires additional high-confidence evidence before business commitment."
                            .to_string()
                    }
                });
            priority_deep_dives.push(serde_json::json!({
                "topic": topic,
                "insights": [topic],
                "evidenceUrls": sources.iter().skip(idx).take(2).cloned().collect::<Vec<_>>(),
                "implication": implication,
            }));
        }
    }
    if counter_evidence_checks.is_empty() {
        for row in conflict_matrix.iter().take(6) {
            let topic = row
                .get("topic")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let supporting = row
                .get("claimA")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let counter = row
                .get("claimB")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let verdict = row
                .get("verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if topic.is_empty() && verdict.is_empty() {
                continue;
            }
            counter_evidence_checks.push(serde_json::json!({
                "topic": topic,
                "supporting": supporting,
                "counter": counter,
                "verdict": verdict,
                "confidence": 0.62,
                "urls": [],
            }));
        }
    }
    if counter_evidence_checks.is_empty() {
        counter_evidence_checks.push(serde_json::json!({
            "topic": risks.first().cloned().unwrap_or_else(|| {
                if contains_cjk(question) {
                    "关键假设仍待验证".to_string()
                } else {
                    "Critical assumptions remain unverified".to_string()
                }
            }),
            "supporting": confirmed.first().cloned().unwrap_or_default(),
            "counter": pending.first().cloned().unwrap_or_default(),
            "verdict": if contains_cjk(question) {
                "证据不充分，先小步验证再扩量。"
            } else {
                "Evidence remains partial; run controlled validation before scale."
            },
            "confidence": 0.45,
            "urls": sources.iter().take(2).cloned().collect::<Vec<_>>(),
        }));
    }
    if action_plan_now.is_empty() {
        action_plan_now = actions.iter().take(3).cloned().collect();
    }
    if action_plan_next.is_empty() {
        action_plan_next = pending
            .iter()
            .take(3)
            .cloned()
            .chain(actions.iter().skip(3).take(2).cloned())
            .take(3)
            .collect();
    }
    if action_plan_later.is_empty() {
        action_plan_later = open_questions
            .iter()
            .take(3)
            .cloned()
            .chain(risks.iter().take(2).cloned())
            .take(3)
            .collect();
    }
    if action_plan_now.is_empty() {
        action_plan_now.push(if contains_cjk(question) {
            "先锁定 1-2 个高影响假设，72 小时内完成快速验证。".to_string()
        } else {
            "Lock 1-2 high-impact assumptions and validate within 72 hours.".to_string()
        });
    }
    if action_plan_next.is_empty() {
        action_plan_next.push(if contains_cjk(question) {
            "在 1-2 周内补齐关键缺口证据，并更新决策阈值。".to_string()
        } else {
            "Within 1-2 weeks, close key evidence gaps and refresh decision thresholds.".to_string()
        });
    }
    if action_plan_later.is_empty() {
        action_plan_later.push(if contains_cjk(question) {
            "按季度复盘策略收益与风险，迭代长期路线。".to_string()
        } else {
            "Review strategy outcomes quarterly and iterate long-term roadmap.".to_string()
        });
    }

    summary = pm_sanitize_sentence(&summary, 2800, false).unwrap_or(summary);
    highlights = pm_sanitize_list(highlights, 8, false);
    confirmed = pm_sanitize_list(confirmed, 8, false);
    pending = pm_sanitize_list(pending, 8, false);
    risks = pm_sanitize_list(risks, 8, false);
    actions = pm_sanitize_list(actions, 8, false);
    open_questions = pm_sanitize_list(open_questions, 8, false);
    breadth_scan = pm_sanitize_list(breadth_scan, 10, false);
    action_plan_now = pm_sanitize_list(action_plan_now, 6, false);
    action_plan_next = pm_sanitize_list(action_plan_next, 6, false);
    action_plan_later = pm_sanitize_list(action_plan_later, 6, false);
    sources = pm_sanitize_url_list(sources, 40);
    if highlights.is_empty() {
        highlights.push(summary.clone());
    }
    if confirmed.is_empty() {
        confirmed = highlights.iter().take(3).cloned().collect();
    }
    if pending.is_empty() {
        pending = open_questions.iter().take(3).cloned().collect();
    }
    if risks.is_empty() {
        risks.push(if contains_cjk(question) {
            "关键变量仍需持续监测，避免在低置信条件下扩大投入。".to_string()
        } else {
            "Critical variables still require monitoring before scaling commitment.".to_string()
        });
    }
    if actions.is_empty() {
        actions = action_plan_now.iter().take(3).cloned().collect();
    }
    if breadth_scan.is_empty() {
        breadth_scan = highlights.iter().take(6).cloned().collect();
    }
    if action_plan_now.is_empty() {
        action_plan_now = actions.iter().take(3).cloned().collect();
    }
    if action_plan_next.is_empty() {
        action_plan_next = pending.iter().take(3).cloned().collect();
    }
    if action_plan_later.is_empty() {
        action_plan_later = risks.iter().take(3).cloned().collect();
    }

    let metric_model = pm_build_metric_model(
        &highlights,
        &confirmed,
        &breadth_scan,
        &sources,
        raw_report.as_ref().and_then(|v| v.get("quant")),
    );
    let metric_count = metric_model
        .get("coverage")
        .and_then(|v| v.get("structuredMetricCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let timeseries_count = metric_model
        .get("coverage")
        .and_then(|v| v.get("timeSeriesCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let report_strategy = pm_compute_report_strategy(
        &question_type,
        metric_count,
        timeseries_count,
        sources.len(),
        evidence_triads.len(),
        risks.len(),
        actions.len(),
        quality,
    );

    let report_json = serde_json::json!({
        "schemaVersion": "pm_report.v2",
        "question": question,
        "questionType": question_type,
        "summary": summary,
        "highlights": highlights,
        "sections": {
            "confirmed": confirmed,
            "pending": pending,
            "risks": risks,
            "actions": actions,
        },
        "deepResearchLayers": {
            "breadthScan": breadth_scan,
            "priorityDeepDives": priority_deep_dives,
            "counterEvidenceChecks": counter_evidence_checks,
            "actionPlan": {
                "now": action_plan_now,
                "next": action_plan_next,
                "later": action_plan_later,
            }
        },
        "evidenceTriads": evidence_triads,
        "conflictMatrix": conflict_matrix,
        "openQuestions": open_questions,
        "sources": sources,
        "metricModel": metric_model,
        "reportStrategy": report_strategy,
        "quant": {
            "enabled": quant_enabled,
            "notes": if quant_enabled {
                "Quant module enabled by intent (ROI/CPI/LTV/eCPM terms detected)."
            } else {
                "Quant module skipped for non-financial question intent."
            },
            "scenarios": [],
        },
    });
    let subtask_findings = if !priority_deep_dives.is_empty() {
        priority_deep_dives
            .iter()
            .take(18)
            .enumerate()
            .map(|(idx, item)| {
                let topic = item
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("untitled_subtask");
                let insights = item
                    .get("insights")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let evidence_urls = item
                    .get("evidenceUrls")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                serde_json::json!({
                    "subtask_key": format!("s{}", idx + 1),
                    "subtask_title": topic,
                    "status": "completed",
                    "findings": insights,
                    "evidence_urls": evidence_urls,
                })
            })
            .collect::<Vec<_>>()
    } else {
        confirmed
            .iter()
            .take(18)
            .enumerate()
            .map(|(idx, claim)| {
                serde_json::json!({
                    "subtask_key": format!("s{}", idx + 1),
                    "subtask_title": format!("finding-{}", idx + 1),
                    "status": "completed",
                    "findings": [claim],
                    "evidence_urls": [],
                })
            })
            .collect::<Vec<_>>()
    };
    let cross_task_conflicts = if !counter_evidence_checks.is_empty() {
        counter_evidence_checks.clone()
    } else {
        conflict_matrix
            .iter()
            .take(24)
            .map(|row| {
                serde_json::json!({
                    "topic": row.get("topic").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    "source_a": row.get("sourceA").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    "source_b": row.get("sourceB").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    "verdict": row.get("verdict").cloned().unwrap_or(serde_json::Value::String("pending".to_string())),
                })
            })
            .collect::<Vec<_>>()
    };
    let report_json_v3 = serde_json::json!({
        "schemaVersion": "pm_report.v3",
        "question": question,
        "questionType": question_type,
        "executive_summary": summary,
        "subtask_findings": subtask_findings,
        "cross_task_conflicts": cross_task_conflicts,
        "decision_matrix": {
            "confirmed": confirmed,
            "pending": pending,
            "risks": risks,
            "actions": actions,
        },
        "action_plan": {
            "now": action_plan_now,
            "next": action_plan_next,
            "later": action_plan_later,
        },
        "evidence_index": evidence_triads,
        "gaps_and_next_queries": open_questions,
        "sources": sources,
    });
    let report_html = render_pm_report_html(&report_json);
    let report_html_v3 = render_pm_report_html(&report_json_v3);
    PmReportArtifactDto {
        schema_version: "pm_report.v2".to_string(),
        question_type: classify_pm_question_type(question).to_string(),
        quant_enabled,
        report_json,
        report_html,
        report_json_v3: Some(report_json_v3),
        report_html_v3: Some(report_html_v3),
    }
}

fn render_pm_report_html(report_json: &serde_json::Value) -> String {
    crate::render_pm_report_html(report_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PmAnswerQualityDto, PmClaimEvidenceDto, PmConflictGraphDto};

    #[test]
    fn build_pm_report_artifact_sanitizes_noise_and_urls_from_report_json() {
        let question = "网赚游戏app印尼市场";
        let answer_text = r#"
研究结论
- 印尼网赚游戏赛道存在结构性增长窗口，重点在广告变现效率优化。

REPORT_JSON
{
  "summary": "印尼网赚游戏市场处于增长窗口，适合先做低风险验证。",
  "highlights": [
    "印尼网赚游戏赛道仍有增长空间，重点在广告填充率和留存。",
    "missing_tool_retrieval",
    "https://noise-only.example/path"
  ],
  "sections": {
    "confirmed": ["广告变现链路持续优化，现金流回收速度更快。"],
    "pending": ["核心买量渠道的真实 CPI 波动仍需补充样本验证。"],
    "risks": ["政策和平台规则变化会直接影响激励玩法与素材分发。"],
    "actions": ["先用小预算跑 72 小时试投，再按留存与回收分层扩量。"]
  },
  "deepResearchLayers": {
    "breadthScan": ["用户在线时长提升带来更多激励曝光窗口。"],
    "actionPlan": {
      "now": ["72 小时内完成首轮试投与素材 AB 对照。"],
      "next": ["两周内完成分渠道 ROI 对账与创意淘汰规则。"],
      "later": ["季度复盘留存、LTV 与政策风险阈值。"]
    }
  },
  "sources": [
    "{(https://dataportal.com/reports/digital-2025-indonesia\\nPrompt:)}",
    "https://www.appsflyer.com/resources/reports/app-marketing-trends/"
  ]
}
"#;

        let artifact = build_pm_report_artifact(Some(question), answer_text, None);
        let report = artifact.report_json;
        let highlights = report
            .get("highlights")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let highlight_texts = highlights
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(highlight_texts.iter().any(|v| v.contains("增长空间")));
        assert!(!highlight_texts
            .iter()
            .any(|v| v.contains("missing_tool_retrieval")));
        assert!(!highlight_texts
            .iter()
            .any(|v| v.starts_with("https://noise-only.example")));

        let sources = report
            .get("sources")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let source_texts = sources
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(source_texts.contains(&"https://dataportal.com/reports/digital-2025-indonesia"));
        assert!(!source_texts.iter().any(|v| v.contains("\\nPrompt:")));
    }

    #[test]
    fn build_pm_report_artifact_renders_enterprise_sections_for_sparse_input() {
        let question = "印尼网赚游戏 app 市场还有机会吗";
        let answer_text = "结论：方向可行，但需要先做小范围验证。";

        let artifact = build_pm_report_artifact(Some(question), answer_text, None);
        let html = artifact.report_html;

        assert!(html.contains("id=\"overview\""));
        assert!(html.contains("id=\"insights\""));
        assert!(html.contains("id=\"deep\""));
        assert!(html.contains("id=\"action\""));
        assert!(html.contains("id=\"sources\""));
        assert!(html.contains("企业研究交付"));
        assert!(!html.contains("missing_tool_retrieval"));
        assert!(!html.contains("low_claim_evidence_alignment"));
    }

    #[test]
    fn build_pm_report_artifact_adapts_section_labels_by_question_type() {
        let policy_question = "印尼网赚游戏 app 的政策监管和合规风险有哪些";
        let answer_text = "政策结论：重点关注激励玩法与广告合规。";

        let artifact = build_pm_report_artifact(Some(policy_question), answer_text, None);
        assert_eq!(artifact.question_type, "policy_regulation");
        assert!(artifact.report_html.contains("政策概览"));
        assert!(artifact.report_html.contains("合规洞察"));
        assert!(artifact.report_html.contains("合规红线"));
    }

    #[test]
    fn build_pm_report_artifact_emits_metric_model_and_strategy_v2() {
        let question = "印尼网赚游戏 app 的 ROI 和 CPI 还有提升空间吗";
        let answer_text = r#"
研究结论：ROI 1.6，CPI $0.34，D7 留存 18%。

REPORT_JSON
{
  "summary": "ROI 与 CPI 有优化空间，需继续压测素材组合。",
  "highlights": ["ROI 1.6，CPI $0.34，D7 留存 18%。"],
  "sections": {
    "confirmed": ["ROI 1.6，变现效率具备扩量潜力。"],
    "pending": ["CPI 在渠道切换后可能波动。"],
    "risks": ["广告平台政策变化影响激励流量。"],
    "actions": ["72 小时内完成素材分层与出价回归。"]
  },
  "deepResearchLayers": {
    "breadthScan": ["eCPM 8.7，D1 留存 39%。"],
    "priorityDeepDives": [],
    "counterEvidenceChecks": [],
    "actionPlan": {"now":["校准预算"],"next":["验证创意"],"later":["季度复盘"]}
  },
  "evidenceTriads": [{"claim":"ROI 1.6","evidence":"历史投放回收改善","url":"https://example.com/roi","cited":true}],
  "openQuestions": [],
  "sources": ["https://example.com/roi", "https://example.com/cpi"],
  "quant": {
    "enabled": true,
    "notes": "enabled",
    "scenarios": [
      {"name":"base","assumptions":[],"metrics":{"roi":1.6,"cpi":"$0.34","d7_retention":"18%"}}
    ]
  }
}
"#;

        let artifact = build_pm_report_artifact(Some(question), answer_text, None);
        assert_eq!(artifact.schema_version, "pm_report.v2");
        assert_eq!(
            artifact
                .report_json
                .get("schemaVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "pm_report.v2"
        );
        let metric_count = artifact
            .report_json
            .get("metricModel")
            .and_then(|v| v.get("coverage"))
            .and_then(|v| v.get("structuredMetricCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(metric_count >= 2);
        let section_order = artifact
            .report_json
            .get("reportStrategy")
            .and_then(|v| v.get("sectionOrder"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(!section_order.is_empty());
    }

    #[test]
    fn build_pm_report_artifact_does_not_emit_uncited_placeholder_triads_from_quality() {
        let question = "基于一手业务数据给出策略";
        let answer_text = "## 结论\n\n先基于一手数据做分层策略。";
        let quality = PmAnswerQualityDto {
            passed: false,
            deliverable: true,
            quality_level: "partial".to_string(),
            has_tool_calls: false,
            tool_call_count: 0,
            citation_count: 0,
            domain_count: 0,
            claim_count: 1,
            claim_alignment_ok: false,
            triad_total_claims: 1,
            triad_aligned_claims: 0,
            triad_coverage: 0.0,
            conflict_adjudicated: false,
            conflict_confidence: 0.35,
            conflict_reason: "no explicit conflict graph".to_string(),
            citations: Vec::new(),
            domains: Vec::new(),
            claim_alignment: vec![PmClaimEvidenceDto {
                claim: "Compare current MCP server options for internal product ops workflows."
                    .to_string(),
                evidence_excerpt: "missing evidence URL from tool outputs".to_string(),
                urls: Vec::new(),
                cited: false,
            }],
            evidence_tree: Vec::new(),
            conflict_matrix: Vec::new(),
            conflict_graph: PmConflictGraphDto {
                topic_count: 0,
                edge_count: 0,
                adjudicated_count: 0,
                unresolved_count: 0,
                avg_confidence: 0.0,
                edges: Vec::new(),
            },
            missing: Vec::new(),
            suggestions: Vec::new(),
        };

        let artifact = build_pm_report_artifact(Some(question), answer_text, Some(&quality));
        let triads = artifact
            .report_json
            .get("evidenceTriads")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let triad_text = serde_json::to_string(&triads).unwrap();

        assert!(!triad_text.contains("Compare current MCP server options"));
    }
}
