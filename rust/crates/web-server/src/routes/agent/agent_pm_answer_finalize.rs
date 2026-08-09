use super::*;

fn pm_has_conclusion_sections(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (text.contains("已证实") && text.contains("待验证"))
        || (lower.contains("confirmed")
            && (lower.contains("pending") || lower.contains("verification")))
}

fn pm_is_chat_mode_quality(quality: &PmAnswerQualityDto) -> bool {
    let reason = quality.conflict_reason.to_ascii_lowercase();
    reason.contains("chat mode bypassed deep-research quality gate")
        || reason.contains("direct answer mode bypassed deep-research quality gate")
        || (quality.tool_call_count == 0
            && quality.citation_count == 0
            && quality.domain_count == 0
            && quality.claim_count == 0
            && quality.quality_level == "high"
            && !quality.has_tool_calls
            && quality.triad_total_claims == 0)
}

fn pm_humanize_missing_key(key: &str, cjk_mode: bool) -> Option<String> {
    let normalized = key.trim().to_ascii_lowercase();
    let pretty_gap_label = |raw: &str| -> String {
        raw.trim()
            .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '`')
            .replace('_', " ")
            .trim()
            .to_string()
    };
    let mapped = match normalized.as_str() {
        "missing_tool_retrieval" => {
            if cjk_mode {
                "关键来源检索未完成，需要补齐市场、客户、社区或其他公开证据信号。"
            } else {
                "Critical source retrieval is incomplete; add market, customer, community, or other public evidence signals."
            }
        }
        "missing_citations"
        | "insufficient_citations"
        | "insufficient_visible_citation_density" => {
            if cjk_mode {
                "关键结论缺少可追溯来源，需补充明确 URL。"
            } else {
                "Key conclusions lack traceable references; add explicit source URLs."
            }
        }
        "insufficient_domain_diversity" => {
            if cjk_mode {
                "来源多样性不足，建议至少覆盖 2 个独立域名。"
            } else {
                "Source diversity is low; cover at least 2 independent domains."
            }
        }
        "low_claim_evidence_alignment" | "insufficient_claim_evidence_url_triads" => {
            if cjk_mode {
                "部分结论尚未与证据一一对齐，需要补齐 claim-evidence-url。"
            } else {
                "Some conclusions are not aligned to evidence yet; complete claim-evidence-url links."
            }
        }
        "uncovered_claim_nodes" => {
            if cjk_mode {
                "仍有高优先级结论未闭环验证，建议补证后再扩大决策。"
            } else {
                "High-priority claims are still uncovered; validate before scaling decisions."
            }
        }
        "low_conflict_confidence" | "missing_conflict_matrix" | "unresolved_source_conflicts" => {
            if cjk_mode {
                "跨来源分歧尚未充分裁决，结论置信度需要保守处理。"
            } else {
                "Cross-source disagreements are not fully adjudicated; keep confidence conservative."
            }
        }
        other if other.starts_with("contract_invalid:") => {
            if cjk_mode {
                "本轮输出结构异常，已触发自动修复并保留可执行结论。"
            } else {
                "Output contract was malformed; auto-repair was triggered with best-effort conclusions."
            }
        }
        other if other.starts_with("subtask_depth_gap:") => {
            let label = pretty_gap_label(other.trim_start_matches("subtask_depth_gap:"));
            if cjk_mode {
                return Some(if label.is_empty() {
                    "部分子任务证据深度未达标（引用数或域名数不足）。".to_string()
                } else {
                    format!("子任务「{}」证据深度未达标（引用数或域名数不足）。", label)
                });
            }
            return Some(if label.is_empty() {
                "Some subtasks did not meet evidence depth thresholds (citations/domains)."
                    .to_string()
            } else {
                format!(
                    "Subtask \"{}\" did not meet evidence depth thresholds (citations/domains).",
                    label
                )
            });
        }
        other if other.starts_with("subtask_probe_gap:") => {
            let label = pretty_gap_label(other.trim_start_matches("subtask_probe_gap:"));
            if cjk_mode {
                return Some(if label.is_empty() {
                    "部分子任务并行检索不足，需补充多源并行探针。".to_string()
                } else {
                    format!("子任务「{}」并行检索不足，需补充多源并行探针。", label)
                });
            }
            return Some(if label.is_empty() {
                "Some subtasks had insufficient parallel retrieval coverage.".to_string()
            } else {
                format!(
                    "Subtask \"{}\" had insufficient parallel retrieval coverage.",
                    label
                )
            });
        }
        other if other.starts_with("dimension_gap:") => {
            let label = pretty_gap_label(other.trim_start_matches("dimension_gap:"));
            if cjk_mode {
                return Some(if label.is_empty() {
                    "报告对部分关键维度覆盖不足，需要补齐章节。".to_string()
                } else {
                    format!("报告对关键维度「{}」覆盖不足，需要补齐章节。", label)
                });
            }
            return Some(if label.is_empty() {
                "Some key report dimensions were not covered sufficiently.".to_string()
            } else {
                format!(
                    "Key report dimension \"{}\" was not covered sufficiently.",
                    label
                )
            });
        }
        _ => "",
    };
    if mapped.is_empty() {
        return None;
    }
    Some(mapped.to_string())
}

fn pm_humanize_suggestion_line(raw: &str, cjk_mode: bool) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("enable search/browser mcp tools") {
        return if cjk_mode {
            "优先使用稳定的参考通道，并把高影响结论绑定到可追溯依据。".to_string()
        } else {
            "Use stable reference channels and bind high-impact conclusions to traceable basis."
                .to_string()
        };
    }
    if lower.contains("increase citation coverage") {
        return if cjk_mode {
            "核心结论至少补充 3 条直接来源链接。".to_string()
        } else {
            "Add at least 3 direct source links for key conclusions.".to_string()
        };
    }
    if lower.contains("add explicit conflict adjudication") {
        return if cjk_mode {
            "为冲突观点补充裁决依据，并标明最终采用口径。".to_string()
        } else {
            "Add adjudication rationale for conflicting views and state final adopted stance."
                .to_string()
        };
    }
    if lower.contains("auto depth-repair converted partial evidence") {
        return if cjk_mode {
            "系统已完成自动补全，建议下一轮做缺口补证而非全量重跑。".to_string()
        } else {
            "Auto-repair finished; next run should target only evidence gaps, not full replay."
                .to_string()
        };
    }
    if lower.contains("align each claim to a triad") {
        return if cjk_mode {
            "将关键结论补齐为 claim-evidence-url 三元组并逐条对齐。".to_string()
        } else {
            "Align each key conclusion to a claim-evidence-url triad.".to_string()
        };
    }
    if lower.contains("increase triad coverage") {
        return if cjk_mode {
            "先把核心结论的三元组覆盖率提升到可决策阈值，再输出最终建议。".to_string()
        } else {
            "Raise triad coverage for key conclusions to a decision-ready threshold first."
                .to_string()
        };
    }
    if lower.contains("backfill urls") {
        return if cjk_mode {
            "为高影响结论补齐可追溯 URL，并按置信度分层输出。".to_string()
        } else {
            "Add traceable URLs for high-impact conclusions and separate them by confidence."
                .to_string()
        };
    }
    if lower.contains("runtime recovery failed")
        || lower.contains("deterministic emergency conclusion")
    {
        return if cjk_mode {
            "下一轮优先收敛到最相关的参考通道，并限制无效重复尝试。".to_string()
        } else {
            "Next run should converge on the most relevant reference channels with capped repeated attempts."
                .to_string()
        };
    }
    if lower.contains("depth gate:")
        || lower.contains("dimension coverage gap:")
        || lower.contains("subtask_depth_gap:")
        || lower.contains("subtask_probe_gap:")
        || lower.contains("dimension_gap:")
    {
        return if cjk_mode {
            "对子任务执行定向补证，先补齐来源数与引用数再合并结论。".to_string()
        } else {
            "Run targeted subtask backfill and meet source/citation thresholds before merge."
                .to_string()
        };
    }
    if lower.contains("missing_")
        || lower.contains("subtask_")
        || lower.contains("dimension_")
        || lower.contains("claim_evidence")
        || lower.contains("runtime")
    {
        return if cjk_mode {
            "针对当前证据缺口做定向补证，再产出最终决策版本。".to_string()
        } else {
            "Run a targeted evidence-gap pass before producing the final decision version."
                .to_string()
        };
    }
    raw.to_string()
}

