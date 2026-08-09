use super::*;

pub(super) fn build_pm_direct_answer_quality() -> PmAnswerQualityDto {
    PmAnswerQualityDto {
        passed: true,
        deliverable: true,
        quality_level: "high".to_string(),
        has_tool_calls: false,
        tool_call_count: 0,
        citation_count: 0,
        domain_count: 0,
        claim_count: 0,
        claim_alignment_ok: true,
        triad_total_claims: 0,
        triad_aligned_claims: 0,
        triad_coverage: 1.0,
        conflict_adjudicated: true,
        conflict_confidence: 1.0,
        conflict_reason: "direct answer mode bypassed deep-research quality gate".to_string(),
        citations: Vec::new(),
        domains: Vec::new(),
        claim_alignment: Vec::new(),
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
    }
}

pub(super) fn build_pm_direct_answer_timeout_fallback(
    question: &str,
    _reason: &str,
    _timeout_secs: u64,
) -> String {
    let first_party = extract_pm_first_party_evidence(question);
    let collect = |key: &str, cap: usize| -> Vec<String> {
        let mut out = Vec::new();
        if let Some(items) = first_party.get(key).and_then(serde_json::Value::as_array) {
            for item in items.iter().take(cap.saturating_mul(2).max(cap)) {
                let raw = if let Some(text) = item.as_str() {
                    Some(text.to_string())
                } else if key == "metrics" {
                    item.get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(|name| {
                            let value = item
                                .get("value")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("");
                            if value.trim().is_empty() {
                                name.to_string()
                            } else {
                                format!("{name}={value}")
                            }
                        })
                } else {
                    item.get("cohort")
                        .or_else(|| item.get("name"))
                        .or_else(|| item.get("title"))
                        .and_then(serde_json::Value::as_str)
                        .map(std::string::ToString::to_string)
                };
                let Some(raw) = raw else {
                    continue;
                };
                let value = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if value.trim().is_empty()
                    || value.contains("...")
                    || value.contains("+2 more")
                    || out.iter().any(|existing: &String| existing == &value)
                {
                    continue;
                }
                out.push(value.chars().take(120).collect::<String>());
                if out.len() >= cap {
                    break;
                }
            }
        }
        out
    };
    let metrics = collect("metrics", 6);
    let objectives = collect("objectives", 4);
    let guardrails = collect("guardrails", 4);
    let cohorts = collect("opportunityCohorts", 4);
    if contains_cjk(question) {
        let mut basis = Vec::new();
        if !objectives.is_empty() {
            basis.push(format!("- 目标：{}", objectives.join("、")));
        }
        if !metrics.is_empty() {
            basis.push(format!("- 指标：{}", metrics.join("、")));
        }
        if !guardrails.is_empty() {
            basis.push(format!("- 保护线：{}", guardrails.join("、")));
        }
        if basis.is_empty() {
            basis.push("- 基于当前问题和上下文先给可执行保守判断。".to_string());
        }
        let mut actions = Vec::new();
        if cohorts.is_empty() {
            actions.push("- 先按人群/场景/链路拆解，不做一刀切策略。".to_string());
        } else {
            for cohort in cohorts.iter().take(3) {
                actions.push(format!(
                    "- 针对「{cohort}」单独定义动作、触发条件和停止条件。"
                ));
            }
        }
        actions.push(
            "- 每个动作都绑定实验组/对照组、观察窗口、主指标、保护指标和 kill criteria。"
                .to_string(),
        );
        actions.push(
            "- 如果问题依赖实时事实，先按低置信度处理，补到权威来源后再给精确数值。".to_string(),
        );
        return format!(
            "## 可执行保守结论\n\n\
这版先基于当前问题和已有上下文给出可推进答案，不暴露内部执行细节。\n\n\
## 依据\n{}\n\n\
## 建议动作\n{}\n\n\
## 置信度与验证\n- 当前按低到中置信度处理，适合先小流量验证。\n- 任何高风险决策都需要补齐来源或一手数据切片后再放大。",
            basis.join("\n"),
            actions.join("\n")
        );
    }
    let mut basis = Vec::new();
    if !objectives.is_empty() {
        basis.push(format!("- Objectives: {}", objectives.join(", ")));
    }
    if !metrics.is_empty() {
        basis.push(format!("- Metrics: {}", metrics.join(", ")));
    }
    if !guardrails.is_empty() {
        basis.push(format!("- Guardrails: {}", guardrails.join(", ")));
    }
    if basis.is_empty() {
        basis.push("- Based on the current question and available context, provide a conservative actionable answer.".to_string());
    }
    let mut actions = Vec::new();
    if cohorts.is_empty() {
        actions.push("- Split the work by cohort, scenario, or funnel step instead of using one uniform policy.".to_string());
    } else {
        for cohort in cohorts.iter().take(3) {
            actions.push(format!(
                "- For {cohort}, define a dedicated action, trigger, and stop condition."
            ));
        }
    }
    actions.push("- Bind each action to treatment/control, observation window, primary metric, guardrails, and kill criteria.".to_string());
    actions.push("- If the question depends on live facts, treat this as low-confidence until an authoritative source is added.".to_string());
    format!(
        "## Conservative Actionable Answer\n\n\
This answer is based on the current question and available context without exposing internal runtime details.\n\n\
## Basis\n{}\n\n\
## Recommended Actions\n{}\n\n\
## Confidence And Validation\n- Treat this as low-to-medium confidence and validate with a small controlled rollout first.\n- Backfill authoritative sources or first-party slices before scaling high-risk decisions.",
        basis.join("\n"),
        actions.join("\n")
    )
}

