use super::*;

pub(super) fn build_pm_preface_fallback(
    original_question: &str,
    plan: &serde_json::Value,
) -> String {
    let query_variants: Vec<String> = plan
        .get("queryVariants")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let routes: Vec<String> = plan
        .get("sourceRoutes")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .filter_map(|item| item.get("routeId").and_then(|v| v.as_str()))
                .take(4)
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if contains_cjk(original_question) {
        let route_text = if routes.is_empty() {
            "通用检索".to_string()
        } else {
            routes.join("、")
        };
        let variant_text = if query_variants.is_empty() {
            original_question.trim().to_string()
        } else {
            query_variants
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" / ")
        };
        return format!(
            "我将围绕你的目标开展多源研究，并优先检索可量化证据与可追溯来源。\n\
1. 明确研究边界与关键问题\n\
2. 按路线抓取来源（{route_text}）\n\
3. 围绕核心查询变体检索（{variant_text}）\n\
4. 交叉校验冲突信息并输出结论与建议"
        );
    }
    let route_text = if routes.is_empty() {
        "general web retrieval".to_string()
    } else {
        routes.join(", ")
    };
    let variant_text = if query_variants.is_empty() {
        original_question.trim().to_string()
    } else {
        query_variants
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" / ")
    };
    format!(
        "I will run a multi-source research pass and prioritize quantifiable, traceable evidence.\n\
1. Clarify scope and key questions\n\
2. Retrieve from prioritized routes ({route_text})\n\
3. Search core query variants ({variant_text})\n\
4. Cross-check conflicts and produce actionable recommendations"
    )
}

pub(super) fn push_pm_emergency_url(out: &mut Vec<String>, raw: &str) {
    if let Some(candidate) = normalize_http_url_candidate(raw) {
        if is_pm_high_signal_source_url(&candidate) {
            out.push(candidate);
        }
    }
}

fn collect_pm_emergency_evidence_urls(
    tool_summary: Option<&serde_json::Value>,
    quality: Option<&PmAnswerQualityDto>,
    probe_outcomes: Option<&[PmProbeOutcome]>,
) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();

    if let Some(quality) = quality {
        for url in quality.citations.iter().take(16) {
            push_pm_emergency_url(&mut urls, url);
        }
        for row in quality.claim_alignment.iter().take(16) {
            for url in row.urls.iter().take(3) {
                push_pm_emergency_url(&mut urls, url);
            }
        }
        for node in quality.evidence_tree.iter().take(12) {
            for leaf in node.evidences.iter().take(2) {
                push_pm_emergency_url(&mut urls, &leaf.url);
            }
        }
        for edge in quality.conflict_graph.edges.iter().take(12) {
            for url in edge.urls.iter().take(2) {
                push_pm_emergency_url(&mut urls, url);
            }
        }
    }

    if let Some(outcomes) = probe_outcomes {
        for outcome in outcomes.iter().take(16) {
            if let Some(quality) = &outcome.quality {
                for url in quality.citations.iter().take(2) {
                    push_pm_emergency_url(&mut urls, url);
                }
                for row in quality.claim_alignment.iter().take(4) {
                    for url in row.urls.iter().take(2) {
                        push_pm_emergency_url(&mut urls, url);
                    }
                }
            }
            if let Some(turn) = &outcome.turn {
                for hit in build_pm_tool_evidence_hits(&turn.tool_calls)
                    .into_iter()
                    .take(6)
                {
                    push_pm_emergency_url(&mut urls, &hit.url);
                }
            }
        }
    }

    if let Some(summary) = tool_summary {
        if let Some(summary_urls) = summary.get("urls").and_then(|value| value.as_array()) {
            for url in summary_urls
                .iter()
                .take(16)
                .filter_map(|value| value.as_str())
            {
                push_pm_emergency_url(&mut urls, url);
            }
        }
        if let Some(samples) = summary.get("samples").and_then(|value| value.as_array()) {
            for sample in samples.iter().take(16) {
                if let Some(sample_urls) = sample.get("urls").and_then(|value| value.as_array()) {
                    for url in sample_urls
                        .iter()
                        .take(4)
                        .filter_map(|value| value.as_str())
                    {
                        push_pm_emergency_url(&mut urls, url);
                    }
                }
                if let Some(text) = sample.get("input").and_then(|value| value.as_str()) {
                    for url in extract_http_urls(text).into_iter().take(3) {
                        push_pm_emergency_url(&mut urls, &url);
                    }
                }
                if let Some(text) = sample.get("output").and_then(|value| value.as_str()) {
                    for url in extract_http_urls(text).into_iter().take(3) {
                        push_pm_emergency_url(&mut urls, &url);
                    }
                }
            }
        }
    }

    let mut seen = HashSet::<String>::new();
    let mut deduped: Vec<String> = Vec::new();
    for url in urls {
        if seen.insert(url.clone()) {
            deduped.push(url);
        }
        if deduped.len() >= 8 {
            break;
        }
    }
    deduped
}