fn collect_pm_quality_and_tool_urls(
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
    limit: usize,
) -> Vec<String> {
    let mut urls = Vec::<String>::new();
    for url in quality.citations.iter().take(limit) {
        push_pm_emergency_url(&mut urls, url);
    }
    for row in quality.claim_alignment.iter().take(limit) {
        for url in row.urls.iter().take(2) {
            push_pm_emergency_url(&mut urls, url);
        }
    }
    for hit in build_pm_tool_evidence_hits(tool_calls)
        .into_iter()
        .take(limit)
    {
        push_pm_emergency_url(&mut urls, &hit.url);
    }
    let mut seen = HashSet::<String>::new();
    let mut deduped = Vec::new();
    for url in urls {
        if seen.insert(url.clone()) {
            deduped.push(url);
        }
        if deduped.len() >= limit {
            break;
        }
    }
    deduped
}

fn pm_answer_needs_depth_repair(
    visible_text: &str,
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> bool {
    let trimmed = visible_text.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.contains("本轮没有成功捕获外部检索证据")
        || trimmed.contains("暂无可证实结论")
        || lower.contains("no successful retrieval evidence")
        || lower.contains("no stable evidence")
    {
        return true;
    }
    let tool_hits = build_pm_tool_evidence_hits(tool_calls);
    let evidence_signal = !tool_hits.is_empty()
        || quality.citation_count > 0
        || quality.domain_count > 0
        || quality.triad_aligned_claims > 0
        || quality.deliverable;
    let char_len = trimmed.chars().count();
    if char_len < 220 && !evidence_signal {
        return true;
    }
    let has_sections = pm_has_conclusion_sections(trimmed);
    if !has_sections && pm_visible_answer_has_actionable_evidence(trimmed, quality, tool_calls) {
        return false;
    }
    if !has_sections && !evidence_signal {
        return true;
    }
    let inline_urls = extract_http_urls(trimmed);
    if !tool_hits.is_empty() && inline_urls.is_empty() && quality.citation_count == 0 {
        return true;
    }
    !quality.deliverable && !pm_visible_answer_has_actionable_evidence(trimmed, quality, tool_calls)
}

fn pm_visible_answer_has_actionable_evidence(
    visible_text: &str,
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> bool {
    let trimmed = visible_text.trim();
    if trimmed.chars().count() < 280 {
        return false;
    }
    let has_sections = pm_has_conclusion_sections(trimmed);
    let inline_urls = extract_http_urls(trimmed);
    let tool_hits = build_pm_tool_evidence_hits(tool_calls);
    if has_sections && !inline_urls.is_empty() {
        return true;
    }
    if quality.citation_count > 0 && quality.domain_count > 0 && quality.deliverable {
        return true;
    }
    if quality.triad_aligned_claims >= 2 && quality.domain_count >= 2 {
        return true;
    }
    if has_sections {
        return quality.deliverable && !tool_hits.is_empty();
    }
    (inline_urls.len() >= 2 || (quality.citation_count >= 2 && quality.domain_count >= 2))
        && (quality.deliverable || !tool_hits.is_empty())
}

fn build_pm_evidence_appendix(
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
    cjk_mode: bool,
) -> Option<String> {
    let mut lines = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();

    for row in quality
        .claim_alignment
        .iter()
        .filter(|row| row.cited)
        .take(8)
    {
        let claim = truncate_for_log(row.claim.trim(), 160);
        let Some(url) = row.urls.first() else {
            continue;
        };
        let domain = extract_url_domain(url).unwrap_or_default();
        let key = format!("{}|{}", claim.to_ascii_lowercase(), url);
        if !seen.insert(key) {
            continue;
        }
        if claim.is_empty() {
            continue;
        }
        if cjk_mode {
            lines.push(format!("- {}（{}，{}）", claim, domain, url));
        } else {
            lines.push(format!("- {} ({}, {})", claim, domain, url));
        }
        if lines.len() >= 6 {
            break;
        }
    }

    if lines.len() < 4 {
        for hit in build_pm_tool_evidence_hits(tool_calls).into_iter().take(12) {
            let excerpt = truncate_for_log(hit.excerpt.trim(), 160);
            if excerpt.is_empty() {
                continue;
            }
            let key = format!("{}|{}", excerpt.to_ascii_lowercase(), hit.url);
            if !seen.insert(key) {
                continue;
            }
            if cjk_mode {
                lines.push(format!("- {}（{}，{}）", excerpt, hit.domain, hit.url));
            } else {
                lines.push(format!("- {} ({}, {})", excerpt, hit.domain, hit.url));
            }
            if lines.len() >= 6 {
                break;
            }
        }
    }

    if lines.is_empty() {
        return None;
    }
    Some(if cjk_mode {
        format!("证据来源（节选）\n{}", lines.join("\n"))
    } else {
        format!("Evidence Sources (sample)\n{}", lines.join("\n"))
    })
}

fn pm_line_has_numeric_signal(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if pm_is_tool_diagnostic_excerpt(trimmed) {
        return false;
    }
    let has_digit = trimmed.chars().any(|ch| ch.is_ascii_digit());
    let lower = trimmed.to_ascii_lowercase();
    has_digit
        && (trimmed.contains('%')
            || lower.contains("ecpm")
            || lower.contains("cpi")
            || lower.contains("roi")
            || lower.contains("roas")
            || lower.contains("retention")
            || lower.contains("arpdau")
            || lower.contains("usd")
            || lower.contains('$')
            || trimmed.contains("留存")
            || trimmed.contains("回收")
            || trimmed.contains("成本"))
}

fn pm_count_numeric_lines(text: &str) -> usize {
    text.lines()
        .filter(|line| pm_line_has_numeric_signal(line))
        .count()
}

fn build_pm_quant_signal_appendix(
    tool_calls: &[agent_gateway::ToolCallRecord],
    cjk_mode: bool,
) -> Option<String> {
    let mut lines = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for hit in build_pm_tool_evidence_hits(tool_calls).into_iter().take(20) {
        let excerpt = truncate_for_log(hit.excerpt.trim(), 180);
        if excerpt.is_empty() || !pm_line_has_numeric_signal(&excerpt) {
            continue;
        }
        let key = excerpt.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        if cjk_mode {
            lines.push(format!("- {}（{}，{}）", excerpt, hit.domain, hit.url));
        } else {
            lines.push(format!("- {} ({}, {})", excerpt, hit.domain, hit.url));
        }
        if lines.len() >= 5 {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(if cjk_mode {
        format!("关键量化发现（节选）\n{}", lines.join("\n"))
    } else {
        format!("Key Quant Signals (sample)\n{}", lines.join("\n"))
    })
}

fn build_pm_depth_repair_text(
    visible_text: &str,
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> String {
    let tool_hits = build_pm_tool_evidence_hits(tool_calls);
    let urls = collect_pm_quality_and_tool_urls(quality, tool_calls, 12);
    let cjk_mode = contains_cjk(visible_text)
        || quality
            .claim_alignment
            .iter()
            .any(|row| contains_cjk(&row.claim) || contains_cjk(&row.evidence_excerpt));

    let dedup_lines = |items: Vec<String>, limit: usize| -> Vec<String> {
        let mut out = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();
        for raw in items {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let normalized = line.to_ascii_lowercase();
            if seen.insert(normalized) {
                out.push(line.to_string());
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    };

    let mut confirmed = Vec::<String>::new();
    for row in quality
        .claim_alignment
        .iter()
        .filter(|row| row.cited)
        .take(6)
    {
        let claim = truncate_for_log(row.claim.trim(), 150);
        if !claim.is_empty() {
            let source_url = row
                .urls
                .first()
                .map(|url| truncate_for_log(url.trim(), 120))
                .unwrap_or_default();
            if source_url.is_empty() {
                confirmed.push(format!("- {claim}"));
            } else if cjk_mode {
                confirmed.push(format!("- {claim}（来源：{source_url}）"));
            } else {
                confirmed.push(format!("- {claim} (source: {source_url})"));
            }
        }
    }
    if confirmed.is_empty() {
        for hit in tool_hits.iter().take(5) {
            let claim = truncate_for_log(hit.excerpt.trim(), 150);
            if !claim.is_empty() {
                let source_url = truncate_for_log(hit.url.trim(), 120);
                if source_url.is_empty() {
                    confirmed.push(format!("- {claim}"));
                } else if cjk_mode {
                    confirmed.push(format!("- {claim}（来源：{source_url}）"));
                } else {
                    confirmed.push(format!("- {claim} (source: {source_url})"));
                }
            }
        }
    }
    if confirmed.is_empty() {
        confirmed.push(if cjk_mode {
            "- 当前可用证据有限，先以小范围验证为主。".to_string()
        } else {
            "- Available evidence is still limited; proceed with controlled validation first."
                .to_string()
        });
    }
    confirmed = dedup_lines(confirmed, 6);

    let mut pending = Vec::<String>::new();
    for row in quality
        .claim_alignment
        .iter()
        .filter(|row| !row.cited)
        .take(4)
    {
        let claim = truncate_for_log(row.claim.trim(), 150);
        if !claim.is_empty() {
            pending.push(format!("- {claim}"));
        }
    }
    for miss in quality.missing.iter().take(6) {
        if let Some(humanized) = pm_humanize_missing_key(miss, cjk_mode) {
            pending.push(format!("- {humanized}"));
        }
    }
    if pending.is_empty() {
        pending.push(if cjk_mode {
            "- 关键数据仍需补齐：渠道成本、留存、变现效率与人群分层。".to_string()
        } else {
            "- Key inputs still missing: channel cost, retention, monetization efficiency, and user segmentation."
                .to_string()
        });
    }
    pending = dedup_lines(pending, 6);

    let mut risks = Vec::<String>::new();
    for miss in quality.missing.iter().take(6) {
        if let Some(humanized) = pm_humanize_missing_key(miss, cjk_mode) {
            risks.push(format!("- {humanized}"));
        }
    }
    if quality.conflict_graph.edge_count > 0 {
        risks.push(if cjk_mode {
            format!(
                "- 存在跨来源分歧（{} 条冲突边），当前结论需保守执行。",
                quality.conflict_graph.edge_count
            )
        } else {
            format!(
                "- Cross-source conflicts detected ({} conflict edges); execute with conservative guardrails.",
                quality.conflict_graph.edge_count
            )
        });
    }
    if risks.is_empty() {
        risks.push(if cjk_mode {
            "- 证据覆盖仍不均衡，放量前需通过对照实验验证关键假设。".to_string()
        } else {
            "- Evidence coverage is still uneven; validate key assumptions before scaling."
                .to_string()
        });
    }
    risks = dedup_lines(risks, 6);

    let mut actions = Vec::<String>::new();
    for item in quality.suggestions.iter().take(6) {
        actions.push(format!("- {}", pm_humanize_suggestion_line(item, cjk_mode)));
    }
    if actions.is_empty() {
        actions = if cjk_mode {
            vec![
                "- 先完成 72 小时小预算试投，验证 CPI、留存、回收窗口。".to_string(),
                "- 按渠道与素材做分层对照，淘汰低效组合。".to_string(),
                "- 在确认回收阈值前，避免提前扩量。".to_string(),
            ]
        } else {
            vec![
                "- Run a 72-hour small-budget test to validate CPI, retention, and payback window."
                    .to_string(),
                "- Segment by channel and creative, then remove underperforming mixes.".to_string(),
                "- Avoid scale-up before payback thresholds are confirmed.".to_string(),
            ]
        };
    }
    actions = dedup_lines(actions, 8);

    let mut now = actions.iter().take(3).cloned().collect::<Vec<_>>();
    let mut next = pending
        .iter()
        .take(2)
        .cloned()
        .chain(actions.iter().skip(3).take(2).cloned())
        .take(3)
        .collect::<Vec<_>>();
    let mut later = risks
        .iter()
        .take(2)
        .cloned()
        .chain(pending.iter().skip(2).take(2).cloned())
        .take(3)
        .collect::<Vec<_>>();
    now = dedup_lines(now, 3);
    next = dedup_lines(next, 3);
    later = dedup_lines(later, 3);
    if now.is_empty() {
        now.push(if cjk_mode {
            "- 先锁定 1-2 个最高影响变量，72 小时内完成验证。".to_string()
        } else {
            "- Lock 1-2 highest-impact variables and validate within 72 hours.".to_string()
        });
    }
    if next.is_empty() {
        next.push(if cjk_mode {
            "- 1-2 周内补齐关键缺口并更新预算阈值。".to_string()
        } else {
            "- Close key evidence gaps and refresh budget thresholds within 1-2 weeks.".to_string()
        });
    }
    if later.is_empty() {
        later.push(if cjk_mode {
            "- 按季度复盘 ROI 与风险暴露，迭代策略。".to_string()
        } else {
            "- Review ROI and risk exposure quarterly, then iterate strategy.".to_string()
        });
    }

    let summary_line = confirmed
        .first()
        .map(|line| line.trim_start_matches("- ").to_string())
        .unwrap_or_else(|| {
            if cjk_mode {
                "当前结论可作为试运行决策输入，先按保守置信度推进。".to_string()
            } else {
                "Current findings can support pilot decisions under conservative confidence."
                    .to_string()
            }
        });
    let source_hint = if urls.is_empty() {
        if cjk_mode {
            "置信度说明：高风险结论先小步验证后扩量，不把未经验证的信息当作确定事实。".to_string()
        } else {
            "Confidence note: validate high-risk conclusions in small steps before scaling; do not treat unverified information as settled fact."
                .to_string()
        }
    } else {
        let mut domains = urls
            .iter()
            .filter_map(|url| extract_url_domain(url))
            .collect::<Vec<_>>();
        domains.sort();
        domains.dedup();
        let domain_sample = domains.into_iter().take(4).collect::<Vec<_>>().join(" / ");
        if cjk_mode {
            format!("参考依据：已覆盖关键来源域名（{}）。", domain_sample)
        } else {
            format!("Reference basis: key domains covered ({domain_sample}).")
        }
    };

    if cjk_mode {
        return format!(
            "结论摘要\n{}\n{}\n\n\
当前已收敛的信息：\n{}\n\n\
仍需验证的关键点：\n{}\n\n\
主要风险与约束：\n{}\n\n\
建议优先动作（从快到慢）：\n{}\n\n\
建议节奏：\n- 0-72h：{}\n- 1-2周：{}\n- 季度：{}",
            summary_line,
            source_hint,
            confirmed.join("\n"),
            pending.join("\n"),
            risks.join("\n"),
            actions.join("\n"),
            now.join("；"),
            next.join("；"),
            later.join("；"),
        );
    }

    format!(
        "Summary\n{}\n{}\n\n\
What is already solid:\n{}\n\n\
What still needs validation:\n{}\n\n\
Main risks and constraints:\n{}\n\n\
Recommended actions (fast to slow):\n{}\n\n\
Suggested cadence:\n- 0-72h: {}\n- 1-2 weeks: {}\n- Quarterly: {}",
        summary_line,
        source_hint,
        confirmed.join("\n"),
        pending.join("\n"),
        risks.join("\n"),
        actions.join("\n"),
        now.join("; "),
        next.join("; "),
        later.join("; "),
    )
}

pub(super) fn finalize_pm_answer_text_with_repair_flag(
    answer_text: &str,
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> (String, bool) {
    let visible_text = extract_pm_visible_answer_text(answer_text);
    let cjk_mode = contains_cjk(answer_text)
        || quality.missing.iter().any(|item| contains_cjk(item))
        || quality.suggestions.iter().any(|item| contains_cjk(item));
    if pm_is_chat_mode_quality(quality) {
        let chat_text = if visible_text.trim().is_empty() {
            "你好，我在。你可以直接说一个需要研究或分析的问题，我会给你结论和建议。".to_string()
        } else {
            visible_text.trim().to_string()
        };
        return (
            pm_normalize_visible_markdown(&chat_text, cjk_mode, false),
            false,
        );
    }
    let trimmed_visible = visible_text.trim().to_string();
    let mut repaired = false;
    let mut final_text = if trimmed_visible.is_empty() {
        repaired = true;
        build_pm_depth_repair_text("", quality, tool_calls)
    } else {
        trimmed_visible
    };

    if !final_text.trim().is_empty() {
        let needs_repair = pm_answer_needs_depth_repair(&final_text, quality, tool_calls);
        let has_actionable =
            pm_visible_answer_has_actionable_evidence(&final_text, quality, tool_calls);
        if needs_repair && !has_actionable {
            if let Some(appendix) = build_pm_evidence_appendix(quality, tool_calls, cjk_mode) {
                if !final_text.contains("证据来源（节选）")
                    && !final_text.contains("Evidence Sources (sample)")
                {
                    repaired = true;
                    final_text = format!("{}\n\n{}", final_text.trim(), appendix);
                }
            }
        }
    }

    if pm_count_numeric_lines(&final_text) < 2 {
        if let Some(quant_appendix) = build_pm_quant_signal_appendix(tool_calls, cjk_mode) {
            if !final_text.contains("关键量化发现（节选）")
                && !final_text.contains("Key Quant Signals (sample)")
            {
                repaired = true;
                final_text = format!("{}\n\n{}", final_text.trim(), quant_appendix);
            }
        }
    }

    if final_text.trim().is_empty() {
        repaired = true;
        final_text = build_pm_always_answer_fallback(cjk_mode);
    }

    final_text = pm_ensure_markdown_structure(&final_text, cjk_mode);

    if pm_flag_enabled("PM_VISIBLE_ANSWER_ORIGIN_MARKER", false)
        && pm_answer_has_external_evidence(quality, tool_calls)
    {
        let deep_summary = pm_answer_has_external_evidence(quality, tool_calls);
        final_text = pm_append_answer_origin_marker(&final_text, deep_summary);
    }
    (final_text, repaired)
}

fn pm_answer_has_external_evidence(
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
) -> bool {
    if quality.citation_count > 0
        || quality.domain_count > 0
        || quality.triad_aligned_claims > 0
        || quality
            .claim_alignment
            .iter()
            .any(|row| row.cited && !row.urls.is_empty())
    {
        return true;
    }
    build_pm_tool_evidence_hits(tool_calls)
        .into_iter()
        .any(|hit| is_pm_high_signal_source_url(&hit.url))
}

fn pm_answer_has_origin_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("回答来源标记=")
        || lower.contains("answer origin marker=")
        || lower.contains("answer source marker=")
        || lower.contains("生成方式=")
        || lower.contains("generation mode=")
}

fn pm_append_answer_origin_marker(text: &str, deep_summary: bool) -> String {
    if pm_answer_has_origin_marker(text) {
        return text.to_string();
    }
    let cjk_mode = contains_cjk(text);
    let marker = if cjk_mode {
        if deep_summary {
            "注：生成方式=深度总结；已结合可追溯外部证据。"
        } else {
            "注：生成方式=专家推理；未进入引用区的外部资料不作为依据。"
        }
    } else if deep_summary {
        "Note: Generation mode=Deep summary; traceable external evidence was included."
    } else {
        "Note: Generation mode=Expert reasoning; external material outside the citation set is not treated as evidence."
    };
    if text.trim().is_empty() {
        marker.to_string()
    } else {
        format!("{}\n\n{}", text.trim(), marker)
    }
}

fn pm_try_convert_markdown_heading_line(
    line: &str,
    cjk_mode: bool,
) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed
        .trim_matches(|ch: char| {
            ch == '#'
                || ch == '*'
                || ch == '`'
                || ch == '_'
                || ch == '-'
                || ch == '='
                || ch == '~'
                || ch.is_whitespace()
        })
        .trim_end_matches([':', '：'])
        .trim();
    if normalized.is_empty() {
        return None;
    }
    let lower = normalized.to_ascii_lowercase();

    if cjk_mode && trimmed.starts_with("你的问题：") {
        let content = trimmed.trim_start_matches("你的问题：").trim();
        return Some((
            "问题定义".to_string(),
            if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            },
        ));
    }
    if !cjk_mode && lower.starts_with("your question:") {
        let content = trimmed
            .split_once(':')
            .map(|(_, rhs)| rhs.trim())
            .unwrap_or("");
        return Some((
            "Question".to_string(),
            if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            },
        ));
    }

    if cjk_mode {
        let heading = match normalized {
            "执行摘要" | "结论摘要" | "核心结论" | "结论" => "核心结论",
            "已证实" | "关键发现" => "关键发现",
            "待验证" => "待验证项",
            "风险项" | "主要风险与约束" | "风险与边界" => "风险与边界",
            "建议动作" | "建议优先动作（从快到慢）" | "可执行建议" => {
                "可执行建议"
            }
            "建议节奏" => "执行节奏",
            "来源状态" | "参考来源" | "证据来源（节选）" => "参考来源",
            _ => return None,
        };
        return Some((heading.to_string(), None));
    }

    let heading = match lower.as_str() {
        "summary" | "executive summary" | "conclusion" => "Key Takeaways",
        "confirmed" | "key findings" => "Key Findings",
        "pending" | "to validate" => "Pending Validation",
        "risks" | "risks and constraints" => "Risks and Boundaries",
        "actions" | "recommended actions" | "action plan" => "Action Plan",
        "source status" | "evidence sources (sample)" | "references" => "References",
        _ => return None,
    };
    Some((heading.to_string(), None))
}

const PM_FORMAT_METRIC_TOKENS: &[&str] = &[
    "ARPPU", "ARPU", "ROAS", "ROI", "AIPU", "eCPM", "CPM", "CPC", "CPA", "CPI", "CTR", "CVR",
    "LTV", "CAC", "DAU", "WAU", "MAU", "UV", "PV", "GMV", "MRR", "ARR", "NPS", "SLA", "SLO",
];

fn pm_metric_token_match_at(input: &str, idx: usize) -> Option<(&'static str, usize)> {
    for token in PM_FORMAT_METRIC_TOKENS {
        let end = idx.saturating_add(token.len());
        let Some(slice) = input.get(idx..end) else {
            continue;
        };
        if !slice.eq_ignore_ascii_case(token) {
            continue;
        }
        let prev = input[..idx].chars().next_back();
        let next = input[end..].chars().next();
        let all_lower = slice.chars().all(|ch| ch.is_ascii_lowercase());
        if all_lower
            && prev.is_some_and(|ch| ch.is_ascii_alphabetic())
            && next.is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            continue;
        }
        return Some((*token, end));
    }
    None
}

fn pm_is_metric_adjacent_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || ('\u{3400}'..='\u{9fff}').contains(&ch)
        || matches!(
            ch,
            '%' | '$' | '¥' | '￥' | '<' | '>' | '=' | '+' | '-' | '.' | ','
        )
}

fn pm_space_metric_tokens(input: &str) -> String {
    if input.trim().is_empty() || input.contains("http://") || input.contains("https://") {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len() + 16);
    let mut idx = 0usize;
    while idx < input.len() {
        if !input.is_char_boundary(idx) {
            idx += 1;
            continue;
        }
        if let Some((_token, end)) = pm_metric_token_match_at(input, idx) {
            let prev = input[..idx].chars().next_back();
            if prev.is_some_and(pm_is_metric_adjacent_char)
                && !out.chars().next_back().is_some_and(char::is_whitespace)
                && !out.is_empty()
            {
                out.push(' ');
            }
            if let Some(slice) = input.get(idx..end) {
                out.push_str(slice);
            }
            let next = input[end..].chars().next();
            if next.is_some_and(pm_is_metric_adjacent_char) {
                out.push(' ');
            }
            idx = end;
            continue;
        }
        let Some(ch) = input[idx..].chars().next() else {
            break;
        };
        out.push(ch);
        idx += ch.len_utf8();
    }
    let mut compact = String::with_capacity(out.len());
    let mut prev_space = false;
    for ch in out.chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_space {
                compact.push(' ');
            }
            prev_space = true;
        } else {
            compact.push(ch);
            prev_space = false;
        }
    }
    compact
}