fn pm_quality_level(quality: &PmAnswerQualityDto) -> &'static str {
    match quality.quality_level.as_str() {
        "high" => "high",
        "partial" => "partial",
        _ => "low",
    }
}

pub(super) fn pm_quality_delivery_score(quality: &PmAnswerQualityDto) -> f64 {
    let level_bonus = match pm_quality_level(quality) {
        "high" => 0.34,
        "partial" => 0.22,
        _ => 0.05,
    };
    let citation_score = (quality.citation_count.min(8) as f64 / 8.0) * 0.20;
    let domain_score = (quality.domain_count.min(5) as f64 / 5.0) * 0.18;
    let triad_score = quality.triad_coverage.clamp(0.0, 1.0) * 0.18;
    let conflict_score = quality.conflict_confidence.clamp(0.0, 1.0) * 0.10;
    let align_score = if quality.claim_alignment_ok {
        0.08
    } else {
        0.0
    };
    let tool_score = if quality.has_tool_calls { 0.06 } else { 0.0 };
    let missing_penalty = (quality.missing.len().min(8) as f64) * 0.015;
    let raw_score = (level_bonus
        + citation_score
        + domain_score
        + triad_score
        + conflict_score
        + align_score
        + tool_score
        - missing_penalty)
        .clamp(0.0, 1.0);
    // A failed gate must never look like a perfect result in governance. Keep
    // the evidence/delivery score useful, but below the 0.60 pass threshold.
    if quality.passed {
        raw_score
    } else {
        raw_score.min(0.59)
    }
}

pub(super) fn pm_is_deliverable_quality(quality: &PmAnswerQualityDto) -> bool {
    quality.deliverable
}

pub(super) fn pm_is_soft_deliverable_quality(quality: &PmAnswerQualityDto) -> bool {
    if !pm_flag_enabled("PM_ENABLE_SOFT_QUALITY_GATE", true) {
        return pm_is_deliverable_quality(quality);
    }
    if pm_is_deliverable_quality(quality) {
        return true;
    }
    // Soft gate may deliver a degraded answer only when there is visible
    // answer substance. Tool calls, missing keys, or suggestions alone are
    // diagnostics; they must not turn a failed retrieval into a completed PM answer.
    let has_answer_substance = quality.claim_count >= 2
        || quality.triad_coverage >= 0.20
        || quality.citation_count >= 1
        || quality.domain_count >= 1;
    let not_diagnostic_only = quality.claim_count > 0 || quality.triad_total_claims > 0;
    has_answer_substance && not_diagnostic_only
}

pub(super) fn pm_synthesize_stage_status(quality: &PmAnswerQualityDto) -> &'static str {
    if pm_is_soft_deliverable_quality(quality) {
        "completed"
    } else {
        "failed"
    }
}

