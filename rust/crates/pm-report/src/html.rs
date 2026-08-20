use super::{
    contains_cjk, extract_url_domain, is_pm_high_signal_source_url, normalize_http_url_candidate,
    pm_escape_html, truncate_for_log,
};

fn compact_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_noise_line(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    lower.contains("low_claim_evidence_alignment")
        || lower.contains("insufficient_claim_evidence_url_triads")
        || lower.contains("missing_tool_retrieval")
        || lower.contains("missing_citations")
        || lower.contains("contract_invalid:")
        || lower.contains("runtime error")
        || lower.contains("runtime execution failed")
        || lower.contains("runtime recovery failed")
        || lower.contains("retrieve source slot timed out")
        || lower.contains("timed out")
        || lower.contains("prompt:")
        || lower.contains("tool '")
        || lower.contains("webfetch")
}

fn clean_text(raw: &str, max_chars: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || is_noise_line(trimmed) {
        return None;
    }
    let mut cleaned = compact_ws(trimmed);
    if let Some(url) = normalize_http_url_candidate(&cleaned) {
        if cleaned == url {
            return None;
        }
    }
    cleaned = cleaned
        .trim_matches(|ch: char| {
            ch == '{'
                || ch == '}'
                || ch == '['
                || ch == ']'
                || ch == '('
                || ch == ')'
                || ch == '|'
                || ch == '-'
        })
        .trim()
        .to_string();
    if cleaned.is_empty() || is_noise_line(&cleaned) {
        return None;
    }
    Some(cleaned.chars().take(max_chars).collect())
}

fn strings_from_json(value: Option<&serde_json::Value>, max_items: usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    if let Some(cleaned) = clean_text(text, 240) {
                        if !out.iter().any(|existing: &String| existing == &cleaned) {
                            out.push(cleaned);
                        }
                    }
                }
                if out.len() >= max_items {
                    break;
                }
            }
        }
        serde_json::Value::String(text) => {
            if let Some(cleaned) = clean_text(text, 240) {
                out.push(cleaned);
            }
        }
        _ => {}
    }
    out
}

fn urls_from_json(value: Option<&serde_json::Value>, max_items: usize) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let push_url = |raw: &str, out: &mut Vec<String>| {
        if let Some(url) = normalize_http_url_candidate(raw) {
            if is_pm_high_signal_source_url(&url)
                && !out.iter().any(|existing: &String| existing == &url)
            {
                out.push(url);
            }
            return;
        }
        for url in super::extract_http_urls(raw) {
            if is_pm_high_signal_source_url(&url)
                && !out.iter().any(|existing: &String| existing == &url)
            {
                out.push(url);
            }
        }
    };
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    push_url(text, &mut out);
                }
                if out.len() >= max_items {
                    break;
                }
            }
        }
        serde_json::Value::String(text) => {
            push_url(text, &mut out);
        }
        _ => {}
    }
    out.truncate(max_items);
    out
}

fn render_list(items: &[String], empty_text: &str) -> String {
    if items.is_empty() {
        return format!("<li>{}</li>", pm_escape_html(empty_text));
    }
    let mut out = String::new();
    for item in items {
        out.push_str("<li>");
        out.push_str(&pm_escape_html(item));
        out.push_str("</li>");
    }
    out
}

fn extract_metric_value(text: &str) -> Option<String> {
    let mut best = String::new();
    for raw in text.split_whitespace() {
        let token = raw
            .trim_matches(|ch: char| {
                ch == ','
                    || ch == '.'
                    || ch == ';'
                    || ch == ':'
                    || ch == '，'
                    || ch == '。'
                    || ch == '：'
                    || ch == ')'
                    || ch == '('
            })
            .trim();
        if token.is_empty() || !token.chars().any(|ch| ch.is_ascii_digit()) {
            continue;
        }
        if token.len() > best.len() {
            best = token.to_string();
        }
    }
    if best.is_empty() {
        None
    } else {
        Some(best)
    }
}

fn build_kpis(lines: &[String], is_zh: bool) -> Vec<(String, String, String, String)> {
    let icons = ["📈", "💰", "👥", "🎯", "📱", "⚡"];
    let accents = ["blue", "orange", "green", "purple", "teal", "red"];
    let mut out = Vec::new();
    for line in lines {
        let Some(value) = extract_metric_value(line) else {
            continue;
        };
        let label = line.replacen(&value, "", 1).trim().to_string();
        let label = if label.is_empty() {
            line.clone()
        } else {
            label
        };
        if label.chars().count() < 2 {
            continue;
        }
        let key = format!("{}|{}", value, label);
        if out.iter().any(|(_, _, existing_value, existing_label)| {
            format!("{existing_value}|{existing_label}") == key
        }) {
            continue;
        }
        let idx = out.len();
        out.push((
            icons[idx % icons.len()].to_string(),
            accents[idx % accents.len()].to_string(),
            value,
            label.chars().take(if is_zh { 18 } else { 32 }).collect(),
        ));
        if out.len() >= 6 {
            break;
        }
    }
    out
}