fn pm_cleanup_numeric_glue(input: &str) -> String {
    static UI_MORE_RE: OnceLock<regex::Regex> = OnceLock::new();
    static COMMA_NUMBER_PERCENT_RE: OnceLock<regex::Regex> = OnceLock::new();
    static PERCENT_VALUE_RE: OnceLock<regex::Regex> = OnceLock::new();
    static CURRENCY_JOIN_RE: OnceLock<regex::Regex> = OnceLock::new();
    static NUMBER_CURRENCY_RE: OnceLock<regex::Regex> = OnceLock::new();
    let mut out = UI_MORE_RE
        .get_or_init(|| regex::Regex::new(r"(?i)(?:^|\s)[+＋]\s*\d+\s+more\.?").unwrap())
        .replace_all(input, " ")
        .to_string();
    out = COMMA_NUMBER_PERCENT_RE
        .get_or_init(|| regex::Regex::new(r"(\d,\d{3})(\d{1,3}(?:\.\d+)?%)").unwrap())
        .replace_all(&out, "$1 $2")
        .to_string();
    out = PERCENT_VALUE_RE
        .get_or_init(|| regex::Regex::new(r"(%)([$¥￥]?\d)").unwrap())
        .replace_all(&out, "$1 $2")
        .to_string();
    out = CURRENCY_JOIN_RE
        .get_or_init(|| regex::Regex::new(r"([$¥￥]\d[\d,]*(?:\.\d+)?)([$¥￥])").unwrap())
        .replace_all(&out, "$1 $2")
        .to_string();
    out = NUMBER_CURRENCY_RE
        .get_or_init(|| regex::Regex::new(r"(\d)([$¥￥])").unwrap())
        .replace_all(&out, "$1 $2")
        .to_string();
    out.replace("； 。", "。")
        .replace("；。", "。")
        .replace("。。", "。")
        .trim()
        .to_string()
}