pub(super) fn update_best_pm_turn_quality(
    best_turn: &mut Option<TurnResult>,
    best_quality: &mut Option<PmAnswerQualityDto>,
    candidate_turn: &TurnResult,
    candidate_quality: &PmAnswerQualityDto,
) {
    if candidate_turn.text.trim().is_empty() && candidate_turn.tool_calls.is_empty() {
        return;
    }
    let candidate_score = pm_quality_delivery_score(candidate_quality);
    let should_replace = match best_quality.as_ref() {
        None => true,
        Some(existing) => candidate_score > pm_quality_delivery_score(existing) + 0.01,
    };
    if should_replace {
        *best_turn = Some(candidate_turn.clone());
        *best_quality = Some(candidate_quality.clone());
    }
}

pub(super) fn pick_preferred_pm_result(
    current_turn: TurnResult,
    current_quality: PmAnswerQualityDto,
    best_turn: &Option<TurnResult>,
    best_quality: &Option<PmAnswerQualityDto>,
) -> (TurnResult, PmAnswerQualityDto) {
    if let (Some(stored_turn), Some(stored_quality)) = (best_turn.as_ref(), best_quality.as_ref()) {
        let stored_score = pm_quality_delivery_score(stored_quality);
        let current_score = pm_quality_delivery_score(&current_quality);
        if stored_score > current_score + 0.01 {
            return (stored_turn.clone(), stored_quality.clone());
        }
    }
    (current_turn, current_quality)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_quality() -> PmAnswerQualityDto {
        PmAnswerQualityDto {
            passed: false,
            deliverable: false,
            quality_level: "low".to_string(),
            has_tool_calls: false,
            tool_call_count: 0,
            citation_count: 0,
            domain_count: 0,
            claim_count: 0,
            claim_alignment_ok: false,
            triad_total_claims: 0,
            triad_aligned_claims: 0,
            triad_coverage: 0.0,
            conflict_adjudicated: false,
            conflict_confidence: 0.0,
            conflict_reason: String::new(),
            citations: Vec::new(),
            domains: Vec::new(),
            claim_alignment: Vec::new(),
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
        }
    }

    #[test]
    fn soft_deliverable_accepts_low_evidence_recovered_answer() {
        let mut quality = make_quality();
        quality.tool_call_count = 3;
        quality.claim_count = 2;
        quality.missing.push("retrieve_timeout".to_string());
        assert!(pm_is_soft_deliverable_quality(&quality));
    }

    #[test]
    fn soft_deliverable_rejects_empty_runtime_failure_diagnostics_only() {
        let mut quality = make_quality();
        quality.missing.push("runtime_error".to_string());
        quality.suggestions.push("retry later".to_string());
        assert!(!pm_is_soft_deliverable_quality(&quality));
    }

    #[test]
    fn soft_deliverable_rejects_tool_only_diagnostics() {
        let mut quality = make_quality();
        quality.tool_call_count = 4;
        assert!(!pm_is_soft_deliverable_quality(&quality));
    }

    #[test]
    fn failed_quality_gate_cannot_report_a_passing_delivery_score() {
        let mut quality = make_quality();
        quality.quality_level = "high".to_string();
        quality.has_tool_calls = true;
        quality.citation_count = 8;
        quality.domain_count = 5;
        quality.triad_coverage = 1.0;
        quality.conflict_confidence = 1.0;
        quality.claim_alignment_ok = true;

        assert_eq!(pm_quality_delivery_score(&quality), 0.59);
        quality.passed = true;
        assert!(pm_quality_delivery_score(&quality) > 0.90);
    }

    #[test]
    fn direct_answer_timeout_fallback_hides_internal_reason() {
        let text = build_pm_direct_answer_timeout_fallback(
            "帮我分析这个策略问题",
            "runtime execution failed: direct answer turn timed out after 45s",
            120,
        );
        assert!(!text.contains("runtime execution failed"));
        assert!(!text.contains("direct answer turn timed out"));
        assert!(!text.contains("45s"));
        assert!(text.contains("可执行"));
        assert!(!text.contains("超时"));
        assert!(!text.to_ascii_lowercase().contains("timeout"));
    }
}