fn section_labels(question_type: &str, is_zh: bool) -> (String, String, String, String, String) {
    if is_zh {
        return match question_type {
            "growth_monetization" => (
                "增长概览".to_string(),
                "关键杠杆".to_string(),
                "重点专题".to_string(),
                "行动路线".to_string(),
                "来源与附录".to_string(),
            ),
            "user_insight" => (
                "用户概览".to_string(),
                "行为洞察".to_string(),
                "重点专题".to_string(),
                "行动路线".to_string(),
                "来源与附录".to_string(),
            ),
            "policy_regulation" => (
                "政策概览".to_string(),
                "合规洞察".to_string(),
                "重点专题".to_string(),
                "行动路线".to_string(),
                "来源与附录".to_string(),
            ),
            "competitive_landscape" => (
                "格局概览".to_string(),
                "竞争洞察".to_string(),
                "重点专题".to_string(),
                "行动路线".to_string(),
                "来源与附录".to_string(),
            ),
            _ => (
                "市场概览".to_string(),
                "核心洞察".to_string(),
                "重点专题".to_string(),
                "行动路线".to_string(),
                "来源与附录".to_string(),
            ),
        };
    }
    match question_type {
        "growth_monetization" => (
            "Growth Overview".to_string(),
            "Key Levers".to_string(),
            "Deep Dives".to_string(),
            "Action Plan".to_string(),
            "Sources & Appendix".to_string(),
        ),
        "user_insight" => (
            "User Overview".to_string(),
            "Behavior Insights".to_string(),
            "Deep Dives".to_string(),
            "Action Plan".to_string(),
            "Sources & Appendix".to_string(),
        ),
        "policy_regulation" => (
            "Policy Overview".to_string(),
            "Compliance Insights".to_string(),
            "Deep Dives".to_string(),
            "Action Plan".to_string(),
            "Sources & Appendix".to_string(),
        ),
        "competitive_landscape" => (
            "Landscape Overview".to_string(),
            "Competitive Insights".to_string(),
            "Deep Dives".to_string(),
            "Action Plan".to_string(),
            "Sources & Appendix".to_string(),
        ),
        _ => (
            "Market Overview".to_string(),
            "Core Insights".to_string(),
            "Deep Dives".to_string(),
            "Action Plan".to_string(),
            "Sources & Appendix".to_string(),
        ),
    }
}

fn insight_titles(question_type: &str, is_zh: bool) -> (String, String, String) {
    if is_zh {
        return match question_type {
            "growth_monetization" => (
                "增长机会".to_string(),
                "漏斗瓶颈".to_string(),
                "变现杠杆".to_string(),
            ),
            "user_insight" => (
                "核心人群".to_string(),
                "行为阻塞".to_string(),
                "留存触发".to_string(),
            ),
            "policy_regulation" => (
                "合规机会".to_string(),
                "合规红线".to_string(),
                "政策风向".to_string(),
            ),
            "competitive_landscape" => (
                "可打穿空位".to_string(),
                "竞争壁垒".to_string(),
                "竞对动向".to_string(),
            ),
            _ => (
                "机会信号".to_string(),
                "风险信号".to_string(),
                "趋势信号".to_string(),
            ),
        };
    }
    match question_type {
        "growth_monetization" => (
            "Growth Opportunities".to_string(),
            "Funnel Bottlenecks".to_string(),
            "Monetization Levers".to_string(),
        ),
        "user_insight" => (
            "Core Segments".to_string(),
            "Behavior Friction".to_string(),
            "Retention Triggers".to_string(),
        ),
        "policy_regulation" => (
            "Compliance Opportunities".to_string(),
            "Compliance Red Lines".to_string(),
            "Policy Direction".to_string(),
        ),
        "competitive_landscape" => (
            "Whitespace Plays".to_string(),
            "Competitive Moats".to_string(),
            "Competitor Motion".to_string(),
        ),
        _ => (
            "Opportunity Signals".to_string(),
            "Risk Signals".to_string(),
            "Trend Signals".to_string(),
        ),
    }
}