fn pm_prettify_dense_metric_text(input: &str) -> String {
    pm_cleanup_numeric_glue(&pm_space_metric_tokens(input))
}

fn pm_heading_line_looks_dense_or_tabular(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() > 72 {
        return true;
    }
    let digit_count = trimmed.chars().filter(|ch| ch.is_ascii_digit()).count();
    let symbol_count = trimmed
        .chars()
        .filter(|ch| matches!(ch, '%' | '$' | '¥' | '￥' | '|' | '/' | '\\'))
        .count();
    let metric_hits = [
        "roi", "roas", "aipu", "ecpm", "cpm", "uv", "dau", "arpu", "ltv", "ctr", "cvr", "收入",
        "成本", "利润", "留存", "转化", "占比", "日均", "指标", "分层",
    ]
    .iter()
    .filter(|needle| trimmed.to_ascii_lowercase().contains(*needle))
    .count();
    (digit_count >= 3 && metric_hits >= 1)
        || (symbol_count >= 2 && metric_hits >= 1)
        || metric_hits >= 4
}

fn pm_split_dense_section_content(content: &str, heading_level: usize) -> Option<(String, String)> {
    if content.is_empty() || !pm_heading_line_looks_dense_or_tabular(content) {
        return None;
    }
    let separators = [
        " eCPM 分层",
        " eCPM分层",
        " AIPU 分层",
        " AIPU分层",
        " ROI",
        " ROAS",
        " 日均",
        " 指标",
        " 收入",
        " 成本",
        " 用户类型",
        " 人群",
    ];
    for sep in separators {
        if let Some(idx) = content.find(sep) {
            let heading = content[..idx].trim();
            let rest = content[idx..].trim();
            if !heading.is_empty() && rest.chars().count() >= 8 && heading.chars().count() <= 40 {
                return Some((
                    format!("{} {}", "#".repeat(heading_level.clamp(1, 6)), heading),
                    pm_format_dense_section_remainder(rest),
                ));
            }
        }
    }
    None
}