fn pm_emergency_compact_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pm_emergency_clean_label(raw: &str, max_chars: usize) -> Option<String> {
    let value = pm_emergency_compact_ws(raw);
    let lower = value.to_ascii_lowercase();
    if value.trim().is_empty()
        || lower.contains("runtime execution failed")
        || lower.contains("detected first-party evidence")
        || lower.contains("durationms")
        || lower.contains("toolcallcount")
        || lower.contains("+1 more")
        || lower.contains("+2 more")
        || lower.contains("+3 more")
        || value.contains("...")
        || value.contains('…')
    {
        return None;
    }
    let mut value = value.chars().take(max_chars).collect::<String>();
    value = value
        .trim_matches(|ch: char| ch.is_ascii_punctuation() || matches!(ch, '：' | ':' | '，'))
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn pm_emergency_push_unique(out: &mut Vec<String>, raw: impl AsRef<str>, max_chars: usize) {
    let Some(value) = pm_emergency_clean_label(raw.as_ref(), max_chars) else {
        return;
    };
    if !out.iter().any(|item| item.eq_ignore_ascii_case(&value)) {
        out.push(value);
    }
}

fn pm_emergency_first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(serde_json::Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

fn pm_emergency_collect_first_party_labels(
    evidence: &serde_json::Value,
    key: &str,
    cap: usize,
    max_chars: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(items) = evidence.get(key).and_then(serde_json::Value::as_array) else {
        return out;
    };
    for item in items.iter().take(cap.saturating_mul(2).max(cap)) {
        if let Some(text) = item.as_str() {
            pm_emergency_push_unique(&mut out, text, max_chars);
        } else if key == "metrics" {
            if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
                let value = item
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if value.trim().is_empty() {
                    pm_emergency_push_unique(&mut out, name, max_chars);
                } else {
                    pm_emergency_push_unique(&mut out, format!("{name}={value}"), max_chars);
                }
            }
        } else if let Some(primary) =
            pm_emergency_first_string(item, &["cohort", "name", "title", "label", "text"])
        {
            let secondary =
                pm_emergency_first_string(item, &["why", "lesson", "strategyHint", "reason"]);
            let label = if let Some(secondary) = secondary {
                format!("{primary}: {secondary}")
            } else {
                primary
            };
            pm_emergency_push_unique(&mut out, label, max_chars);
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

pub(super) fn build_pm_emergency_conclusion_text(
    question: &str,
    _reason: &str,
    _attempt: usize,
    tool_summary: Option<&serde_json::Value>,
    quality: Option<&PmAnswerQualityDto>,
    probe_outcomes: Option<&[PmProbeOutcome]>,
) -> String {
    let evidence_urls = collect_pm_emergency_evidence_urls(tool_summary, quality, probe_outcomes);
    let triad_aligned = quality.map(|q| q.triad_aligned_claims).unwrap_or(0);
    let citation_count = quality.map(|q| q.citation_count).unwrap_or(0);
    let confirmed_count = triad_aligned.max(citation_count).max(evidence_urls.len());
    let evidence_ready = confirmed_count > 0 && !evidence_urls.is_empty();
    let include_origin_marker = pm_flag_enabled("PM_VISIBLE_ANSWER_ORIGIN_MARKER", false);
    let first_party = extract_pm_first_party_evidence(question);
    let metrics = pm_emergency_collect_first_party_labels(&first_party, "metrics", 8, 80);
    let objectives = pm_emergency_collect_first_party_labels(&first_party, "objectives", 5, 100);
    let guardrails = pm_emergency_collect_first_party_labels(&first_party, "guardrails", 5, 120);
    let cohorts =
        pm_emergency_collect_first_party_labels(&first_party, "opportunityCohorts", 4, 140);
    let existing =
        pm_emergency_collect_first_party_labels(&first_party, "existingMechanics", 4, 120);
    if contains_cjk(question) {
        let mut basis_lines = Vec::new();
        if evidence_ready {
            basis_lines.push(format!("- 已保留可追溯来源 {} 条。", evidence_urls.len()));
        } else if confirmed_count > 0 {
            basis_lines.push(format!("- 已保留可用证据信号约 {} 条。", confirmed_count));
        } else {
            basis_lines
                .push("- 外部证据没有稳定沉淀，本版以用户问题和已有上下文推理为主。".to_string());
        }
        if !evidence_urls.is_empty() {
            basis_lines.push(format!(
                "- 可追溯来源样本：{}",
                evidence_urls
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if !objectives.is_empty() {
            basis_lines.push(format!("- 目标：{}", objectives.join("、")));
        }
        if !metrics.is_empty() {
            basis_lines.push(format!("- 指标：{}", metrics.join("、")));
        }
        if !guardrails.is_empty() {
            basis_lines.push(format!("- 保护线：{}", guardrails.join("、")));
        }

        let mut action_lines = Vec::new();
        if cohorts.is_empty() {
            action_lines.push("- 先把问题拆成目标、核心指标、保护指标和关键人群/场景，避免用统一策略覆盖全部对象。".to_string());
        } else {
            for cohort in cohorts.iter().take(3) {
                action_lines.push(format!(
                    "- 针对「{cohort}」单独设计动作、触发条件和停止条件。"
                ));
            }
        }
        if !existing.is_empty() {
            action_lines.push(format!(
                "- 优先复用已有能力做低成本实验：{}。",
                existing.join("、")
            ));
        }
        action_lines.push(
            "- 每个建议都必须绑定实验组/对照组、观察窗口、主指标、保护指标和 kill criteria。"
                .to_string(),
        );
        action_lines.push(
            "- 如果外部来源不足，不引用弱证据；先给低到中置信度方案，用小流量验证替代拍脑袋放量。"
                .to_string(),
        );

        let verify_lines = vec![
            "- 强实时事实补齐官方或一手来源后再给精确数值。".to_string(),
            "- 高风险结论至少补一个可追溯 URL 或内部数据切片。".to_string(),
            "- 若保护指标连续恶化，立即回滚并复盘人群/触发条件。".to_string(),
        ];

        let mut text = format!(
            "## 可执行结论\n\n\
本轮先交付可推进的保守版结论，不把不稳定检索片段包装成确定事实。\n\n\
## 依据\n\
{}\n\n\
## 建议动作\n\
{}\n\n\
## 验证与保护线\n\
{}",
            basis_lines.join("\n"),
            action_lines.join("\n"),
            verify_lines.join("\n")
        );
        if include_origin_marker {
            let marker = if evidence_ready {
                "注：生成方式=深度总结；已结合可追溯外部证据。"
            } else {
                "注：生成方式=专家推理；未进入引用区的外部资料不作为依据。"
            };
            text = format!("{}\n\n{}", text, marker);
        }
        return text;
    }

    let mut basis_lines = Vec::new();
    if evidence_ready {
        basis_lines.push(format!(
            "- Retained {} traceable source URL(s).",
            evidence_urls.len()
        ));
    } else if confirmed_count > 0 {
        basis_lines.push(format!(
            "- Retained about {} usable evidence signal(s).",
            confirmed_count
        ));
    } else {
        basis_lines.push("- This answer is grounded in the user question and available context, with conservative confidence for high-impact claims.".to_string());
    }
    if !evidence_urls.is_empty() {
        basis_lines.push(format!(
            "- Traceable source sample: {}",
            evidence_urls
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    if !objectives.is_empty() {
        basis_lines.push(format!("- Objectives: {}", objectives.join(", ")));
    }
    if !metrics.is_empty() {
        basis_lines.push(format!("- Metrics: {}", metrics.join(", ")));
    }
    if !guardrails.is_empty() {
        basis_lines.push(format!("- Guardrails: {}", guardrails.join(", ")));
    }

    let mut action_lines = Vec::new();
    if cohorts.is_empty() {
        action_lines.push("- Split the question into objective, primary metrics, guardrails, and key cohorts/scenarios before choosing actions.".to_string());
    } else {
        for cohort in cohorts.iter().take(3) {
            action_lines.push(format!(
                "- For {cohort}, define a dedicated action, trigger condition, and stop condition."
            ));
        }
    }
    if !existing.is_empty() {
        action_lines.push(format!(
            "- Prefer low-cost experiments using existing mechanisms: {}.",
            existing.join(", ")
        ));
    }
    action_lines.push("- Bind every recommendation to treatment/control, observation window, primary metric, guardrails, and kill criteria.".to_string());
    action_lines.push("- Treat reference material as supporting context; validate high-impact claims with a small rollout before scaling.".to_string());

    let verify_lines = vec![
        "- Confirm exact current facts with authoritative sources before acting on precise values."
            .to_string(),
        "- Add at least one traceable URL or first-party data slice for high-impact claims."
            .to_string(),
        "- Roll back if guardrails degrade across the agreed observation window.".to_string(),
    ];

    let mut text = format!(
        "## Actionable Conclusion\n\n\
This is a conservative, usable answer based on the question and retained context; unstable retrieval fragments are not presented as settled facts.\n\n\
## Basis\n\
{}\n\n\
## Recommended Actions\n\
{}\n\n\
## Validation And Guardrails\n\
{}",
        basis_lines.join("\n"),
        action_lines.join("\n"),
        verify_lines.join("\n")
    );
    if include_origin_marker {
        let marker = if evidence_ready {
            "Note: Generation mode=Deep summary; traceable external evidence was included."
        } else {
            "Note: Generation mode=Expert reasoning; external material outside the citation set is not treated as evidence."
        };
        text = format!("{}\n\n{}", text, marker);
    }
    text
}

#[derive(Debug, Clone)]
pub(super) struct PmToolEvidenceHit {
    pub(super) url: String,
    pub(super) domain: String,
    pub(super) excerpt: String,
    pub(super) source_tool: String,
    pub(super) source_route: String,
    pub(super) relevance_score: Option<f64>,
    pub(super) confidence: Option<f64>,
    pub(super) trusted_relevance: bool,
}

fn parse_pm_websearch_url_content_chars(
    tc: &agent_gateway::ToolCallRecord,
) -> Vec<(String, usize)> {
    if !tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&tc.output) else {
        return Vec::new();
    };
    let Some(results) = json.get("results").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::<(String, usize)>::new();
    for item in results {
        let Some(rows) = item.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for row in rows {
            let Some(url) = row
                .get("url")
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_http_url_candidate)
            else {
                continue;
            };
            let content_chars = row
                .get("contentChars")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            out.push((url, content_chars));
        }
    }
    out
}

pub(super) fn build_pm_websearch_content_chars_map(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> std::collections::HashMap<String, usize> {
    let mut out = std::collections::HashMap::<String, usize>::new();
    for tc in tool_calls {
        for (url, content_chars) in parse_pm_websearch_url_content_chars(tc) {
            let entry = out.entry(url).or_insert(0);
            *entry = (*entry).max(content_chars);
        }
    }
    out
}

pub(super) fn pm_is_citable_url_by_content_chars(
    url: &str,
    content_chars_by_url: &std::collections::HashMap<String, usize>,
) -> bool {
    let Some(normalized) = normalize_http_url_candidate(url) else {
        return false;
    };
    match content_chars_by_url.get(&normalized).copied() {
        Some(chars) => chars > 0,
        None => true,
    }
}

fn score_claim_to_tool_hit(claim: &str, hit: &PmToolEvidenceHit) -> f64 {
    let claim_terms = tokenize_for_match(claim);
    if claim_terms.is_empty() {
        return 0.0;
    }
    let evidence_text = format!(
        "{} {} {} {}",
        hit.excerpt.to_ascii_lowercase(),
        hit.url.to_ascii_lowercase(),
        hit.domain.to_ascii_lowercase(),
        hit.source_tool.to_ascii_lowercase()
    );
    if !claim_evidence_semantically_supported(claim, &hit.excerpt) {
        return 0.0;
    }
    let matched = claim_terms
        .iter()
        .filter(|term| evidence_text.contains(term.as_str()))
        .count();
    let mut score = matched as f64 / claim_terms.len() as f64;
    if claim
        .to_ascii_lowercase()
        .contains(hit.domain.to_ascii_lowercase().as_str())
    {
        score += 0.12;
    }
    score.clamp(0.0, 1.0)
}

/// A URL is only a source locator.  Before a claim can be admitted, hard
/// evidence fields must survive a lightweight deterministic check. This is
/// intentionally conservative: numeric/date/unit mismatches become a gap and
/// are left for the model or a later source to repair.
pub(super) fn claim_evidence_semantically_supported(claim: &str, excerpt: &str) -> bool {
    crate::behavior_trace("PM-003");
    let claim_lower = claim.to_ascii_lowercase();
    let evidence_lower = excerpt.to_ascii_lowercase();
    let number_pattern =
        regex::Regex::new(r"\b\d+(?:[.,]\d+)?\s*%?\b").expect("static claim number regex");
    let claim_numbers = number_pattern
        .find_iter(&claim_lower)
        .map(|m| m.as_str().replace(',', ""))
        .collect::<Vec<_>>();
    if claim_numbers
        .iter()
        .any(|number| !evidence_lower.replace(',', "").contains(number))
    {
        return false;
    }
    let unit_groups: &[&[&str]] = &[
        &["%", "percent", "percentage", "百分比", "百分点"],
        &["day", "days", "日", "天", "周", "月", "年"],
        &["user", "users", "用户", "人次"],
    ];
    for group in unit_groups {
        let claim_has = group.iter().any(|token| claim_lower.contains(token));
        let evidence_has = group.iter().any(|token| evidence_lower.contains(token));
        if claim_has && !evidence_has {
            return false;
        }
    }
    // Currency names are not interchangeable evidence. In particular, `元`
    // is a CNY marker while `美元` is USD; treating them as one broad group
    // would admit numerically identical but materially different claims.
    let currency = |text: &str| {
        if text.contains("usd") || text.contains('$') || text.contains("美元") {
            Some("usd")
        } else if text.contains("rmb")
            || text.contains("cny")
            || text.contains("人民币")
            || text.contains("元")
        {
            Some("cny")
        } else {
            None
        }
    };
    if currency(&claim_lower).is_some() && currency(&claim_lower) != currency(&evidence_lower) {
        return false;
    }
    let entity_pattern =
        regex::Regex::new(r"\b[A-Za-z][A-Za-z0-9_-]{1,}\b").expect("static claim entity regex");
    let generic_entities = [
        "the", "and", "for", "with", "that", "this", "from", "platform", "supports", "support",
        "report", "data", "user", "users",
    ];
    let claim_entities = entity_pattern
        .find_iter(claim)
        .map(|value| value.as_str().to_ascii_lowercase())
        .filter(|value| !generic_entities.contains(&value.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let evidence_entities = entity_pattern
        .find_iter(excerpt)
        .map(|value| value.as_str().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    if !claim_entities.is_subset(&evidence_entities) {
        return false;
    }
    let negated = |text: &str| {
        [
            "不支持",
            "未支持",
            "没有",
            "并非",
            "不是",
            " no ",
            " not ",
            "never",
        ]
        .iter()
        .any(|marker| text.contains(marker))
    };
    if negated(&claim_lower) != negated(&evidence_lower) {
        return false;
    }
    let stop_words = [
        "the", "and", "for", "with", "that", "this", "from", "是", "的", "了", "在", "与", "及",
        "和", "一个", "这个",
    ];
    let claim_terms = tokenize_for_match(claim)
        .into_iter()
        .filter(|term| !stop_words.contains(&term.as_str()))
        .collect::<Vec<_>>();
    if claim_terms.is_empty() {
        return false;
    }
    let evidence_terms = tokenize_for_match(excerpt);
    let matched_terms = claim_terms
        .iter()
        .filter(|term| evidence_terms.iter().any(|candidate| candidate == *term))
        .count();
    // Numeric/unit checks above are necessary but not sufficient: a random
    // excerpt containing the same number must still share topical evidence.
    let minimum_overlap = if claim_terms.len() <= 3 { 1 } else { 2 };
    if matched_terms < minimum_overlap
        || (claim_terms.len() >= 4 && (matched_terms as f64 / claim_terms.len() as f64) < 0.2)
    {
        return false;
    }
    let directional_groups: &[(&[&str], &[&str])] = &[
        (
            &[
                "下降", "骤降", "下滑", "decline", "decrease", "drop", "fall",
            ],
            &["上升", "增长", "提升", "increase", "grow", "rise"],
        ),
        (
            &["增加", "增长", "提升", "increase", "grow", "rise"],
            &[
                "下降", "骤降", "下滑", "decline", "decrease", "drop", "fall",
            ],
        ),
    ];
    for (positive, opposite) in directional_groups {
        if positive.iter().any(|token| claim_lower.contains(token))
            && opposite.iter().any(|token| claim_lower.contains(token)) == false
            && opposite.iter().any(|token| evidence_lower.contains(token))
        {
            return false;
        }
    }
    true
}

fn pm_tool_excerpt_lexical_len(input: &str) -> usize {
    input
        .chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric()
                || ('\u{4e00}'..='\u{9fff}').contains(ch)
                || ('\u{3400}'..='\u{4dbf}').contains(ch)
                || ('\u{3040}'..='\u{30ff}').contains(ch)
                || ('\u{ac00}'..='\u{d7af}').contains(ch)
        })
        .count()
}

pub(super) fn pm_is_tool_diagnostic_excerpt(raw: &str) -> bool {
    let text = raw.trim();
    if text.is_empty() {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    let diagnostic_tokens = [
        "durationms",
        "elapsedms",
        "toolcallcount",
        "contentchars",
        "sourceslotbudgetsecs",
        "pipelinetimeoutsecs",
        "routeallowlist",
        "routepriority",
        "exec_constraints",
        "task_graph",
        "probecandidatecount",
        "probecompletedcount",
        "retrievedurationms",
        "qualitygatepassed",
        "decompositionmode",
        "maxconcurrentsubtasks",
        "maxprobepertask",
        "maxprobepersubtask",
    ];
    if diagnostic_tokens.iter().any(|token| lower.contains(token)) {
        return true;
    }
    let looks_like_json_fragment =
        (text.starts_with('{') || text.starts_with('[') || text.contains("\":"))
            && text.matches(':').count() >= 2
            && text.matches('"').count() >= 4;
    if looks_like_json_fragment {
        return true;
    }
    let punctuation_count = text
        .chars()
        .filter(|ch| matches!(ch, '{' | '}' | '[' | ']' | ':' | ',' | '"'))
        .count();
    let char_count = text.chars().count().max(1);
    punctuation_count * 100 / char_count > 18
}

fn pm_clean_tool_excerpt_candidate(raw: &str) -> Option<String> {
    let mut text = raw.trim().trim_end_matches(',').trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('"') && text.ends_with('"') && text.chars().count() > 1 {
        text = text.trim_matches('"').trim();
    }
    let stripped = strip_pm_list_prefix(text);
    let cleaned = stripped
        .trim_matches(|ch: char| ch == '{' || ch == '}' || ch == '[' || ch == ']')
        .trim();
    if cleaned.is_empty() || is_pm_visible_output_noise(cleaned) {
        return None;
    }
    if pm_is_tool_diagnostic_excerpt(cleaned) {
        return None;
    }
    if normalize_http_url_candidate(cleaned).is_some() {
        return None;
    }
    if pm_tool_excerpt_lexical_len(cleaned) < 14 {
        return None;
    }
    Some(truncate_for_log(cleaned, 260))
}

fn pm_extract_web_search_excerpt(output: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(output).ok()?;
    let rows = json.get("results").and_then(serde_json::Value::as_array)?;
    for item in rows {
        if let Some(content_rows) = item.get("content").and_then(serde_json::Value::as_array) {
            for row in content_rows {
                for key in ["snippet", "title", "content"] {
                    if let Some(raw) = row.get(key).and_then(serde_json::Value::as_str) {
                        if let Some(cleaned) = pm_clean_tool_excerpt_candidate(raw) {
                            return Some(cleaned);
                        }
                    }
                }
            }
        }
        if let Some(raw) = item.as_str() {
            if let Some(cleaned) = pm_clean_tool_excerpt_candidate(raw) {
                return Some(cleaned);
            }
        }
    }
    None
}

fn pm_json_number(value: &serde_json::Value, key: &str) -> Option<f64> {
    value.get(key).and_then(serde_json::Value::as_f64)
}

fn pm_web_search_row_excerpt(row: &serde_json::Value) -> Option<String> {
    for key in ["snippet", "content", "text", "title"] {
        if let Some(raw) = row.get(key).and_then(serde_json::Value::as_str) {
            if let Some(cleaned) = pm_clean_tool_excerpt_candidate(raw) {
                return Some(cleaned);
            }
        }
    }
    None
}

fn pm_web_search_structured_hits(tc: &agent_gateway::ToolCallRecord) -> Vec<PmToolEvidenceHit> {
    if !tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&tc.output) else {
        return Vec::new();
    };
    let Some(results) = json.get("results").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let source_route = if tc.source_name.trim().is_empty() {
        tc.source.clone()
    } else {
        format!("{}:{}", tc.source, tc.source_name)
    };
    let trusted_relevance = serde_json::from_str::<serde_json::Value>(&tc.input)
        .ok()
        .and_then(|input| {
            input
                .get("orchestrator")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|value| value == "unified_search");
    let fallback_excerpt = pm_pick_tool_excerpt(tc);
    let mut hits = Vec::<PmToolEvidenceHit>::new();
    for result in results {
        let Some(rows) = result.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for row in rows {
            let Some(url) = row
                .get("url")
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_http_url_candidate)
            else {
                continue;
            };
            if !is_pm_high_signal_source_url(&url) {
                continue;
            }
            let content_chars = row
                .get("contentChars")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if content_chars == 0 {
                continue;
            }
            let excerpt =
                pm_web_search_row_excerpt(row).unwrap_or_else(|| fallback_excerpt.clone());
            let domain = extract_url_domain(&url).unwrap_or_default();
            hits.push(PmToolEvidenceHit {
                url,
                domain,
                excerpt,
                source_tool: tc.tool_name.clone(),
                source_route: source_route.clone(),
                relevance_score: pm_json_number(row, "relevanceScore"),
                confidence: pm_json_number(row, "confidence"),
                trusted_relevance,
            });
        }
    }
    hits
}

fn pm_pick_tool_excerpt(tc: &agent_gateway::ToolCallRecord) -> String {
    if tc.tool_name.eq_ignore_ascii_case("WebSearch") {
        if let Some(excerpt) = pm_extract_web_search_excerpt(&tc.output) {
            return excerpt;
        }
    }
    for line in tc.output.lines() {
        if let Some(cleaned) = pm_clean_tool_excerpt_candidate(line) {
            return cleaned;
        }
    }
    let first_output = first_non_empty_line(&tc.output);
    if let Some(cleaned) = pm_clean_tool_excerpt_candidate(&first_output) {
        return cleaned;
    }
    let first_input = first_non_empty_line(&tc.input);
    if let Some(cleaned) = pm_clean_tool_excerpt_candidate(&first_input) {
        return cleaned;
    }
    for fallback in [
        "Retrieved source metadata only; no usable evidence excerpt was available.",
        "Source returned no usable excerpt.",
    ] {
        if !pm_is_tool_diagnostic_excerpt(fallback) {
            return fallback.to_string();
        }
    }
    "Source returned no usable excerpt.".to_string()
}

pub(super) fn build_pm_tool_evidence_hits(
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> Vec<PmToolEvidenceHit> {
    let content_chars_by_url = build_pm_websearch_content_chars_map(tool_calls);
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::<String>::new();
    for tc in tool_calls {
        let source_route = if tc.source_name.trim().is_empty() {
            tc.source.clone()
        } else {
            format!("{}:{}", tc.source, tc.source_name)
        };
        for hit in pm_web_search_structured_hits(tc) {
            let dedup_key = format!("{}|{}|{}", tc.tool_name, source_route, hit.url);
            if !seen.insert(dedup_key) {
                continue;
            }
            out.push(hit);
            if out.len() >= 320 {
                return out;
            }
        }
        let excerpt = pm_pick_tool_excerpt(tc);
        let mut urls = extract_http_urls(&tc.output);
        urls.extend(extract_http_urls(&tc.input));
        urls.sort();
        urls.dedup();
        for url in urls.into_iter().take(12) {
            if !pm_is_citable_url_by_content_chars(&url, &content_chars_by_url) {
                continue;
            }
            if !is_pm_high_signal_source_url(&url) {
                continue;
            }
            let domain = extract_url_domain(&url).unwrap_or_default();
            let dedup_key = format!("{}|{}|{}", tc.tool_name, source_route, url);
            if !seen.insert(dedup_key) {
                continue;
            }
            out.push(PmToolEvidenceHit {
                url,
                domain,
                excerpt: excerpt.clone(),
                source_tool: tc.tool_name.clone(),
                source_route: source_route.clone(),
                relevance_score: None,
                confidence: None,
                trusted_relevance: false,
            });
            if out.len() >= 320 {
                return out;
            }
        }
    }
    out
}

pub(super) fn apply_hard_alignment_from_tool_results(
    claim_alignment: Vec<PmClaimEvidenceDto>,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> (Vec<PmClaimEvidenceDto>, Vec<PmEvidenceTreeNodeDto>) {
    let content_chars_by_url = build_pm_websearch_content_chars_map(tool_calls);
    let evidence_hits = build_pm_tool_evidence_hits(tool_calls);
    if claim_alignment.is_empty() {
        return (claim_alignment, Vec::new());
    }

    let mut aligned_rows = Vec::new();
    let mut tree_nodes = Vec::new();

    for row in claim_alignment.into_iter().take(24) {
        let mut urls = row
            .urls
            .iter()
            .filter(|url| pm_is_citable_url_by_content_chars(url, &content_chars_by_url))
            .filter(|url| {
                evidence_hits.iter().any(|hit| {
                    hit.url == **url
                        && claim_evidence_semantically_supported(&row.claim, &hit.excerpt)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut leaves = Vec::new();

        for url in urls.iter().take(5) {
            leaves.push(PmEvidenceLeafDto {
                url: url.clone(),
                domain: extract_url_domain(url).unwrap_or_default(),
                excerpt: row.evidence_excerpt.chars().take(220).collect(),
            });
        }

        let mut scored_hits = evidence_hits
            .iter()
            .map(|hit| (score_claim_to_tool_hit(&row.claim, hit), hit))
            .filter(|(score, _)| *score >= 0.36)
            .collect::<Vec<_>>();
        scored_hits.sort_by(|a, b| b.0.total_cmp(&a.0));

        for (_, hit) in scored_hits.into_iter().take(4) {
            if !pm_is_citable_url_by_content_chars(&hit.url, &content_chars_by_url) {
                continue;
            }
            if !urls.iter().any(|existing| existing == &hit.url) {
                urls.push(hit.url.clone());
            }
            if !leaves.iter().any(|leaf| leaf.url == hit.url) {
                leaves.push(PmEvidenceLeafDto {
                    url: hit.url.clone(),
                    domain: hit.domain.clone(),
                    excerpt: format!(
                        "{} [{} via {}]",
                        hit.excerpt, hit.source_tool, hit.source_route
                    )
                    .chars()
                    .take(220)
                    .collect(),
                });
            }
        }

        urls.sort();
        urls.dedup();
        let cited = !urls.is_empty();
        let evidence_excerpt = if row.evidence_excerpt.trim().is_empty() {
            row.claim.clone()
        } else {
            row.evidence_excerpt.clone()
        };
        aligned_rows.push(PmClaimEvidenceDto {
            claim: row.claim.clone(),
            evidence_excerpt,
            cited,
            urls: urls.clone(),
        });
        tree_nodes.push(PmEvidenceTreeNodeDto {
            claim: row.claim,
            status: if cited {
                "confirmed".to_string()
            } else {
                "gap".to_string()
            },
            evidence_count: urls.len(),
            evidences: if leaves.is_empty() {
                vec![PmEvidenceLeafDto {
                    url: String::new(),
                    domain: String::new(),
                    excerpt: "missing evidence URL from tool outputs".to_string(),
                }]
            } else {
                leaves.into_iter().take(6).collect()
            },
        });
    }

    (aligned_rows, tree_nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pm_visible_answer_text_filters_runtime_error_lines() {
        let raw = "执行摘要\n\
tool 'WebSearch' failed: web search unavailable on all endpoints: https://search.example.com/search?q=test\n\
来源状态：当前证据不足。\n\n\
已证实\n\
- 目前可确认印尼市场存在大量现金激励游戏需求。\n\
- {\"route\":\"web.search.general\"}\n";
        let visible = extract_pm_visible_answer_text(raw);
        assert!(visible.contains("执行摘要"));
        assert!(visible.contains("已证实"));
        assert!(!visible.contains("tool 'WebSearch' failed"));
        assert!(!visible.contains("web search unavailable on all endpoints"));
        assert!(!visible.contains("来源状态：当前证据不足"));
        assert!(!visible.contains("\"route\""));
    }

    #[test]
    fn extract_pm_visible_answer_text_filters_inline_websearch_diagnostics() {
        let raw = "核心结论\n\
我建议先做分人群实验。外部检索这轮有缺口：WebSearch 没配置成功，所以我不能声称某个玩法有外部实证必然有效；但基于一手数据可以先灰度验证。\n\
保护指标包括 ROI、AIPU、次留和 ROAS。";
        let visible = extract_pm_visible_answer_text(raw);
        assert!(visible.contains("核心结论"));
        assert!(visible.contains("我建议先做分人群实验。"));
        assert!(visible.contains("基于一手数据可以先灰度验证"));
        assert!(visible.contains("保护指标包括 ROI"));
        assert!(!visible.contains("WebSearch 没配置成功"));
        assert!(!visible.contains("外部检索这轮有缺口"));
    }

    #[test]
    fn extract_pm_visible_answer_text_returns_empty_for_noise_only_payload() {
        let raw = "REPAIR_SCOPE {\"repairOnly\":[\"citations\"]}\n\
- tool \"WebSearch\" failed: runtime recovery failed\n\
REPORT_JSON {\"schemaVersion\":\"pm_report.v2\"}";
        let visible = extract_pm_visible_answer_text(raw);
        assert!(visible.is_empty());
    }

    #[test]
    fn build_pm_emergency_conclusion_hides_raw_reason_details() {
        let text = build_pm_emergency_conclusion_text(
            "印尼网赚市场怎么样",
            "tool 'WebSearch' failed: web search unavailable on all endpoints: https://search.example.com/search?q=test",
            2,
            None,
            None,
            None,
        );
        assert!(!text.contains("tool 'WebSearch' failed"));
        assert!(!text.contains("search.example.com"));
        assert!(text.contains("## 可执行结论"));
        assert!(text.contains("## 建议动作"));
        assert!(!text.contains("错误类型"));
        assert!(!text.contains("紧急收敛"));
    }

    #[test]
    fn build_pm_emergency_conclusion_prefers_evidence_backed_heading_when_urls_exist() {
        let tool_summary = serde_json::json!({
            "count": 1,
            "errorCount": 0,
            "urls": ["https://example.com/evidence"],
            "samples": []
        });
        let text = build_pm_emergency_conclusion_text(
            "根据ecpm做ewma算法和训练pltv模型哪个收益大？",
            "retrieve timeout",
            2,
            Some(&tool_summary),
            None,
            None,
        );
        assert!(text.contains("## 可执行结论"));
        assert!(text.contains("可追溯来源"));
        assert!(text.contains("https://example.com/evidence"));
    }

    #[test]
    fn extract_pm_visible_answer_text_filters_probe_fact_noise() {
        let raw = "- FACT: WebSearch retrieval attempted for query variant `abc`, provider returned HTTP 429.\n\
### Probe Source [web.search.general] / Subtask [UG激励与提现机制优化] / Variant\n\
rewarded ads payout threshold\n\
执行摘要\n\
已证实\n\
- 可先采用阶梯提现门槛，控制早期现金流风险。";
        let visible = extract_pm_visible_answer_text(raw);
        assert!(visible.contains("执行摘要"));
        assert!(visible.contains("已证实"));
        assert!(!visible.contains("FACT:"));
        assert!(!visible.contains("Probe Source"));
        assert!(!visible.contains("HTTP 429"));
    }

    #[test]
    fn extract_pm_visible_answer_text_filters_rigid_template_headings() {
        let raw =
            "Summary\n结论内容A\n\nResearch Plan\n步骤内容B\n\nClaim-Evidence Alignment\n对齐内容C";
        let visible = extract_pm_visible_answer_text(raw);
        assert!(!visible.contains("Summary"));
        assert!(!visible.contains("Research Plan"));
        assert!(!visible.contains("Claim-Evidence Alignment"));
        assert!(visible.contains("结论内容A"));
        assert!(visible.contains("步骤内容B"));
        assert!(visible.contains("对齐内容C"));
    }

    #[test]
    fn extract_pm_visible_answer_text_does_not_truncate_long_reports_by_line_count() {
        let mut raw = String::new();
        for idx in 0..260 {
            raw.push_str(&format!("第 {idx} 行：这是有效报告内容。\n"));
        }
        raw.push_str("最终结论：完整报告应保留这一行。");

        let visible = extract_pm_visible_answer_text(&raw);

        assert!(visible.contains("第 259 行"));
        assert!(visible.contains("最终结论：完整报告应保留这一行。"));
    }

    #[test]
    fn build_pm_websearch_content_chars_map_extracts_structured_hits() {
        let tc = agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "".to_string(),
            input: "{\"query\":\"test\"}".to_string(),
            output: serde_json::json!({
                "results": [
                    {
                        "content": [
                            {"url":"https://a.example.com/page","contentChars":0},
                            {"url":"https://b.example.com/page","contentChars":1234}
                        ]
                    }
                ]
            })
            .to_string(),
            is_error: false,
            duration_ms: 12,
        };
        let map = build_pm_websearch_content_chars_map(&[tc]);
        assert_eq!(map.get("https://a.example.com/page").copied(), Some(0));
        assert_eq!(map.get("https://b.example.com/page").copied(), Some(1234));
    }

    #[test]
    fn build_pm_tool_evidence_hits_skips_zero_content_char_urls() {
        let tc = agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "".to_string(),
            input: "{\"query\":\"test\"}".to_string(),
            output: serde_json::json!({
                "results": [
                    {
                        "content": [
                            {
                                "url":"https://a.example.com/page",
                                "domain":"a.example.com",
                                "title":"A title",
                                "snippet":"A long enough snippet for lexical matching support",
                                "contentChars":0
                            },
                            {
                                "url":"https://b.example.com/page",
                                "domain":"b.example.com",
                                "title":"B title",
                                "snippet":"Another long enough snippet for lexical matching support",
                                "contentChars":2222
                            }
                        ]
                    }
                ]
            })
            .to_string(),
            is_error: false,
            duration_ms: 12,
        };
        let hits = build_pm_tool_evidence_hits(&[tc]);
        assert!(hits
            .iter()
            .all(|hit| hit.url != "https://a.example.com/page"));
        assert!(hits
            .iter()
            .any(|hit| hit.url == "https://b.example.com/page"));
    }

    #[test]
    fn build_pm_tool_evidence_hits_uses_per_row_excerpt_and_trusted_scores() {
        let tc = agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "native_model_search".to_string(),
            input: serde_json::json!({
                "query": "rewarded ads ROI strategy",
                "orchestrator": "unified_search"
            })
            .to_string(),
            output: serde_json::json!({
                "results": [
                    {
                        "content": [
                            {
                                "url":"https://example.com/rewarded-ads-roi",
                                "title":"Rewarded ads ROI",
                                "snippet":"Rewarded ads can improve opt-in monetization when frequency and retention guardrails are monitored.",
                                "content":"Rewarded ads can improve opt-in monetization when frequency and retention guardrails are monitored.",
                                "contentChars":98,
                                "relevanceScore":0.71,
                                "confidence":0.82
                            },
                            {
                                "url":"https://example.org/frequency-guardrails",
                                "title":"Frequency guardrails",
                                "snippet":"Frequency capping should be measured against churn, session length, and post-ad exits.",
                                "content":"Frequency capping should be measured against churn, session length, and post-ad exits.",
                                "contentChars":86,
                                "relevanceScore":0.64,
                                "confidence":0.78
                            }
                        ]
                    }
                ]
            })
            .to_string(),
            is_error: false,
            duration_ms: 12,
        };

        let hits = build_pm_tool_evidence_hits(&[tc]);

        assert_eq!(hits.len(), 2);
        let first = hits
            .iter()
            .find(|hit| hit.url == "https://example.com/rewarded-ads-roi")
            .expect("first source");
        assert!(first.excerpt.contains("opt-in monetization"));
        assert_eq!(first.relevance_score, Some(0.71));
        assert_eq!(first.confidence, Some(0.82));
        assert!(first.trusted_relevance);

        let second = hits
            .iter()
            .find(|hit| hit.url == "https://example.org/frequency-guardrails")
            .expect("second source");
        assert!(second.excerpt.contains("post-ad exits"));
        assert_eq!(second.relevance_score, Some(0.64));
        assert!(second.trusted_relevance);
    }

    #[test]
    fn tool_evidence_hits_reject_runtime_diagnostic_excerpts() {
        let tc = agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "".to_string(),
            input: "{\"query\":\"durationMs\"}".to_string(),
            output: serde_json::json!({
                "results": [
                    {
                        "content": [
                            {
                                "url":"https://developers.google.com/admob/android/rewarded",
                                "domain":"developers.google.com",
                                "snippet":"\"durationMs\": 1391",
                                "contentChars":1200
                            }
                        ]
                    }
                ]
            })
            .to_string(),
            is_error: false,
            duration_ms: 12,
        };
        let hits = build_pm_tool_evidence_hits(&[tc]);
        assert!(hits.iter().all(|hit| !hit.excerpt.contains("durationMs")));
    }

    #[test]
    fn claim_evidence_requires_numeric_unit_and_directional_support() {
        assert!(claim_evidence_semantically_supported(
            "ROI 12.5% 在 7 天内下降",
            "ROI 12.5% 在 7 天内下降，主要受留存影响"
        ));
        assert!(!claim_evidence_semantically_supported(
            "ROI 12.5% 在 7 天内下降",
            "ROI 11.5% 在 7 天内上升"
        ));
        assert!(!claim_evidence_semantically_supported(
            "成本为 10 美元",
            "成本为 10 元"
        ));
        assert!(!claim_evidence_semantically_supported(
            "平台支持长任务恢复和移动端通知",
            "这是一篇介绍数据库索引优化的文章"
        ));
        assert!(claim_evidence_semantically_supported(
            "平台支持长任务恢复和移动端通知",
            "该平台提供长任务恢复机制，并在移动端发送通知"
        ));
        assert!(!claim_evidence_semantically_supported(
            "AOS 支持长任务恢复",
            "Codex 支持长任务恢复"
        ));
        assert!(!claim_evidence_semantically_supported(
            "平台不支持长任务恢复",
            "平台支持长任务恢复"
        ));
    }
}