fn strategy_layout(report_json: &serde_json::Value) -> String {
    report_json
        .get("reportStrategy")
        .and_then(|v| v.get("layout"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("balanced")
        .to_string()
}

fn default_section_order(layout: &str) -> Vec<String> {
    match layout {
        "metrics_first" => ["overview", "insights", "action", "deep", "sources"],
        "risk_first" => ["insights", "overview", "deep", "action", "sources"],
        "execution_first" => ["action", "overview", "insights", "deep", "sources"],
        _ => ["overview", "insights", "deep", "action", "sources"],
    }
    .iter()
    .map(|item| item.to_string())
    .collect()
}

fn strategy_section_order(report_json: &serde_json::Value, layout: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    if let Some(items) = report_json
        .get("reportStrategy")
        .and_then(|v| v.get("sectionOrder"))
        .and_then(|v| v.as_array())
    {
        for item in items {
            let Some(raw) = item.as_str() else {
                continue;
            };
            let normalized = raw.trim().to_ascii_lowercase();
            if !matches!(
                normalized.as_str(),
                "overview" | "insights" | "deep" | "action" | "sources"
            ) {
                continue;
            }
            if !out.iter().any(|existing| existing == &normalized) {
                out.push(normalized);
            }
        }
    }
    if out.is_empty() {
        return default_section_order(layout);
    }
    for required in ["overview", "insights", "deep", "action", "sources"] {
        if !out.iter().any(|item| item == required) {
            out.push(required.to_string());
        }
    }
    out
}

fn section_meta(
    section_id: &str,
    sec_overview: &str,
    sec_insight: &str,
    sec_deep: &str,
    sec_action: &str,
    sec_source: &str,
) -> (String, &'static str, &'static str) {
    match section_id {
        "overview" => (sec_overview.to_string(), "🌏", "#e3edf9"),
        "insights" => (sec_insight.to_string(), "📊", "#e8f8ef"),
        "deep" => (sec_deep.to_string(), "🧩", "#f3eaf9"),
        "action" => (sec_action.to_string(), "🛠️", "#fef0e6"),
        "sources" => (sec_source.to_string(), "🔗", "#e3f5f2"),
        _ => (sec_overview.to_string(), "📌", "#e3edf9"),
    }
}

fn metric_icon_and_accent(metric_key: &str) -> (&'static str, &'static str) {
    let key = metric_key.to_ascii_lowercase();
    if key.contains("roi") || key.contains("roas") {
        return ("📈", "green");
    }
    if key.contains("cpi") || key.contains("cpa") || key.contains("cpm") {
        return ("💸", "red");
    }
    if key.contains("retention") || key.contains("fill_rate") {
        return ("🔁", "teal");
    }
    if key.contains("revenue") || key.contains("arpu") || key.contains("ltv") {
        return ("💰", "orange");
    }
    if key.contains("user") || key.contains("mau") || key.contains("dau") {
        return ("👥", "blue");
    }
    ("📊", "purple")
}

fn build_kpis_from_metric_model(
    report_json: &serde_json::Value,
    is_zh: bool,
) -> Vec<(String, String, String, String)> {
    let mut out = Vec::<(String, String, String, String)>::new();
    let Some(metrics) = report_json
        .get("metricModel")
        .and_then(|v| v.get("metrics"))
        .and_then(|v| v.as_array())
    else {
        return out;
    };
    let mut seen = std::collections::HashSet::<String>::new();
    for metric in metrics.iter().take(10) {
        let Some(obj) = metric.as_object() else {
            continue;
        };
        let key = obj
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("metric")
            .trim()
            .to_string();
        let label = obj
            .get("label")
            .and_then(|v| v.as_str())
            .and_then(|raw| clean_text(raw, 64))
            .unwrap_or_else(|| {
                if is_zh {
                    "关键指标".to_string()
                } else {
                    "Key Metric".to_string()
                }
            });
        let display = obj
            .get("display")
            .and_then(|v| v.as_str())
            .and_then(|raw| clean_text(raw, 48))
            .or_else(|| {
                obj.get("value").and_then(|v| {
                    v.as_f64()
                        .map(|num| format!("{num:.2}"))
                        .or_else(|| v.as_i64().map(|num| num.to_string()))
                        .or_else(|| v.as_u64().map(|num| num.to_string()))
                })
            })
            .unwrap_or_else(|| "-".to_string());
        if display == "-" {
            continue;
        }
        let dedup_key = format!("{key}|{display}");
        if !seen.insert(dedup_key) {
            continue;
        }
        let (icon, accent) = metric_icon_and_accent(&key);
        out.push((
            icon.to_string(),
            accent.to_string(),
            display,
            label.chars().take(if is_zh { 18 } else { 32 }).collect(),
        ));
        if out.len() >= 6 {
            break;
        }
    }
    out
}

pub fn render_pm_report_html(report_json: &serde_json::Value) -> String {
    let question = report_json
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let question_type = report_json
        .get("questionType")
        .and_then(|v| v.as_str())
        .unwrap_or("general_research");
    let summary = clean_text(
        report_json
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        3200,
    )
    .unwrap_or_default();
    let highlights = strings_from_json(report_json.get("highlights"), 10);
    let sections = report_json
        .get("sections")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let confirmed = strings_from_json(sections.get("confirmed"), 10);
    let pending = strings_from_json(sections.get("pending"), 10);
    let risks = strings_from_json(sections.get("risks"), 10);
    let actions = strings_from_json(sections.get("actions"), 10);
    let deep_layers = report_json
        .get("deepResearchLayers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let breadth_scan = strings_from_json(deep_layers.get("breadthScan"), 10);
    let sources = urls_from_json(report_json.get("sources"), 32);
    let evidence = report_json
        .get("evidenceTriads")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let conflicts = report_json
        .get("conflictMatrix")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let action_plan = deep_layers
        .get("actionPlan")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let action_now = strings_from_json(action_plan.get("now"), 8);
    let action_next = strings_from_json(action_plan.get("next"), 8);
    let action_later = strings_from_json(action_plan.get("later"), 8);
    let quant_enabled = report_json
        .get("quant")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let strategy_layout = strategy_layout(report_json);
    let section_order = strategy_section_order(report_json, &strategy_layout);
    let strategy_confidence = report_json
        .get("reportStrategy")
        .and_then(|v| v.get("confidenceScore"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let strategy_density = report_json
        .get("reportStrategy")
        .and_then(|v| v.get("dataDensity"))
        .and_then(|v| v.as_str())
        .unwrap_or("low");

    let is_zh = contains_cjk(question)
        || contains_cjk(&summary)
        || highlights.iter().any(|item| contains_cjk(item))
        || confirmed.iter().any(|item| contains_cjk(item));
    let empty_text = if is_zh {
        "暂无可展示内容"
    } else {
        "No content available"
    };

    let title = if !question.is_empty() {
        if is_zh {
            format!("{}｜深度研究报告", question)
        } else {
            format!("{} | Deep Research Report", question)
        }
    } else if is_zh {
        "产运深度研究报告".to_string()
    } else {
        "PM Deep Research Report".to_string()
    };
    let subtitle = if summary.is_empty() {
        if is_zh {
            "基于跨来源信息整理可执行结论，优先支持业务决策。".to_string()
        } else {
            "Cross-source synthesis focused on actionable business decisions.".to_string()
        }
    } else {
        summary.clone()
    };
    let badge = if is_zh {
        "企业研究交付"
    } else {
        "Enterprise Research Delivery"
    };

    let (sec_overview, sec_insight, sec_deep, sec_action, sec_source) =
        section_labels(question_type, is_zh);
    let (opportunity_title, challenge_title, trend_title) = insight_titles(question_type, is_zh);

    let mut deep_cards = String::new();
    if let Some(items) = deep_layers
        .get("priorityDeepDives")
        .and_then(|v| v.as_array())
    {
        for item in items.iter().take(8) {
            let Some(obj) = item.as_object() else {
                continue;
            };
            let topic = clean_text(obj.get("topic").and_then(|v| v.as_str()).unwrap_or(""), 220)
                .unwrap_or_else(|| if is_zh { "关键主题" } else { "Key Theme" }.to_string());
            let insights = strings_from_json(obj.get("insights"), 5);
            let implication = clean_text(
                obj.get("implication")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                260,
            )
            .unwrap_or_default();
            let urls = urls_from_json(obj.get("evidenceUrls"), 4);
            let mut chips = String::new();
            for url in urls {
                let label = extract_url_domain(&url).unwrap_or_else(|| truncate_for_log(&url, 36));
                chips.push_str("<a class=\"chip\" href=\"");
                chips.push_str(&pm_escape_html(&url));
                chips.push_str("\" target=\"_blank\" rel=\"noreferrer\">");
                chips.push_str(&pm_escape_html(&label));
                chips.push_str("</a>");
            }
            deep_cards.push_str("<article class=\"card deep-card\"><h3>");
            deep_cards.push_str(&pm_escape_html(&topic));
            deep_cards.push_str("</h3><ul>");
            deep_cards.push_str(&render_list(&insights, empty_text));
            deep_cards.push_str("</ul>");
            if !implication.is_empty() {
                deep_cards.push_str("<p class=\"implication\">");
                deep_cards.push_str(&pm_escape_html(&implication));
                deep_cards.push_str("</p>");
            }
            if !chips.is_empty() {
                deep_cards.push_str("<div class=\"chip-row\">");
                deep_cards.push_str(&chips);
                deep_cards.push_str("</div>");
            }
            deep_cards.push_str("</article>");
        }
    }
    if deep_cards.is_empty() {
        for item in confirmed.iter().take(4) {
            deep_cards.push_str("<article class=\"card deep-card\"><h3>");
            deep_cards.push_str(&pm_escape_html(item));
            deep_cards.push_str("</h3></article>");
        }
    }
    if deep_cards.is_empty() {
        deep_cards.push_str("<article class=\"card deep-card\"><h3>");
        deep_cards.push_str(&pm_escape_html(empty_text));
        deep_cards.push_str("</h3></article>");
    }

    let opportunities = {
        let mut merged = confirmed.clone();
        merged.extend(actions.clone());
        merged.truncate(8);
        merged
    };
    let challenges = {
        let mut merged = risks.clone();
        merged.extend(pending.clone());
        merged.truncate(8);
        merged
    };
    let trends = {
        let mut merged = breadth_scan.clone();
        merged.extend(highlights.clone());
        merged.truncate(8);
        merged
    };

    let mut kpis = build_kpis_from_metric_model(report_json, is_zh);
    if kpis.is_empty() {
        let mut kpi_candidates = Vec::new();
        kpi_candidates.extend(highlights.iter().cloned());
        kpi_candidates.extend(breadth_scan.iter().cloned());
        kpi_candidates.extend(confirmed.iter().cloned());
        kpis = build_kpis(&kpi_candidates, is_zh);
    }
    let mut kpi_cards = String::new();
    for (icon, accent, value, label) in kpis {
        kpi_cards.push_str("<article class=\"kpi-card ");
        kpi_cards.push_str(&accent);
        kpi_cards.push_str("\"><div class=\"kpi-icon\">");
        kpi_cards.push_str(&pm_escape_html(&icon));
        kpi_cards.push_str("</div><div class=\"kpi-value\">");
        kpi_cards.push_str(&pm_escape_html(&value));
        kpi_cards.push_str("</div><div class=\"kpi-label\">");
        kpi_cards.push_str(&pm_escape_html(&label));
        kpi_cards.push_str("</div></article>");
    }
    if kpi_cards.is_empty() {
        kpi_cards.push_str("<article class=\"kpi-card blue\"><div class=\"kpi-icon\">📌</div><div class=\"kpi-value\">");
        kpi_cards.push_str(&pm_escape_html(&format!(
            "{}",
            highlights.len().max(confirmed.len())
        )));
        kpi_cards.push_str("</div><div class=\"kpi-label\">");
        kpi_cards.push_str(&pm_escape_html(if is_zh {
            "可执行关键结论"
        } else {
            "Actionable key findings"
        }));
        kpi_cards.push_str("</div></article>");
    }

    let source_count = sources.len();
    let action_count = action_now.len() + action_next.len() + action_later.len();
    let deep_count = deep_layers
        .get("priorityDeepDives")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0)
        .max(confirmed.len());
    let structured_metric_count = report_json
        .get("metricModel")
        .and_then(|v| v.get("coverage"))
        .and_then(|v| v.get("structuredMetricCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let density_label = match strategy_density {
        "high" => {
            if is_zh {
                "高密度"
            } else {
                "High Density"
            }
        }
        "medium" => {
            if is_zh {
                "中密度"
            } else {
                "Medium Density"
            }
        }
        _ => {
            if is_zh {
                "低密度"
            } else {
                "Low Density"
            }
        }
    };
    let confidence_value = format!("{}%", (strategy_confidence * 100.0).round() as i64);
    let metric_stat_value = if structured_metric_count > 0 {
        structured_metric_count
    } else {
        deep_count
    };
    let metric_stat_label = if structured_metric_count > 0 {
        if is_zh {
            "结构化指标"
        } else {
            "Structured Metrics"
        }
    } else if is_zh {
        "重点专题数"
    } else {
        "Deep-dive Topics"
    };
    let confidence_label = if quant_enabled {
        if is_zh {
            format!("证据置信 · {} · Quant开", density_label)
        } else {
            format!("Confidence · {} · Quant On", density_label)
        }
    } else if is_zh {
        format!("证据置信 · {} · Quant关", density_label)
    } else {
        format!("Confidence · {} · Quant Off", density_label)
    };

    let mut source_chips = String::new();
    for url in &sources {
        let label = extract_url_domain(url).unwrap_or_else(|| truncate_for_log(url, 42));
        source_chips.push_str("<a class=\"chip\" href=\"");
        source_chips.push_str(&pm_escape_html(url));
        source_chips.push_str("\" target=\"_blank\" rel=\"noreferrer\">");
        source_chips.push_str(&pm_escape_html(&label));
        source_chips.push_str("</a>");
    }
    if source_chips.is_empty() {
        source_chips.push_str("<span class=\"muted\">");
        source_chips.push_str(&pm_escape_html(if is_zh {
            "暂无来源链接"
        } else {
            "No source links available"
        }));
        source_chips.push_str("</span>");
    }

    let mut evidence_rows = String::new();
    for row in evidence.iter().take(24) {
        let claim = clean_text(row.get("claim").and_then(|v| v.as_str()).unwrap_or(""), 220)
            .unwrap_or_else(|| "-".to_string());
        let evidence_excerpt = clean_text(
            row.get("evidence").and_then(|v| v.as_str()).unwrap_or(""),
            260,
        )
        .unwrap_or_else(|| "-".to_string());
        let url = row
            .get("url")
            .and_then(|v| v.as_str())
            .and_then(normalize_http_url_candidate)
            .filter(|url| is_pm_high_signal_source_url(url))
            .unwrap_or_default();
        evidence_rows.push_str("<tr><td>");
        evidence_rows.push_str(&pm_escape_html(&claim));
        evidence_rows.push_str("</td><td>");
        evidence_rows.push_str(&pm_escape_html(&evidence_excerpt));
        evidence_rows.push_str("</td><td>");
        if url.is_empty() {
            evidence_rows.push('-');
        } else {
            let label = extract_url_domain(&url).unwrap_or_else(|| truncate_for_log(&url, 36));
            evidence_rows.push_str("<a href=\"");
            evidence_rows.push_str(&pm_escape_html(&url));
            evidence_rows.push_str("\" target=\"_blank\" rel=\"noreferrer\">");
            evidence_rows.push_str(&pm_escape_html(&label));
            evidence_rows.push_str("</a>");
        }
        evidence_rows.push_str("</td></tr>");
    }
    if evidence_rows.is_empty() {
        evidence_rows.push_str("<tr><td colspan=\"3\">");
        evidence_rows.push_str(&pm_escape_html(empty_text));
        evidence_rows.push_str("</td></tr>");
    }

    let mut conflict_rows = String::new();
    for row in conflicts.iter().take(12) {
        let topic = clean_text(row.get("topic").and_then(|v| v.as_str()).unwrap_or(""), 140)
            .unwrap_or_else(|| "-".to_string());
        let claim_a = clean_text(
            row.get("claimA").and_then(|v| v.as_str()).unwrap_or(""),
            200,
        )
        .unwrap_or_else(|| "-".to_string());
        let claim_b = clean_text(
            row.get("claimB").and_then(|v| v.as_str()).unwrap_or(""),
            200,
        )
        .unwrap_or_else(|| "-".to_string());
        let verdict = clean_text(
            row.get("verdict").and_then(|v| v.as_str()).unwrap_or(""),
            140,
        )
        .unwrap_or_else(|| "-".to_string());
        conflict_rows.push_str("<tr><td>");
        conflict_rows.push_str(&pm_escape_html(&topic));
        conflict_rows.push_str("</td><td>");
        conflict_rows.push_str(&pm_escape_html(&claim_a));
        conflict_rows.push_str("</td><td>");
        conflict_rows.push_str(&pm_escape_html(&claim_b));
        conflict_rows.push_str("</td><td>");
        conflict_rows.push_str(&pm_escape_html(&verdict));
        conflict_rows.push_str("</td></tr>");
    }
    if conflict_rows.is_empty() {
        conflict_rows.push_str("<tr><td colspan=\"4\">");
        conflict_rows.push_str(&pm_escape_html(if is_zh {
            "暂无冲突项"
        } else {
            "No conflict rows"
        }));
        conflict_rows.push_str("</td></tr>");
    }

    let summary_text = pm_escape_html(if summary.is_empty() {
        empty_text
    } else {
        &summary
    });
    let overview_meta = if structured_metric_count > 0 {
        format!(
            "<p class=\"muted\">{}: {} · {}: {} · {}: {}</p>",
            pm_escape_html(if is_zh {
                "结构化指标"
            } else {
                "Structured Metrics"
            }),
            structured_metric_count,
            pm_escape_html(if is_zh { "布局策略" } else { "Layout" }),
            pm_escape_html(&strategy_layout),
            pm_escape_html(if is_zh {
                "数据密度"
            } else {
                "Data Density"
            }),
            pm_escape_html(density_label),
        )
    } else {
        String::new()
    };
    let overview_section = format!(
        "<section class=\"fade-in\" id=\"overview\"><div class=\"section-header\"><div class=\"section-icon\" style=\"background:#e3edf9;\">🌏</div><h2>{}</h2></div><article class=\"card\"><p>{}</p>{}<div class=\"kpi-grid\">{}</div></article></section>",
        pm_escape_html(&sec_overview),
        summary_text,
        overview_meta,
        kpi_cards
    );
    let insights_section = format!(
        "<section class=\"fade-in\" id=\"insights\"><div class=\"section-header\"><div class=\"section-icon\" style=\"background:#e8f8ef;\">📊</div><h2>{}</h2></div><div class=\"insight-grid\"><article class=\"insight-card opportunity\"><h3>{}</h3><ul>{}</ul></article><article class=\"insight-card challenge\"><h3>{}</h3><ul>{}</ul></article><article class=\"insight-card trend\"><h3>{}</h3><ul>{}</ul></article></div></section>",
        pm_escape_html(&sec_insight),
        pm_escape_html(&opportunity_title),
        render_list(&opportunities, empty_text),
        pm_escape_html(&challenge_title),
        render_list(&challenges, empty_text),
        pm_escape_html(&trend_title),
        render_list(&trends, empty_text),
    );
    let deep_section = format!(
        "<section class=\"fade-in\" id=\"deep\"><div class=\"section-header\"><div class=\"section-icon\" style=\"background:#f3eaf9;\">🧩</div><h2>{}</h2></div><div class=\"deep-grid\">{}</div></section>",
        pm_escape_html(&sec_deep),
        deep_cards
    );
    let action_section = format!(
        "<section class=\"fade-in\" id=\"action\"><div class=\"section-header\"><div class=\"section-icon\" style=\"background:#fef0e6;\">🛠️</div><h2>{}</h2></div><article class=\"card\"><div class=\"plan-grid\"><div class=\"plan-block\"><h3>{}</h3><ul>{}</ul></div><div class=\"plan-block\"><h3>{}</h3><ul>{}</ul></div><div class=\"plan-block\"><h3>{}</h3><ul>{}</ul></div></div></article></section>",
        pm_escape_html(&sec_action),
        pm_escape_html(if is_zh { "Now（0-72h）" } else { "Now (0-72h)" }),
        render_list(&action_now, empty_text),
        pm_escape_html(if is_zh {
            "Next（1-2周）"
        } else {
            "Next (1-2 weeks)"
        }),
        render_list(&action_next, empty_text),
        pm_escape_html(if is_zh { "Later（季度）" } else { "Later (quarter)" }),
        render_list(&action_later, empty_text),
    );
    let sources_section = format!(
        "<section class=\"fade-in\" id=\"sources\"><div class=\"section-header\"><div class=\"section-icon\" style=\"background:#e3f5f2;\">🔗</div><h2>{}</h2></div><article class=\"card\"><div class=\"chip-row\">{}</div><div class=\"appendix\"><details><summary>{}</summary><div class=\"table-wrap\"><table><thead><tr><th>Claim</th><th>Evidence</th><th>URL</th></tr></thead><tbody>{}</tbody></table></div></details><details><summary>{}</summary><div class=\"table-wrap\"><table><thead><tr><th>Topic</th><th>Claim A</th><th>Claim B</th><th>Verdict</th></tr></thead><tbody>{}</tbody></table></div></details></div></article></section>",
        pm_escape_html(&sec_source),
        source_chips,
        pm_escape_html(if is_zh {
            "证据明细（可展开）"
        } else {
            "Evidence Details (expand)"
        }),
        evidence_rows,
        pm_escape_html(if is_zh {
            "冲突裁决（可展开）"
        } else {
            "Conflict Adjudication (expand)"
        }),
        conflict_rows,
    );
    let mut nav_links = String::new();
    let mut main_sections = String::new();
    for (index, section_id) in section_order.iter().enumerate() {
        let (label, _, _) = section_meta(
            section_id,
            &sec_overview,
            &sec_insight,
            &sec_deep,
            &sec_action,
            &sec_source,
        );
        nav_links.push_str("<a href=\"#");
        nav_links.push_str(section_id);
        nav_links.push('"');
        if index == 0 {
            nav_links.push_str(" class=\"active\"");
        }
        nav_links.push('>');
        nav_links.push_str(&pm_escape_html(&label));
        nav_links.push_str("</a>");

        let section_html = match section_id.as_str() {
            "overview" => overview_section.as_str(),
            "insights" => insights_section.as_str(),
            "deep" => deep_section.as_str(),
            "action" => action_section.as_str(),
            "sources" => sources_section.as_str(),
            _ => "",
        };
        if section_html.is_empty() {
            continue;
        }
        main_sections.push_str(section_html);
    }

    format!(
        r##"<!DOCTYPE html>
<html lang="{lang}">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>{title}</title>
  <style>
    :root {{
      --primary: #1a3a5c;
      --accent: #e8500a;
      --accent2: #f5a623;
      --bg: #f7f9fc;
      --card: #ffffff;
      --text: #1e2d3d;
      --muted: #6b7c93;
      --border: #dde3ec;
      --green: #27ae60;
      --red: #e74c3c;
      --purple: #8e44ad;
      --teal: #16a085;
    }}
    * {{ box-sizing: border-box; margin: 0; padding: 0; }}
    body {{
      font-family: "Segoe UI", "PingFang SC", "Hiragino Sans GB", sans-serif;
      background: var(--bg);
      color: var(--text);
      line-height: 1.7;
    }}
    header {{
      background: linear-gradient(135deg, #0d2137 0%, #1a3a5c 50%, #1e5799 100%);
      color: #fff;
      padding: 54px 24px 42px;
      position: relative;
      overflow: hidden;
    }}
    header::before {{
      content: "";
      position: absolute;
      top: -80px;
      right: -80px;
      width: 380px;
      height: 380px;
      border-radius: 50%;
      background: radial-gradient(circle, rgba(232,80,10,0.26) 0%, transparent 70%);
    }}
    .header-inner {{
      max-width: 1180px;
      margin: 0 auto;
      position: relative;
      z-index: 1;
    }}
    .header-badge {{
      display: inline-block;
      background: rgba(232, 80, 10, 0.9);
      padding: 4px 12px;
      border-radius: 18px;
      font-size: 12px;
      font-weight: 700;
      letter-spacing: 1.2px;
      margin-bottom: 14px;
    }}
    h1 {{
      font-size: clamp(24px, 3.8vw, 42px);
      line-height: 1.25;
      margin-bottom: 10px;
    }}
    .subtitle {{
      max-width: 880px;
      color: rgba(255, 255, 255, 0.84);
      font-size: 15px;
      margin-bottom: 18px;
    }}
    .header-stats {{
      display: flex;
      gap: 14px;
      flex-wrap: wrap;
    }}
    .header-stat {{
      min-width: 180px;
      background: rgba(255, 255, 255, 0.1);
      border: 1px solid rgba(255, 255, 255, 0.2);
      border-radius: 12px;
      padding: 10px 14px;
      backdrop-filter: blur(10px);
    }}
    .header-stat .val {{
      font-size: 23px;
      font-weight: 800;
      color: var(--accent2);
      line-height: 1.2;
    }}
    .header-stat .lbl {{
      font-size: 12px;
      color: rgba(255, 255, 255, 0.72);
      margin-top: 3px;
    }}
    nav {{
      position: sticky;
      top: 0;
      background: #fff;
      border-bottom: 2px solid var(--border);
      box-shadow: 0 2px 12px rgba(0, 0, 0, 0.06);
      z-index: 30;
    }}
    .nav-inner {{
      max-width: 1180px;
      margin: 0 auto;
      display: flex;
      overflow-x: auto;
      scrollbar-width: none;
    }}
    .nav-inner::-webkit-scrollbar {{ display: none; }}
    .nav-inner a {{
      text-decoration: none;
      color: var(--muted);
      font-size: 14px;
      font-weight: 600;
      padding: 14px 20px;
      border-bottom: 3px solid transparent;
      white-space: nowrap;
    }}
    .nav-inner a:hover,
    .nav-inner a.active {{
      color: var(--accent);
      border-bottom-color: var(--accent);
    }}
    main {{
      max-width: 1180px;
      margin: 0 auto;
      padding: 34px 16px 72px;
    }}
    section {{
      margin-bottom: 44px;
    }}
    .section-header {{
      display: flex;
      align-items: center;
      gap: 12px;
      margin-bottom: 20px;
    }}
    .section-icon {{
      width: 44px;
      height: 44px;
      border-radius: 12px;
      display: flex;
      align-items: center;
      justify-content: center;
      font-size: 21px;
      flex-shrink: 0;
    }}
    .section-header h2 {{
      font-size: 24px;
      line-height: 1.2;
      color: var(--primary);
    }}
    .card {{
      background: var(--card);
      border-radius: 16px;
      border: 1px solid var(--border);
      box-shadow: 0 2px 16px rgba(0, 0, 0, 0.06);
      padding: 24px;
    }}
    .card + .card {{ margin-top: 16px; }}
    .kpi-grid {{
      margin-top: 18px;
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
      gap: 14px;
    }}
    .kpi-card {{
      border-radius: 14px;
      border: 1px solid var(--border);
      padding: 18px;
      background: #fff;
      position: relative;
      overflow: hidden;
    }}
    .kpi-card::before {{
      content: "";
      position: absolute;
      left: 0;
      right: 0;
      top: 0;
      height: 4px;
    }}
    .kpi-card.blue::before {{ background: linear-gradient(90deg, #1a3a5c, #1e5799); }}
    .kpi-card.orange::before {{ background: linear-gradient(90deg, #e8500a, #f5a623); }}
    .kpi-card.green::before {{ background: linear-gradient(90deg, #27ae60, #2ecc71); }}
    .kpi-card.purple::before {{ background: linear-gradient(90deg, #8e44ad, #9b59b6); }}
    .kpi-card.teal::before {{ background: linear-gradient(90deg, #16a085, #1abc9c); }}
    .kpi-card.red::before {{ background: linear-gradient(90deg, #c0392b, #e74c3c); }}
    .kpi-icon {{ font-size: 24px; margin-bottom: 8px; }}
    .kpi-value {{ font-size: 28px; font-weight: 800; line-height: 1.1; color: var(--primary); }}
    .kpi-label {{ margin-top: 5px; font-size: 13px; color: var(--muted); }}
    .insight-grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
      gap: 14px;
    }}
    .insight-card {{
      border-radius: 14px;
      padding: 18px;
      border-left: 5px solid;
    }}
    .insight-card.opportunity {{
      background: #e8f8ef;
      border-color: var(--green);
    }}
    .insight-card.challenge {{
      background: #fdecea;
      border-color: var(--red);
    }}
    .insight-card.trend {{
      background: #e3edf9;
      border-color: var(--primary);
    }}
    .insight-card h3 {{
      font-size: 15px;
      font-weight: 800;
      margin-bottom: 8px;
    }}
    .insight-card ul {{
      margin-left: 18px;
      display: grid;
      gap: 6px;
      font-size: 13px;
    }}
    .deep-grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
      gap: 14px;
    }}
    .deep-card h3 {{
      font-size: 17px;
      color: var(--primary);
      margin-bottom: 8px;
    }}
    .deep-card ul {{
      margin-left: 18px;
      display: grid;
      gap: 6px;
      font-size: 13px;
    }}
    .implication {{
      margin-top: 10px;
      font-size: 13px;
      color: #1f2937;
      border-left: 3px solid #2563eb;
      background: #eef5ff;
      border-radius: 8px;
      padding: 7px 10px;
    }}
    .chip-row {{
      margin-top: 10px;
      display: flex;
      gap: 6px;
      flex-wrap: wrap;
    }}
    .chip {{
      display: inline-flex;
      border-radius: 999px;
      border: 1px solid #c7d5e6;
      background: #fff;
      color: #0f4c81;
      text-decoration: none;
      padding: 2px 8px;
      font-size: 12px;
      max-width: 100%;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }}
    .plan-grid {{
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 14px;
    }}
    .plan-block {{
      border: 1px solid var(--border);
      border-radius: 12px;
      padding: 14px;
      background: #fff;
    }}
    .plan-block h3 {{
      font-size: 15px;
      color: var(--primary);
      margin-bottom: 8px;
    }}
    .plan-block ul {{
      margin-left: 18px;
      display: grid;
      gap: 6px;
      font-size: 13px;
    }}
    .appendix {{
      margin-top: 14px;
      border-top: 1px solid var(--border);
      padding-top: 12px;
      display: grid;
      gap: 10px;
    }}
    details {{
      border: 1px solid var(--border);
      border-radius: 12px;
      background: #fff;
      overflow: hidden;
    }}
    summary {{
      cursor: pointer;
      padding: 10px 12px;
      font-weight: 700;
      color: var(--primary);
      background: #f7f9fc;
    }}
    .table-wrap {{ overflow-x: auto; padding: 0 8px 10px; }}
    table {{
      width: 100%;
      border-collapse: collapse;
      font-size: 13px;
      min-width: 680px;
    }}
    th, td {{
      border-bottom: 1px solid #e8edf2;
      padding: 8px 10px;
      text-align: left;
      vertical-align: top;
    }}
    th {{
      background: #f0f4f9;
      color: #32455d;
      font-weight: 700;
    }}
    .muted {{ color: var(--muted); font-size: 12px; }}
    .fade-in {{
      opacity: 0;
      transform: translateY(18px);
      transition: opacity .45s ease, transform .45s ease;
    }}
    .fade-in.visible {{
      opacity: 1;
      transform: translateY(0);
    }}
    @media (max-width: 880px) {{
      .plan-grid {{ grid-template-columns: 1fr; }}
      .deep-grid {{ grid-template-columns: 1fr; }}
      .insight-grid {{ grid-template-columns: 1fr; }}
      main {{ padding: 22px 12px 56px; }}
      header {{ padding: 42px 16px 34px; }}
    }}
  </style>
</head>
<body>
  <header>
    <div class="header-inner">
      <span class="header-badge">{badge}</span>
      <h1>{title}</h1>
      <p class="subtitle">{subtitle}</p>
      <div class="header-stats">
        <div class="header-stat"><div class="val">{source_count}</div><div class="lbl">{sources_label}</div></div>
        <div class="header-stat"><div class="val">{metric_stat_value}</div><div class="lbl">{metric_stat_label}</div></div>
        <div class="header-stat"><div class="val">{action_count}</div><div class="lbl">{actions_label}</div></div>
        <div class="header-stat"><div class="val">{confidence_value}</div><div class="lbl">{confidence_label}</div></div>
      </div>
    </div>
  </header>

  <nav>
    <div class="nav-inner">
      {nav_links}
    </div>
  </nav>

  <main>
    {main_sections}
  </main>

  <script>
    const sections = document.querySelectorAll("section[id]");
    const navLinks = document.querySelectorAll(".nav-inner a");
    window.addEventListener("scroll", () => {{
      let current = "";
      sections.forEach((section) => {{
        if (window.scrollY >= section.offsetTop - 120) current = section.id;
      }});
      navLinks.forEach((link) => {{
        link.classList.toggle("active", link.getAttribute("href") === "#" + current);
      }});
    }});
    const observer = new IntersectionObserver((entries) => {{
      entries.forEach((entry) => {{
        if (entry.isIntersecting) entry.target.classList.add("visible");
      }});
    }}, {{ threshold: 0.1 }});
    document.querySelectorAll(".fade-in").forEach((el) => observer.observe(el));
  </script>
</body>
</html>"##,
        lang = if is_zh { "zh-CN" } else { "en" },
        badge = pm_escape_html(badge),
        title = pm_escape_html(&title),
        subtitle = pm_escape_html(&subtitle),
        source_count = source_count,
        metric_stat_value = metric_stat_value,
        action_count = action_count,
        sources_label = pm_escape_html(if is_zh {
            "来源数量"
        } else {
            "Source Links"
        }),
        metric_stat_label = pm_escape_html(metric_stat_label),
        actions_label = pm_escape_html(if is_zh {
            "行动项数量"
        } else {
            "Action Items"
        }),
        confidence_value = pm_escape_html(&confidence_value),
        confidence_label = pm_escape_html(&confidence_label),
        nav_links = nav_links,
        main_sections = main_sections,
    )
}