fn pm_split_dense_markdown_heading(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let content = pm_prettify_dense_metric_text(trimmed[hashes..].trim());
    pm_split_dense_section_content(&content, hashes)
}

fn pm_numbered_heading_prefix(line: &str) -> Option<usize> {
    let mut chars = line.char_indices().peekable();
    let mut consumed = 0usize;
    let mut saw_marker = false;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit()
            || matches!(
                ch,
                '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '零'
            )
        {
            saw_marker = true;
            consumed = idx + ch.len_utf8();
            chars.next();
            continue;
        }
        break;
    }
    if !saw_marker {
        return None;
    }
    let (_, delimiter) = chars.next()?;
    if !matches!(delimiter, '、' | '.' | '．') {
        return None;
    }
    Some(consumed + delimiter.len_utf8())
}

fn pm_split_plain_numbered_heading(line: &str, cjk_mode: bool) -> Option<(String, Option<String>)> {
    if !cjk_mode || line.starts_with('#') {
        return None;
    }
    let content = pm_prettify_dense_metric_text(line.trim());
    pm_numbered_heading_prefix(&content)?;
    if pm_heading_line_looks_dense_or_tabular(&content) {
        return None;
    }
    let char_count = content.chars().count();
    let terminal_sentence = content.ends_with('。')
        || content.ends_with('！')
        || content.ends_with('？')
        || content.ends_with('.');
    if char_count <= 64 && !terminal_sentence {
        return Some((format!("### {}", content), None));
    }
    None
}

fn pm_demote_numbered_markdown_subheading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if hashes < 3 {
        return None;
    }
    let content = trimmed[hashes..].trim();
    let prefix_len = pm_numbered_heading_prefix(content)?;
    let marker = content[..prefix_len].trim();
    let rest = content[prefix_len..].trim();
    if rest.is_empty() || rest.chars().count() > 96 {
        return None;
    }
    Some(format!("{marker} {rest}"))
}

fn pm_normalize_visible_markdown(
    text: &str,
    cjk_mode: bool,
    ensure_default_heading: bool,
) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let text = pm_clean_internal_visible_noise(text);
    let mut lines: Vec<String> = Vec::new();
    let mut has_heading = false;
    let mut in_fence = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            lines.push(line.to_string());
            continue;
        }
        if in_fence {
            lines.push(line.to_string());
            continue;
        }
        if trimmed.is_empty() {
            if !lines.is_empty() && !lines.last().is_some_and(|last| last.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let cleaned_line = pm_prettify_dense_metric_text(trimmed);
        let trimmed = cleaned_line.trim();
        if let Some((heading, inline_content)) =
            pm_try_convert_markdown_heading_line(trimmed, cjk_mode)
        {
            if !lines.is_empty() && !lines.last().is_some_and(|last| last.is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("## {}", heading));
            has_heading = true;
            if let Some(content) = inline_content {
                lines.push(content);
            }
            continue;
        }
        if let Some((heading, inline_content)) = pm_split_plain_numbered_heading(trimmed, cjk_mode)
        {
            if !lines.is_empty() && !lines.last().is_some_and(|last| last.is_empty()) {
                lines.push(String::new());
            }
            lines.push(heading);
            has_heading = true;
            if let Some(content) = inline_content {
                lines.push(String::new());
                lines.push(content);
            }
            continue;
        }
        if let Some((heading, rest)) = pm_split_dense_markdown_heading(trimmed) {
            if !lines.is_empty() && !lines.last().is_some_and(|last| last.is_empty()) {
                lines.push(String::new());
            }
            lines.push(heading);
            lines.push(String::new());
            lines.push(rest);
            has_heading = true;
            continue;
        }
        if let Some(demoted) = pm_demote_numbered_markdown_subheading(trimmed) {
            lines.push(demoted);
            continue;
        }
        if trimmed.starts_with('#') {
            has_heading = true;
        }
        lines.push(cleaned_line);
    }

    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return String::new();
    }
    if ensure_default_heading && !has_heading {
        let mut prefixed = Vec::with_capacity(lines.len() + 2);
        prefixed.push(format!(
            "## {}",
            if cjk_mode {
                "核心结论"
            } else {
                "Key Takeaways"
            }
        ));
        prefixed.push(String::new());
        prefixed.extend(lines);
        lines = prefixed;
    }
    lines.join("\n")
}

fn pm_format_dense_section_remainder(input: &str) -> String {
    static ROW_PREFIX_RE: OnceLock<regex::Regex> = OnceLock::new();
    let mut rest = pm_prettify_dense_metric_text(input);
    rest = rest.replace(" 结论 ", " 结论\n");
    rest = ROW_PREFIX_RE
        .get_or_init(|| {
            regex::Regex::new(
                r"\s+(?i:(ROI|ROAS|AIPU|eCPM|CPM|CPC|CPA|CPI|CTR|CVR|ARPU|ARPPU|LTV|CAC|DAU|WAU|MAU|UV|PV|GMV|MRR|ARR|NPS))\s*([<>=]|\d|\+|-)",
            )
            .unwrap()
        })
        .replace_all(&rest, "\n- $1 $2")
        .to_string();
    rest.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn pm_clean_internal_visible_noise(input: &str) -> String {
    static FIRST_PARTY_DETECTED_RE: OnceLock<regex::Regex> = OnceLock::new();
    static FIRST_PARTY_SNIPPET_RE: OnceLock<regex::Regex> = OnceLock::new();
    static UI_MORE_RE: OnceLock<regex::Regex> = OnceLock::new();
    let mut out = FIRST_PARTY_DETECTED_RE
        .get_or_init(|| {
            regex::Regex::new(
                r"(?i)\bDetected first-party evidence:\s*\d+\s+metric signals?\s+and\s+\d+\s+opportunity cohorts?\.?",
            )
            .unwrap()
        })
        .replace_all(input, " ")
        .to_string();
    out = FIRST_PARTY_SNIPPET_RE
        .get_or_init(|| {
            regex::Regex::new(r"(?m)^\s*(?:一手片段|First-party snippets)\s*[:：].*$").unwrap()
        })
        .replace_all(&out, "")
        .to_string();
    out = UI_MORE_RE
        .get_or_init(|| regex::Regex::new(r"(?i)(?:^|\s)[+＋]\s*\d+\s+more[.。]?").unwrap())
        .replace_all(&out, " ")
        .to_string();
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn pm_count_markdown_strong_markers(line: &str) -> usize {
    let mut count = 0usize;
    let mut chars = line.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '*' {
            continue;
        }
        if line[..idx].ends_with('\\') {
            continue;
        }
        if chars.peek().is_some_and(|(_, next)| *next == '*') {
            count += 1;
            chars.next();
        }
    }
    count
}

fn pm_repair_split_strong_markers(input: &str) -> String {
    let raw_lines = input.lines().collect::<Vec<_>>();
    if raw_lines.len() < 2 {
        return input.to_string();
    }
    let mut out = Vec::<String>::new();
    let mut idx = 0usize;
    let mut in_fence = false;
    while idx < raw_lines.len() {
        let line = raw_lines[idx];
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push(line.to_string());
            idx += 1;
            continue;
        }
        if !in_fence && idx + 1 < raw_lines.len() {
            let next = raw_lines[idx + 1];
            let next_trimmed = next.trim_start();
            let strong_open = pm_count_markdown_strong_markers(line) % 2 == 1;
            let next_looks_like_close = next_trimmed == "**" || next_trimmed.starts_with("** ");
            if strong_open && next_looks_like_close {
                out.push(format!("{}{}", line.trim_end(), next_trimmed));
                idx += 2;
                continue;
            }
        }
        out.push(line.to_string());
        idx += 1;
    }
    out.join("\n")
}

fn pm_markdown_separator_column_count(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return None;
    }
    let cells = trimmed[1..trimmed.len().saturating_sub(1)]
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();
    if cells.len() < 2
        || cells.iter().any(|cell| {
            let marker = cell.trim_matches(':');
            marker.len() < 3 || !marker.chars().all(|ch| ch == '-')
        })
    {
        return None;
    }
    Some(cells.len())
}

fn pm_split_dense_markdown_table_rows(input: &str, column_count: usize) -> Option<Vec<String>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let segments = trimmed.split('|').collect::<Vec<_>>();
    let mut rows = Vec::<String>::new();
    let mut cells = Vec::<String>::new();
    for (index, raw) in segments.iter().enumerate() {
        let cell = raw.trim();
        let outer_boundary = (index == 0 || index + 1 == segments.len()) && cell.is_empty();
        if outer_boundary || (cells.is_empty() && cell.is_empty()) {
            continue;
        }
        cells.push(cell.to_string());
        if cells.len() == column_count {
            rows.push(format!("| {} |", cells.join(" | ")));
            cells.clear();
        }
    }
    if rows.is_empty() || cells.iter().any(|cell| !cell.is_empty()) {
        return None;
    }
    Some(rows)
}

fn pm_expand_dense_markdown_table_line(line: &str) -> Vec<String> {
    static SEPARATOR_RE: OnceLock<regex::Regex> = OnceLock::new();
    let separator_re = SEPARATOR_RE
        .get_or_init(|| regex::Regex::new(r"\|\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|").unwrap());
    let Some(separator_match) = separator_re.find(line) else {
        return vec![line.to_string()];
    };
    let separator = separator_match.as_str().trim();
    let Some(column_count) = pm_markdown_separator_column_count(separator) else {
        return vec![line.to_string()];
    };
    let prefix = line[..separator_match.start()].trim();
    let Some(rows) =
        pm_split_dense_markdown_table_rows(&line[separator_match.end()..], column_count)
    else {
        return vec![line.to_string()];
    };
    let mut expanded = Vec::with_capacity(rows.len() + 3);
    if !prefix.is_empty() {
        let prefix_cells = prefix
            .strip_prefix('|')
            .unwrap_or(prefix)
            .strip_suffix('|')
            .unwrap_or(prefix)
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if prefix.starts_with('|') && prefix.ends_with('|') {
            expanded.push(prefix.to_string());
        } else if prefix.ends_with('|') && prefix_cells.len() == column_count {
            // Models sometimes omit only the header row's leading pipe.
            expanded.push(format!("| {} |", prefix_cells.join(" | ")));
        } else if prefix.ends_with('|') && prefix_cells.len() == column_count + 1 {
            // A numbered section title can be glued to an unprefixed header.
            // Keep the title as prose and emit a valid GFM header row.
            if prefix_cells[0].is_empty() {
                return vec![line.to_string()];
            }
            expanded.push(prefix_cells[0].to_string());
            expanded.push(String::new());
            expanded.push(format!("| {} |", prefix_cells[1..].join(" | ")));
        } else {
            return vec![line.to_string()];
        }
    }
    expanded.push(separator.to_string());
    expanded.extend(rows);
    expanded
}

fn pm_repair_dense_markdown_tables(input: &str) -> String {
    let mut expanded = Vec::<String>::new();
    let mut in_fence = false;
    for line in input.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            expanded.push(line.to_string());
        } else if in_fence {
            expanded.push(line.to_string());
        } else {
            expanded.extend(pm_expand_dense_markdown_table_line(line));
        }
    }

    let has_nearby_separator = |start: usize, reverse: bool| -> bool {
        let mut table_lines_seen = 0usize;
        let candidates: Box<dyn Iterator<Item = &String>> = if reverse {
            Box::new(expanded[..start].iter().rev())
        } else {
            Box::new(expanded[start + 1..].iter())
        };
        for candidate in candidates {
            let candidate = candidate.trim();
            if candidate.is_empty() || candidate == "|" {
                continue;
            }
            if !candidate.starts_with('|') {
                return false;
            }
            if pm_markdown_separator_column_count(candidate).is_some() {
                return true;
            }
            table_lines_seen += 1;
            if table_lines_seen >= 3 {
                return false;
            }
        }
        false
    };

    expanded
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed != "" && trimmed != "|" {
                return Some(line.as_str());
            }
            let previous = expanded[..index]
                .iter()
                .rev()
                .map(|candidate| candidate.trim())
                .find(|candidate| !candidate.is_empty());
            // A blank line before the first table row separates a prose/numbered
            // title from its table and must remain. Only remove malformed blank
            // rows once the preceding non-empty line is already part of a table.
            let belongs_to_table = previous.is_some_and(|candidate| candidate.starts_with('|'))
                && (has_nearby_separator(index, true) || has_nearby_separator(index, false));
            (!belongs_to_table).then_some(line.as_str())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pm_ensure_markdown_structure(text: &str, cjk_mode: bool) -> String {
    let repaired = pm_repair_dense_markdown_tables(&pm_repair_split_strong_markers(text));
    pm_normalize_visible_markdown(&repaired, cjk_mode, true)
}

fn build_pm_always_answer_fallback(cjk_mode: bool) -> String {
    if cjk_mode {
        return "问题结构已明确，先给出一版可执行的推理结论：优先验证最高杠杆假设，把收益路径、成本路径、体验保护和风险止损拆开；先用小范围实验确认方向，再扩大到更高流量。所有高风险结论按中低置信度执行，并设置明确的保护指标和回滚阈值。".to_string();
    }
    "The problem structure is clear. Here is an actionable reasoning-first conclusion: validate the highest-impact assumptions first, separate upside path, cost path, experience guardrails, and risk stop-loss, then scale only after a small controlled experiment confirms direction. Run high-risk decisions under conservative confidence with explicit guardrails and rollback thresholds."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_humanize_suggestion_line_maps_runtime_failure_noise() {
        let mapped = pm_humanize_suggestion_line(
            "Runtime recovery failed after retries; returned deterministic emergency conclusion.",
            true,
        );
        assert!(!mapped
            .to_ascii_lowercase()
            .contains("runtime recovery failed"));
        assert!(!mapped.contains("来源不稳定"));
        assert!(mapped.contains("参考通道") || mapped.contains("无效重复尝试"));
    }

    #[test]
    fn pm_humanize_missing_key_maps_subtask_depth_gap_to_business_text() {
        let mapped = pm_humanize_missing_key("subtask_depth_gap:用户画像", true)
            .expect("subtask gap should map");
        assert!(mapped.contains("用户画像"));
        assert!(mapped.contains("证据深度未达标"));
    }

    #[test]
    fn pm_humanize_missing_key_maps_dimension_gap_to_business_text() {
        let mapped = pm_humanize_missing_key("dimension_gap:market_size", true)
            .expect("dimension gap should map");
        assert!(mapped.contains("覆盖不足"));
        assert!(mapped.contains("market size"));
    }

    #[test]
    fn pm_append_answer_origin_marker_marks_deep_summary_once() {
        let text = "结论正文";
        let marked = pm_append_answer_origin_marker(text, true);
        assert!(marked.contains("生成方式=深度总结"));
        let marked_twice = pm_append_answer_origin_marker(&marked, true);
        assert_eq!(marked, marked_twice);
    }

    #[test]
    fn pm_append_answer_origin_marker_marks_llm_reply_when_no_evidence() {
        let text = "Interim answer";
        let marked = pm_append_answer_origin_marker(text, false);
        assert!(marked.contains("Generation mode=Expert reasoning"));
    }

    #[test]
    fn pm_ensure_markdown_structure_adds_default_heading_when_missing() {
        let raw = "这是一段没有标题的结果。\n- 要点A\n- 要点B";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.starts_with("## 核心结论"));
    }

    #[test]
    fn pm_ensure_markdown_structure_converts_known_sections_to_h2() {
        let raw = "结论摘要\n结论内容\n\n已证实\n- 证据1\n\n待验证\n- 验证1";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("## 核心结论"));
        assert!(structured.contains("## 关键发现"));
        assert!(structured.contains("## 待验证项"));
    }

    #[test]
    fn pm_ensure_markdown_structure_splits_dense_metric_heading() {
        let raw = "### 三、按 eCPM 用户价值分层 eCPM分层日均UV UV占比 日均收入 收入占比 日均UA+UG成本 ROI AIPU 结论";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("### 三、按 eCPM 用户价值分层\n\n"));
        assert!(structured.contains("eCPM 分层日均 UV"));
        assert!(!structured.contains("### 三、按 eCPM 用户价值分层 eCPM分层日均UV"));
    }

    #[test]
    fn pm_ensure_markdown_structure_splits_plain_dense_numbered_section() {
        let raw = "三、按 eCPM 用户价值分层 eCPM分层日均UVUV占比日均收入收入占比日均UA+UG成本ROIAIPU结论eCPM <18,46133.4%916.7916.72380.38411.26明显亏损池 +3 more。";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(!structured.contains("### 三、按 eCPM 用户价值分层"));
        assert!(structured.contains("eCPM 分层"));
        assert!(structured.contains("ROI AIPU"));
        assert!(!structured.contains("+3 more"));
        assert!(!structured.contains("成本ROIAIPU"));
    }

    #[test]
    fn pm_ensure_markdown_structure_removes_internal_fallback_noise() {
        let raw = "外部检索本轮没有拿到足够有价值信息。Detected first-party evidence: 24 metric signals and 6 opportunity cohorts.\n一手片段：核心成本结构大概是；根据用户价值分层；+2 more。\n建议先小流量验证。";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(!structured.contains("Detected first-party evidence"));
        assert!(!structured.contains("一手片段"));
        assert!(!structured.contains("+2 more"));
        assert!(structured.contains("建议先小流量验证"));
    }

    #[test]
    fn pm_ensure_markdown_structure_spaces_metric_glue_without_changing_meaning() {
        let raw = "hybridROI 不如原加权平均，EWMAROI 小幅上涨，但 AIPU、ROAS1/3/7 下降。";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("hybrid ROI"));
        assert!(structured.contains("EWMA ROI"));
        assert!(structured.contains("ROAS 1/3/7"));
    }

    #[test]
    fn pm_ensure_markdown_structure_keeps_normal_markdown_heading() {
        let raw = "### 策略建议\n- 先做小流量验证";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("### 策略建议"));
        assert!(structured.contains("- 先做小流量验证"));
    }

    #[test]
    fn pm_ensure_markdown_structure_demotes_numbered_subheadings() {
        let raw = "## 先给结论\n\n1. **新用户 AIPU 0 / 1-4 是最大亏损漏斗**\n内容\n\n### 2. **AIPU 5-15 是第一阶段目标**\n内容\n\n### 3. **高 AIPU 老用户要保护**\n内容";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("1. **新用户 AIPU 0 / 1-4 是最大亏损漏斗**"));
        assert!(structured.contains("2. **AIPU 5-15 是第一阶段目标**"));
        assert!(structured.contains("3. **高 AIPU 老用户要保护**"));
        assert!(!structured.contains("### 2. **AIPU"));
        assert!(!structured.contains("### 3. **高 AIPU"));
    }

    #[test]
    fn finalize_chat_mode_still_cleans_mixed_numbered_headings() {
        let quality = build_pm_direct_answer_quality();
        let raw = "## 先给结论\n\n因此策略优先级建议是：\n\n### 1. **第一项**\n2. **第二项**\n\n### 3. **第三项**\n4. **第四项**";
        let (final_text, repaired) = finalize_pm_answer_text_with_repair_flag(raw, &quality, &[]);
        assert!(!repaired);
        assert!(final_text.contains("1. **第一项**"));
        assert!(final_text.contains("2. **第二项**"));
        assert!(final_text.contains("3. **第三项**"));
        assert!(final_text.contains("4. **第四项**"));
        assert!(!final_text.contains("### 1. **第一项**"));
        assert!(!final_text.contains("### 3. **第三项**"));
        assert!(!final_text.starts_with("## 核心结论\n\n## 先给结论"));
    }

    #[test]
    fn finalize_research_mode_never_returns_an_empty_visible_answer() {
        let mut quality = build_pm_direct_answer_quality();
        quality.quality_level = "low".to_string();
        quality.passed = false;
        quality.deliverable = false;
        quality.conflict_reason = "deep research quality gate failed".to_string();
        quality.missing.push("empty_strategy_answer".to_string());

        let (final_text, repaired) = finalize_pm_answer_text_with_repair_flag("", &quality, &[]);

        assert!(repaired);
        assert!(final_text.chars().count() > 80);
        assert!(
            final_text.contains("建议优先动作") || final_text.contains("Recommended actions"),
            "fallback should contain an actionable recommendation section: {final_text}"
        );
    }

    #[test]
    fn pm_ensure_markdown_structure_repairs_split_bold_marker() {
        let raw = "## 核心结论\n\n**上海：在下雨的可能性较高。\n** 上海天气网显示小雨。\n\n天津：暂时不能确认。";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains("**上海：在下雨的可能性较高。** 上海天气网显示小雨。"));
        assert!(!structured.contains("较高。\n**"));
    }

    #[test]
    fn pm_ensure_markdown_structure_repairs_dense_pm_tables() {
        let raw = "## 实施计划\n\n|\n| 能力域 | 具体任务 | 验收标准 |\n|\n|---|---|---| | P0-1 | 权限隔离 | A 用户看不到 B 用户 | | P0-2 | 记忆恢复 | 刷新后继续执行 |";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains(
            "| 能力域 | 具体任务 | 验收标准 |\n|---|---|---|\n| P0-1 | 权限隔离 | A 用户看不到 B 用户 |\n| P0-2 | 记忆恢复 | 刷新后继续执行 |"
        ));
    }

    #[test]
    fn pm_ensure_markdown_structure_repairs_numbered_dense_comparison_table() {
        let raw = "六、横向对比总表\n\n1. 1 任务恢复机制对比表 | 平台 | 恢复范式 | 持久化载体 | 恢复粒度 | 幂等支持 | 证据状态 ||---|---|---|---|---|---|\n| LangGraph | 图 + Checkpointer | 状态检查点存储 | 节点级 | 推荐外部部署等键 | 官方文档 ✅ |\n| Temporal | Durable Execution | 事件日志重放 | 步骤/事件级 | 原生强一致 | 官方教程 ✅ |";
        let structured = pm_ensure_markdown_structure(raw, true);
        assert!(structured.contains(
            "1. 1 任务恢复机制对比表\n\n| 平台 | 恢复范式 | 持久化载体 | 恢复粒度 | 幂等支持 | 证据状态 |\n|---|---|---|---|---|---|\n| LangGraph | 图 + Checkpointer | 状态检查点存储 | 节点级 | 推荐外部部署等键 | 官方文档 ✅ |"
        ));
        assert!(!structured.contains("证据状态 ||---"));
    }

    #[test]
    fn pm_ensure_markdown_structure_preserves_valid_empty_table_cells() {
        let raw = "| A | B | C |\n|---|---|---|\n| value | | tail |";
        let structured = pm_ensure_markdown_structure(raw, false);
        assert!(structured.contains(raw));
    }
}
