use super::*;

fn pm_force_synth_diag_store() -> &'static std::sync::Mutex<HashMap<String, serde_json::Value>> {
    static STORE: OnceLock<std::sync::Mutex<HashMap<String, serde_json::Value>>> = OnceLock::new();
    STORE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn pm_set_force_synth_diag(session_id: &str, diag: serde_json::Value) {
    let Ok(mut guard) = pm_force_synth_diag_store().lock() else {
        tracing::warn!(
            session_id = %session_id,
            "force synth map-reduce diag store lock poisoned while setting diag"
        );
        return;
    };
    guard.insert(session_id.to_string(), diag);
}

pub(super) fn pm_take_force_synth_diag(session_id: &str) -> Option<serde_json::Value> {
    let Ok(mut guard) = pm_force_synth_diag_store().lock() else {
        tracing::warn!(
            session_id = %session_id,
            "force synth map-reduce diag store lock poisoned while taking diag"
        );
        return None;
    };
    guard.remove(session_id)
}

pub(super) fn pm_attach_force_synth_diag(
    session_id: &str,
    mut detail: serde_json::Value,
) -> serde_json::Value {
    let Some(diag) = pm_take_force_synth_diag(session_id) else {
        return detail;
    };
    if let Some(obj) = detail.as_object_mut() {
        obj.insert("mapReduce".to_string(), diag);
        return detail;
    }
    serde_json::json!({
        "detail": detail,
        "mapReduce": diag
    })
}

fn pm_min_visible_citations_for_answer(answer_chars: usize) -> usize {
    if answer_chars <= 2_400 {
        return 3;
    }
    (3 + answer_chars.saturating_sub(2_400).div_ceil(1_600)).min(8)
}

fn pm_min_visible_domains_for_answer(answer_chars: usize) -> usize {
    if answer_chars >= 4_000 {
        3
    } else {
        2
    }
}

pub(super) fn evaluate_pm_answer_quality(turn: &agent_gateway::TurnResult) -> PmAnswerQualityDto {
    let has_tool_calls = !turn.tool_calls.is_empty();
    let websearch_content_chars = build_pm_websearch_content_chars_map(&turn.tool_calls);
    let visible_answer = extract_pm_visible_answer_text(&turn.text);
    let mut citations = extract_http_urls(&visible_answer)
        .into_iter()
        .filter(|url| is_pm_high_signal_source_url(url))
        .filter(|url| pm_is_citable_url_by_content_chars(url, &websearch_content_chars))
        .collect::<Vec<_>>();
    // Native Responses search and several MCP providers return citation
    // metadata in tool results rather than replaying literal URLs into model
    // text. Those are still real references: the content admission map only
    // contains URLs with a non-trivial retrieved excerpt. Persist them in the
    // evidence/quality projection so provider wire differences do not make a
    // well-grounded report fail every quality gate.
    citations.extend(
        websearch_content_chars
            .keys()
            .filter(|url| is_pm_high_signal_source_url(url))
            .filter(|url| pm_is_citable_url_by_content_chars(url, &websearch_content_chars))
            .cloned(),
    );
    citations.sort();
    citations.dedup();
    let citation_count = citations.len();
    let answer_chars = visible_answer.chars().count();
    let min_visible_citations = pm_min_visible_citations_for_answer(answer_chars);
    let min_visible_domains = pm_min_visible_domains_for_answer(answer_chars);
    let mut domains = std::collections::BTreeSet::new();
    for url in &citations {
        if let Some(domain) = extract_url_domain(url) {
            domains.insert(domain);
        }
    }
    let domain_count = domains.len();
    let domain_list: Vec<String> = domains.iter().cloned().collect();
    let extracted_claim_alignment = extract_claim_alignment(&visible_answer);
    let (claim_alignment, evidence_tree) =
        apply_hard_alignment_from_tool_results(extracted_claim_alignment, &turn.tool_calls);
    let conflict_matrix = extract_conflict_matrix(&visible_answer);
    let conflict_graph = build_pm_conflict_graph(&conflict_matrix, &claim_alignment);
    // Natural decision reports contain many recommendation/checklist bullets.
    // They are not all externally verifiable factual claims. Evaluate triads
    // only for claim rows the alignment extractor recognized, otherwise a
    // useful report with eight cited sources can be scored as 4/256 and trigger
    // a full, expensive rewrite that does not improve factuality.
    let claim_count = claim_alignment.len();
    let cited_claim_count = claim_alignment.iter().filter(|row| row.cited).count();
    let triad_total_claims = claim_count.max(claim_alignment.len());
    let triad_aligned_claims = cited_claim_count;
    let triad_coverage = if triad_total_claims == 0 {
        if citation_count > 0 {
            1.0
        } else {
            0.0
        }
    } else {
        (triad_aligned_claims as f64 / triad_total_claims as f64).clamp(0.0, 1.0)
    };
    let claim_alignment_ok = if claim_count == 0 {
        citation_count > 0
    } else {
        triad_coverage >= 0.60
    };
    let (conflict_adjudicated, conflict_confidence, conflict_reason) =
        if conflict_graph.edge_count == 0 {
            (
            false,
            if domain_count >= 2 { 0.60 } else { 0.35 },
            "no explicit conflict graph; confidence inferred from cross-domain consistency only"
                .to_string(),
        )
        } else {
            let adjudicated_ratio =
                conflict_graph.adjudicated_count as f64 / conflict_graph.edge_count as f64;
            let confidence = (0.45
                + adjudicated_ratio * 0.30
                + conflict_graph.avg_confidence * 0.15
                + (domain_count.min(4) as f64 / 4.0) * 0.10)
                .clamp(0.0, 1.0);
            (
                conflict_graph.adjudicated_count > 0,
                confidence,
                format!(
                    "conflict edges={} adjudicated={} unresolved={} domain_count={}",
                    conflict_graph.edge_count,
                    conflict_graph.adjudicated_count,
                    conflict_graph.unresolved_count,
                    domain_count,
                ),
            )
        };
    let mut missing = Vec::new();
    let mut suggestions = Vec::new();

    if !has_tool_calls {
        missing.push("missing_tool_retrieval".to_string());
        suggestions.push(
            "Enable search/browser MCP tools and let assistant retrieve evidence first."
                .to_string(),
        );
    }
    if citation_count == 0 {
        missing.push("missing_citations".to_string());
        suggestions.push(
            "Provide source URLs for each key fact and mark uncertain items explicitly."
                .to_string(),
        );
    }
    if citation_count > 0 && citation_count < min_visible_citations {
        missing.push("insufficient_visible_citation_density".to_string());
        suggestions.push(format!(
            "Increase visible inline citation coverage to at least {min_visible_citations} distinct URLs for this answer length; place links immediately after supported claims."
        ));
    }
    if domain_count < min_visible_domains {
        missing.push("insufficient_domain_diversity".to_string());
        suggestions.push(format!(
            "Use at least {min_visible_domains} distinct visible source domains and cross-check consistency."
        ));
    }
    if !claim_alignment_ok {
        missing.push("low_claim_evidence_alignment".to_string());
        suggestions.push(
            "Align each claim to a triad: claim sentence + evidence snippet + source URL."
                .to_string(),
        );
    }
    if triad_coverage < 0.60 {
        missing.push("insufficient_claim_evidence_url_triads".to_string());
        suggestions.push(
            "Increase triad coverage to >=60% for key claims before final synthesis.".to_string(),
        );
    }
    let uncovered_nodes = evidence_tree
        .iter()
        .filter(|node| node.status == "gap")
        .count();
    if uncovered_nodes > 0 {
        missing.push("uncovered_claim_nodes".to_string());
        suggestions.push(
            "Backfill URLs for uncovered claims in the evidence tree to avoid weak conclusions."
                .to_string(),
        );
    }
    if conflict_confidence < 0.55 {
        missing.push("low_conflict_confidence".to_string());
        suggestions.push(
            "Add explicit conflict adjudication with verdict reasons and supporting URLs."
                .to_string(),
        );
    }
    let passed = has_tool_calls
        && citation_count >= min_visible_citations
        && domain_count >= min_visible_domains
        && claim_alignment_ok
        && triad_coverage >= 0.60
        && uncovered_nodes == 0
        && conflict_confidence >= 0.55;
    let deliverable = !turn.text.trim().is_empty()
        && ((has_tool_calls && citation_count >= 1 && domain_count >= 1)
            || citation_count >= 2
            || triad_coverage >= 0.35);
    let quality_level = if passed {
        "high".to_string()
    } else if deliverable {
        "partial".to_string()
    } else {
        "low".to_string()
    };

    PmAnswerQualityDto {
        passed,
        deliverable,
        quality_level,
        has_tool_calls,
        tool_call_count: turn.tool_calls.len(),
        citation_count,
        domain_count,
        claim_count,
        claim_alignment_ok,
        triad_total_claims,
        triad_aligned_claims,
        triad_coverage,
        conflict_adjudicated,
        conflict_confidence,
        conflict_reason,
        citations,
        domains: domain_list,
        claim_alignment,
        evidence_tree,
        conflict_matrix,
        conflict_graph,
        missing,
        suggestions,
    }
}

pub(super) fn apply_pm_conflict_gate(quality: &mut PmAnswerQualityDto) {
    let has_unresolved_conflicts =
        quality.conflict_graph.edge_count > 0 && quality.conflict_graph.unresolved_count > 0;
    if has_unresolved_conflicts {
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        quality.conflict_confidence = quality.conflict_confidence.min(0.45);
        quality.conflict_reason = "detected cross-source conflicts remain unresolved".to_string();
        if !quality
            .missing
            .iter()
            .any(|x| x == "unresolved_source_conflicts")
        {
            quality
                .missing
                .push("unresolved_source_conflicts".to_string());
        }
        let suggestion = "Adjudicate each detected source conflict and state the supported verdict, rationale, confidence, and citations.".to_string();
        if !quality.suggestions.iter().any(|x| x == &suggestion) {
            quality.suggestions.push(suggestion);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct PmEvidenceAdmissionReport {
    pub(super) accepted_probe_outcomes: Vec<PmProbeOutcome>,
    pub(super) accepted_tool_calls: Vec<agent_gateway::ToolCallRecord>,
    pub(super) accepted_urls: Vec<String>,
    pub(super) accepted_domains: Vec<String>,
    pub(super) rejected_evidence_count: usize,
    pub(super) rejection_reasons: Vec<String>,
    pub(super) examined_evidence_count: usize,
    pub(super) external_evidence_usable: bool,
    pub(super) expert_only_fallback: bool,
}

impl PmEvidenceAdmissionReport {
    pub(super) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "examinedEvidenceCount": self.examined_evidence_count,
            "acceptedEvidenceCount": self.accepted_urls.len(),
            "acceptedProbeCount": self.accepted_probe_outcomes.len(),
            "acceptedToolCallCount": self.accepted_tool_calls.len(),
            "acceptedDomainCount": self.accepted_domains.len(),
            "rejectedEvidenceCount": self.rejected_evidence_count,
            "rejectionReasons": self.rejection_reasons,
            "externalEvidenceUsable": self.external_evidence_usable,
            "expertOnlyFallback": self.expert_only_fallback,
            "acceptedUrls": self.accepted_urls.iter().take(12).collect::<Vec<_>>(),
            "acceptedDomains": self.accepted_domains.iter().take(12).collect::<Vec<_>>(),
        })
    }
}

fn pm_push_admission_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|item| item == reason) {
        reasons.push(reason.to_string());
    }
}

fn pm_admission_normalized_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::<String>::new();
    let stopwords = [
        "the", "and", "for", "with", "from", "about", "what", "when", "where", "which", "how",
        "why", "are", "is", "was", "were", "this", "that", "these", "those", "into", "based",
        "using", "give", "make", "need", "please", "你", "我", "他", "她", "它", "我们", "你们",
        "他们", "这个", "那个", "一下", "请", "帮我", "基于", "根据",
    ];
    for token in tokenize_for_match(input) {
        let trimmed = token.trim().to_ascii_lowercase();
        if trimmed.is_empty() || stopwords.contains(&trimmed.as_str()) {
            continue;
        }
        if !tokens.iter().any(|item| item == &trimmed) {
            tokens.push(trimmed);
        }
    }

    let mut cjk_run = Vec::<char>::new();
    let flush_cjk = |run: &mut Vec<char>, tokens: &mut Vec<String>| {
        if run.len() < 2 {
            run.clear();
            return;
        }
        for size in [2usize, 3usize] {
            if run.len() < size {
                continue;
            }
            for window in run.windows(size).take(24) {
                let token = window.iter().collect::<String>();
                if !tokens.iter().any(|item| item == &token) {
                    tokens.push(token);
                }
            }
        }
        run.clear();
    };
    for ch in input.chars() {
        let is_cjk =
            ('\u{4e00}'..='\u{9fff}').contains(&ch) || ('\u{3400}'..='\u{4dbf}').contains(&ch);
        if is_cjk {
            cjk_run.push(ch);
        } else {
            flush_cjk(&mut cjk_run, &mut tokens);
        }
    }
    flush_cjk(&mut cjk_run, &mut tokens);
    tokens.truncate(96);
    tokens
}

fn pm_admission_excerpt_is_usable(excerpt: &str) -> bool {
    let trimmed = excerpt.trim();
    if trimmed.is_empty() || pm_is_tool_diagnostic_excerpt(trimmed) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("no usable excerpt")
        || lower.contains("metadata only")
        || lower.contains("source returned no usable")
        || lower.contains("本轮未返回")
    {
        return false;
    }
    let lexical_len = trimmed
        .chars()
        .filter(|ch| {
            ch.is_ascii_alphanumeric()
                || ('\u{4e00}'..='\u{9fff}').contains(ch)
                || ('\u{3400}'..='\u{4dbf}').contains(ch)
        })
        .count();
    lexical_len >= 24
}

fn pm_admission_relevance_ok(context_tokens: &[String], hit: &PmToolEvidenceHit) -> bool {
    if hit.trusted_relevance
        && hit.relevance_score.unwrap_or_default() >= 0.26
        && hit.confidence.unwrap_or(0.0) >= 0.55
    {
        return true;
    }
    if context_tokens.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        hit.excerpt, hit.url, hit.domain, hit.source_tool, hit.source_route
    )
    .to_ascii_lowercase();
    let matched = context_tokens
        .iter()
        .filter(|token| haystack.contains(token.as_str()))
        .count();
    if context_tokens.len() <= 4 {
        matched >= 1
    } else {
        matched >= 2 || (matched as f64 / context_tokens.len() as f64) >= 0.10
    }
}

fn pm_admission_context_for_outcome(user_message: &str, outcome: &PmProbeOutcome) -> String {
    let user_excerpt = user_message.chars().take(1800).collect::<String>();
    [
        outcome.variant.as_str(),
        outcome.subtask_title.as_deref().unwrap_or(""),
        outcome.subtask_goal.as_deref().unwrap_or(""),
        outcome.subtask_deliverable.as_deref().unwrap_or(""),
        user_excerpt.as_str(),
    ]
    .join("\n")
}

fn pm_admit_tool_calls(
    context_tokens: &[String],
    tool_calls: &[agent_gateway::ToolCallRecord],
    accepted_tool_calls: &mut Vec<agent_gateway::ToolCallRecord>,
    accepted_urls: &mut std::collections::BTreeSet<String>,
    accepted_domains: &mut std::collections::BTreeSet<String>,
    reasons: &mut Vec<String>,
) -> usize {
    let mut accepted_hits = 0usize;
    for tc in tool_calls {
        if tc.is_error {
            pm_push_admission_reason(reasons, "tool_error");
            continue;
        }
        let hits = build_pm_tool_evidence_hits(std::slice::from_ref(tc));
        if hits.is_empty() {
            pm_push_admission_reason(reasons, "missing_source_backed_hit");
            continue;
        }
        let mut accepted_this_call = false;
        for hit in hits {
            if !is_pm_high_signal_source_url(&hit.url) {
                pm_push_admission_reason(reasons, "low_signal_url");
                continue;
            }
            if !pm_admission_excerpt_is_usable(&hit.excerpt) {
                pm_push_admission_reason(reasons, "missing_usable_excerpt");
                continue;
            }
            if !pm_admission_relevance_ok(context_tokens, &hit) {
                pm_push_admission_reason(reasons, "low_query_relevance");
                continue;
            }
            accepted_hits = accepted_hits.saturating_add(1);
            accepted_this_call = true;
            accepted_urls.insert(hit.url.clone());
            if !hit.domain.trim().is_empty() {
                accepted_domains.insert(hit.domain.to_ascii_lowercase());
            }
        }
        if accepted_this_call {
            accepted_tool_calls.push(tc.clone());
        }
    }
    accepted_hits
}

pub(super) fn admit_pm_external_evidence(
    user_message: &str,
    probe_outcomes: &[PmProbeOutcome],
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> PmEvidenceAdmissionReport {
    let mut accepted_probe_outcomes = Vec::<PmProbeOutcome>::new();
    let mut accepted_tool_calls = Vec::<agent_gateway::ToolCallRecord>::new();
    let mut accepted_urls = std::collections::BTreeSet::<String>::new();
    let mut accepted_domains = std::collections::BTreeSet::<String>::new();
    let mut rejection_reasons = Vec::<String>::new();
    let mut examined_evidence_count = 0usize;
    let mut accepted_evidence_count = 0usize;

    for outcome in probe_outcomes {
        let Some(turn) = outcome.turn.as_ref() else {
            if outcome.error.is_some() {
                examined_evidence_count = examined_evidence_count.saturating_add(1);
                pm_push_admission_reason(&mut rejection_reasons, "probe_error");
            }
            continue;
        };
        if turn.tool_calls.is_empty() {
            examined_evidence_count = examined_evidence_count.saturating_add(1);
            pm_push_admission_reason(&mut rejection_reasons, "probe_without_tool_evidence");
            continue;
        }
        examined_evidence_count = examined_evidence_count.saturating_add(turn.tool_calls.len());
        let tokens = pm_admission_normalized_tokens(&pm_admission_context_for_outcome(
            user_message,
            outcome,
        ));
        let before = accepted_evidence_count;
        let mut admitted_tool_calls = Vec::<agent_gateway::ToolCallRecord>::new();
        accepted_evidence_count = accepted_evidence_count.saturating_add(pm_admit_tool_calls(
            &tokens,
            &turn.tool_calls,
            &mut admitted_tool_calls,
            &mut accepted_urls,
            &mut accepted_domains,
            &mut rejection_reasons,
        ));
        if accepted_evidence_count > before {
            let mut admitted = outcome.clone();
            if let Some(mut admitted_turn) = admitted.turn.clone() {
                admitted_turn.tool_calls = admitted_tool_calls.clone();
                admitted.quality = Some(evaluate_pm_answer_quality(&admitted_turn));
                admitted.turn = Some(admitted_turn);
            }
            accepted_tool_calls =
                merge_pm_tool_calls_unique(&accepted_tool_calls, &admitted_tool_calls);
            accepted_probe_outcomes.push(admitted);
        }
    }

    if !observed_tool_calls.is_empty() {
        examined_evidence_count = examined_evidence_count.saturating_add(observed_tool_calls.len());
        let tokens = pm_admission_normalized_tokens(user_message);
        accepted_evidence_count = accepted_evidence_count.saturating_add(pm_admit_tool_calls(
            &tokens,
            observed_tool_calls,
            &mut accepted_tool_calls,
            &mut accepted_urls,
            &mut accepted_domains,
            &mut rejection_reasons,
        ));
    }

    accepted_tool_calls = merge_pm_tool_calls_unique(&[], &accepted_tool_calls);
    let accepted_urls_vec = accepted_urls.into_iter().collect::<Vec<_>>();
    let accepted_domains_vec = accepted_domains.into_iter().collect::<Vec<_>>();
    let external_evidence_usable = !accepted_urls_vec.is_empty();
    let rejected_evidence_count = examined_evidence_count.saturating_sub(accepted_evidence_count);
    PmEvidenceAdmissionReport {
        accepted_probe_outcomes,
        accepted_tool_calls,
        accepted_urls: accepted_urls_vec,
        accepted_domains: accepted_domains_vec,
        rejected_evidence_count,
        rejection_reasons,
        examined_evidence_count,
        external_evidence_usable,
        expert_only_fallback: examined_evidence_count > 0 && !external_evidence_usable,
    }
}

pub(super) fn apply_pm_evidence_admission_gate(
    quality: &mut PmAnswerQualityDto,
    answer_text: &str,
    admission: &PmEvidenceAdmissionReport,
) {
    if admission.examined_evidence_count == 0 {
        return;
    }
    let accepted_url_set = admission
        .accepted_urls
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut visible_admitted_citations = quality
        .citations
        .iter()
        .filter(|url| accepted_url_set.contains(*url))
        .cloned()
        .collect::<Vec<_>>();
    visible_admitted_citations.sort();
    visible_admitted_citations.dedup();
    // Admission proves that a source may be cited; it does not prove that the
    // final answer actually cited it. Keep quality metrics tied to links the
    // user can see so tool output cannot silently inflate citation coverage.
    quality.citations = visible_admitted_citations;
    let mut visible_domains = std::collections::BTreeSet::new();
    for url in &quality.citations {
        if let Some(domain) = extract_url_domain(url) {
            visible_domains.insert(domain);
        }
    }
    quality.domains = visible_domains.into_iter().collect();
    quality.citation_count = quality.citations.len();
    quality.domain_count = quality.domains.len();
    quality.tool_call_count = admission.accepted_tool_calls.len();
    quality.has_tool_calls =
        !admission.accepted_tool_calls.is_empty() || !admission.accepted_probe_outcomes.is_empty();
    let answer_chars = extract_pm_visible_answer_text(answer_text).chars().count();
    let min_visible_citations = pm_min_visible_citations_for_answer(answer_chars);
    let min_visible_domains = pm_min_visible_domains_for_answer(answer_chars);
    if quality.citation_count < min_visible_citations {
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        push_pm_quality_missing_once(quality, "insufficient_visible_citation_density".to_string());
        push_pm_quality_suggestion_once(
            quality,
            format!(
                "Place at least {min_visible_citations} admitted source links immediately after the claims they support for this answer length."
            ),
        );
    }
    if quality.domain_count < min_visible_domains {
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        push_pm_quality_missing_once(quality, "insufficient_domain_diversity".to_string());
    }
    if !admission.external_evidence_usable {
        quality.passed = false;
        quality.deliverable = quality.deliverable && quality.claim_count >= 2;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        push_pm_quality_missing_once(quality, "external_evidence_not_admitted".to_string());
        push_pm_quality_suggestion_once(
            quality,
            "Discard low-quality external snippets and answer from first-party data plus clearly marked expert reasoning unless a later retry finds source-backed evidence."
                .to_string(),
        );
    } else if quality.citation_count < 2 {
        remove_pm_quality_missing(quality, "missing_tool_retrieval");
        remove_pm_quality_missing(quality, "external_evidence_not_admitted");
        remove_pm_stale_no_tool_review_markers(quality);
        remove_pm_quality_suggestion_containing(quality, "Enable search/browser MCP tools");
        remove_pm_quality_suggestion_containing(quality, "Discard low-quality external snippets");
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        if !answer_text.trim().is_empty() {
            quality.deliverable = true;
            if quality.quality_level == "low" {
                quality.quality_level = "partial".to_string();
            }
        }
        push_pm_quality_missing_once(quality, "thin_admitted_external_evidence".to_string());
    } else {
        remove_pm_quality_missing(quality, "missing_tool_retrieval");
        remove_pm_quality_missing(quality, "missing_citations");
        remove_pm_quality_missing(quality, "external_evidence_not_admitted");
        remove_pm_quality_missing(quality, "thin_admitted_external_evidence");
        remove_pm_stale_no_tool_review_markers(quality);
        remove_pm_quality_suggestion_containing(quality, "Enable search/browser MCP tools");
        remove_pm_quality_suggestion_containing(quality, "Discard low-quality external snippets");
        remove_pm_quality_suggestion_containing(quality, "Increase citation coverage");
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct PmDepthCoverageGateResult {
    pub(super) enforced: bool,
    pub(super) subtask_gap_titles: Vec<String>,
    pub(super) dimension_gap_titles: Vec<String>,
    pub(super) coverage_gate: SubtaskCoverageGate,
    pub(super) gap_repair_plan: GapRepairPlan,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(super) struct SubtaskEvidenceBundle {
    pub(super) subtask_key: String,
    pub(super) title: String,
    pub(super) probe_count: usize,
    pub(super) citation_count: usize,
    pub(super) domain_count: usize,
    pub(super) citations: Vec<String>,
    pub(super) domains: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(super) struct SubtaskCoverageGate {
    pub(super) passed: bool,
    pub(super) min_parallel_agents: usize,
    pub(super) min_citations_per_subtask: usize,
    pub(super) min_domains_per_subtask: usize,
    pub(super) bundles: Vec<SubtaskEvidenceBundle>,
    pub(super) gap_subtasks: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone)]
pub(super) struct GapRepairPlan {
    pub(super) enabled: bool,
    pub(super) reason: String,
    pub(super) target_subtasks: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct PmSubtaskCoverage {
    probe_count: usize,
    citations: HashSet<String>,
    domains: HashSet<String>,
}

fn normalized_pm_key(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

fn pm_contains_dimension_text(haystack: &str, needle: &str) -> bool {
    let needle_trimmed = needle.trim();
    if needle_trimmed.is_empty() {
        return false;
    }
    let haystack_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle_trimmed.to_ascii_lowercase();
    if haystack_lower.contains(&needle_lower) {
        return true;
    }

    // Planner titles commonly append generic work labels that should not have
    // to appear verbatim in the user-facing report. Match the distinctive
    // subject after removing those labels, while still requiring every
    // remaining subject token to be present.
    let mut subject = needle_lower;
    for generic in [
        "能力调查",
        "能力分析",
        "横向对比",
        "差异化建议",
        "专题研究",
        "深度研究",
        "调查",
        "分析",
        "研究",
        "capability investigation",
        "capability analysis",
        "comparative analysis",
        "comparison",
        "assessment",
        "research",
        "overview",
    ] {
        subject = subject.replace(generic, " ");
    }
    let subject_tokens = subject
        .split(|ch: char| {
            ch.is_whitespace()
                || ch.is_ascii_punctuation()
                || matches!(
                    ch,
                    '，' | '。' | '；' | '：' | '、' | '（' | '）' | '【' | '】' | '与'
                )
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter(|token| !matches!(*token, "and" | "the" | "for" | "of"))
        .collect::<Vec<_>>();
    !subject_tokens.is_empty()
        && subject_tokens
            .iter()
            .all(|token| haystack_lower.contains(token))
}

fn collect_report_json_texts(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if !text.trim().is_empty() {
                out.push(text.clone());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_report_json_texts(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_report_json_texts(item, out);
            }
        }
        _ => {}
    }
}

fn push_pm_quality_missing_once(quality: &mut PmAnswerQualityDto, key: String) {
    if !quality.missing.iter().any(|item| item == &key) {
        quality.missing.push(key);
    }
}

fn push_pm_quality_suggestion_once(quality: &mut PmAnswerQualityDto, suggestion: String) {
    if !quality.suggestions.iter().any(|item| item == &suggestion) {
        quality.suggestions.push(suggestion);
    }
}

fn remove_pm_quality_missing(quality: &mut PmAnswerQualityDto, key: &str) {
    quality.missing.retain(|item| item != key);
}

fn remove_pm_quality_suggestion_containing(quality: &mut PmAnswerQualityDto, needle: &str) {
    quality.suggestions.retain(|item| !item.contains(needle));
}

fn remove_pm_stale_no_tool_review_markers(quality: &mut PmAnswerQualityDto) {
    let stale_needles = [
        "没有实际工具",
        "没有工具",
        "工具证据为空",
        "工具检索记录",
        "缺少可验证的检索证据",
        "no tool",
        "missing tool",
        "without tool",
    ];
    quality.missing.retain(|item| {
        if !(item.starts_with("llm_missing_evidence:") || item.starts_with("llm_weak_claim:")) {
            return true;
        }
        let lower = item.to_ascii_lowercase();
        !stale_needles
            .iter()
            .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
    });
}

fn pm_force_synthesize_no_tool_options(timeout_secs: u64) -> agent_gateway::AgentTurnOptions {
    let mut options = agent_gateway::AgentTurnOptions {
        // Planning and parallel retrieval already performed the deep work. The
        // reduce pass should synthesize admitted evidence, not pay for another
        // high-reasoning exploration round.
        reasoning_budget: agent_gateway::InternalReasoningBudget::Fast,
        prefer_native_web_search: false,
        suppress_native_web_search: true,
        stream_timeout_secs: Some(timeout_secs),
        disable_tools: true,
        disable_provider_thinking: true,
        model_budget_stage: runtime::RuntimeModelBudgetStage::FinalSynthesis,
        ..agent_gateway::AgentTurnOptions::default()
    };
    options
        .blocked_tools
        .extend(pm_blocked_non_search_research_tools());
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
    options.system_instructions.push(
        "Internal PM synthesis turn. Do not call tools, search, browse, fetch URLs, or inspect resources. Use only the supplied user question, first-party context, admitted evidence, and notes. Return the final user-facing Markdown answer."
            .to_string(),
    );
    options
}

pub(super) fn apply_pm_depth_coverage_gate(
    quality: &mut PmAnswerQualityDto,
    plan: &serde_json::Value,
    turn_text: &str,
    probe_outcomes: &[PmProbeOutcome],
    min_parallel_agents: usize,
    min_citations_per_subtask: usize,
    min_domains_per_subtask: usize,
) -> PmDepthCoverageGateResult {
    let planned_max_probe_per_subtask = plan
        .get("parallelism")
        .and_then(|value| value.get("maxProbePerSubtask"))
        .and_then(|value| value.as_u64())
        .unwrap_or(1)
        .clamp(1, 6) as usize;
    let effective_min_parallel_agents = min_parallel_agents
        .max(1)
        .min(planned_max_probe_per_subtask.max(1));
    let mut pass_result = PmDepthCoverageGateResult::default();
    pass_result.coverage_gate.passed = true;
    pass_result.coverage_gate.min_parallel_agents = effective_min_parallel_agents;
    pass_result.coverage_gate.min_citations_per_subtask = min_citations_per_subtask.max(1);
    pass_result.coverage_gate.min_domains_per_subtask = min_domains_per_subtask.max(1);
    if pm_is_report_strategy_mode(plan) {
        return pass_result;
    }

    let decomposition_mode = plan
        .get("taskGraph")
        .and_then(|value| value.get("decompositionMode"))
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "light".to_string());
    if decomposition_mode == "none" {
        return pass_result;
    }
    let subtasks = plan
        .get("taskGraph")
        .and_then(|value| value.get("subtasks"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if subtasks.is_empty() {
        return pass_result;
    }

    let mut expected = Vec::<(String, Vec<String>, bool)>::new();
    for (idx, subtask) in subtasks.iter().enumerate() {
        let Some(obj) = subtask.as_object() else {
            continue;
        };
        let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let goal = obj
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let deliverable = obj
            .get("deliverable")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let required_evidence_type = obj
            .get("requiredEvidenceType")
            .or_else(|| obj.get("required_evidence_type"))
            .or_else(|| obj.get("evidenceType"))
            .or_else(|| obj.get("evidence_type"))
            .and_then(|v| v.as_str());
        let requires_external_probe =
            pm_domain::task_graph::pm_subtask_allows_external_probe(required_evidence_type);
        let display = if !title.is_empty() {
            title.to_string()
        } else if !deliverable.is_empty() {
            deliverable.to_string()
        } else if !goal.is_empty() {
            goal.to_string()
        } else if !id.trim().is_empty() {
            id.trim().to_string()
        } else {
            format!("subtask-{}", idx + 1)
        };
        let mut keys = Vec::<String>::new();
        for raw in [id, title, goal, deliverable] {
            let key = normalized_pm_key(raw);
            if !key.is_empty() && !keys.iter().any(|existing| existing == &key) {
                keys.push(key);
            }
        }
        expected.push((display, keys, requires_external_probe));
    }
    if expected.is_empty() {
        return pass_result;
    }

    let mut coverage = HashMap::<String, PmSubtaskCoverage>::new();
    for outcome in probe_outcomes {
        let mut candidate_keys = Vec::<String>::new();
        if let Some(id) = outcome.subtask_id.as_deref() {
            let key = normalized_pm_key(id);
            if !key.is_empty() {
                candidate_keys.push(key);
            }
        }
        if let Some(title) = outcome.subtask_title.as_deref() {
            let key = normalized_pm_key(title);
            if !key.is_empty() {
                candidate_keys.push(key);
            }
        }
        if candidate_keys.is_empty() {
            continue;
        }
        let Some(outcome_quality) = outcome.quality.as_ref() else {
            continue;
        };
        let mut matched_keys = Vec::<String>::new();
        for (_display, keys, requires_external_probe) in &expected {
            if !requires_external_probe {
                continue;
            }
            if keys.is_empty() {
                continue;
            }
            if keys
                .iter()
                .any(|key| candidate_keys.iter().any(|hit| hit == key))
            {
                matched_keys.extend(keys.iter().cloned());
            }
        }
        if matched_keys.is_empty() {
            continue;
        }
        for key in matched_keys {
            let entry = coverage.entry(key).or_default();
            entry.probe_count = entry.probe_count.saturating_add(1);
            for citation in &outcome_quality.citations {
                if !citation.trim().is_empty() {
                    entry.citations.insert(citation.clone());
                }
            }
            for domain in &outcome_quality.domains {
                if !domain.trim().is_empty() {
                    entry.domains.insert(domain.to_ascii_lowercase());
                }
            }
        }
    }

    let mut report_texts = vec![turn_text.to_string()];
    if let Some(report_json) = extract_named_json_object(turn_text, "REPORT_JSON") {
        collect_report_json_texts(&report_json, &mut report_texts);
    }
    let report_corpus = report_texts.join("\n");

    let mut subtask_gaps = Vec::<String>::new();
    let mut dimension_gaps = Vec::<String>::new();
    let min_parallel_agents = effective_min_parallel_agents;
    let mut evidence_bundles = Vec::<SubtaskEvidenceBundle>::new();
    let mut gap_subtasks = Vec::<String>::new();
    for (display, keys, requires_external_probe) in &expected {
        let mut probe_count = 0usize;
        let mut citations = 0usize;
        let mut domains = 0usize;
        let mut citation_set = HashSet::<String>::new();
        let mut domain_set = HashSet::<String>::new();
        for key in keys {
            if let Some(metric) = coverage.get(key) {
                probe_count = probe_count.max(metric.probe_count);
                citations = citations.max(metric.citations.len());
                domains = domains.max(metric.domains.len());
                citation_set.extend(metric.citations.iter().cloned());
                domain_set.extend(metric.domains.iter().cloned());
            }
        }
        let mut citations_vec: Vec<String> = citation_set.into_iter().collect();
        citations_vec.sort();
        citations_vec.truncate(8);
        let mut domains_vec: Vec<String> = domain_set.into_iter().collect();
        domains_vec.sort();
        domains_vec.truncate(8);
        evidence_bundles.push(SubtaskEvidenceBundle {
            subtask_key: normalized_pm_key(display),
            title: display.clone(),
            probe_count,
            citation_count: citations,
            domain_count: domains,
            citations: citations_vec,
            domains: domains_vec,
        });

        if *requires_external_probe
            && (probe_count < min_parallel_agents
                || citations < min_citations_per_subtask
                || domains < min_domains_per_subtask)
        {
            subtask_gaps.push(display.clone());
            gap_subtasks.push(display.clone());
            let key = format!("subtask_depth_gap:{}", normalized_pm_key(display));
            push_pm_quality_missing_once(quality, key);
            if probe_count < min_parallel_agents {
                let key = format!("subtask_probe_gap:{}", normalized_pm_key(display));
                push_pm_quality_missing_once(quality, key);
            }
        }
        if !pm_contains_dimension_text(&report_corpus, display) {
            dimension_gaps.push(display.clone());
            let key = format!("dimension_gap:{}", normalized_pm_key(display));
            push_pm_quality_missing_once(quality, key);
        }
    }

    if !subtask_gaps.is_empty() || !dimension_gaps.is_empty() {
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        if !subtask_gaps.is_empty() {
            let suggestion = format!(
                "Depth gate: each subtask must include >= {} successful parallel probes, >= {} citations and >= {} distinct domains. Missing: {}",
                min_parallel_agents,
                min_citations_per_subtask,
                min_domains_per_subtask,
                subtask_gaps.join(", ")
            );
            push_pm_quality_suggestion_once(quality, suggestion);
        }
        if !dimension_gaps.is_empty() {
            let suggestion = format!(
                "Dimension coverage gap: explicitly cover these TASK_GRAPH dimensions in final report: {}",
                dimension_gaps.join(", ")
            );
            push_pm_quality_suggestion_once(quality, suggestion);
        }
    }

    let coverage_gate = SubtaskCoverageGate {
        passed: subtask_gaps.is_empty(),
        min_parallel_agents,
        min_citations_per_subtask,
        min_domains_per_subtask,
        bundles: evidence_bundles,
        gap_subtasks: gap_subtasks.clone(),
    };
    let gap_repair_plan = if coverage_gate.passed {
        GapRepairPlan::default()
    } else {
        GapRepairPlan {
            enabled: true,
            reason: "subtask_coverage_not_met".to_string(),
            target_subtasks: gap_subtasks,
        }
    };

    PmDepthCoverageGateResult {
        enforced: !subtask_gaps.is_empty() || !dimension_gaps.is_empty(),
        subtask_gap_titles: subtask_gaps,
        dimension_gap_titles: dimension_gaps,
        coverage_gate,
        gap_repair_plan,
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct PmReportStrategyGateResult {
    pub(super) enforced: bool,
    pub(super) passed: bool,
    pub(super) missing_checks: Vec<String>,
    pub(super) matched_metric_count: usize,
    pub(super) has_segment_strategy: bool,
    pub(super) has_experiment_plan: bool,
    pub(super) has_guardrails: bool,
    pub(super) has_opportunity_cohorts: bool,
    pub(super) respects_anti_patterns: bool,
}

fn pm_text_has_any_ci(text: &str, tokens: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    tokens.iter().any(|token| lower.contains(token))
}

fn pm_text_has_any_raw(text: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| text.contains(token))
}

fn pm_strategy_overlap_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let lower = text.to_ascii_lowercase();
    for token in lower.split(|ch: char| {
        ch.is_whitespace()
            || ch.is_ascii_punctuation()
            || matches!(
                ch,
                '，' | '。' | '；' | '：' | '、' | '（' | '）' | '【' | '】' | '“' | '”'
            )
    }) {
        let token = token.trim();
        let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        if (token.chars().count() >= 3 || (has_digit && !token.is_empty()))
            && !matches!(
                token,
                "the"
                    | "and"
                    | "for"
                    | "with"
                    | "that"
                    | "this"
                    | "user"
                    | "users"
                    | "metric"
                    | "metrics"
                    | "strategy"
                    | "experiment"
                    | "cohort"
                    | "segment"
                    | "report"
            )
        {
            if !out.iter().any(|item| item == token) {
                out.push(token.to_string());
            }
        }
    }
    for ch in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            let s = ch.to_string();
            if !out.iter().any(|item| item == &s) {
                out.push(s);
            }
        }
    }
    out
}

fn pm_common_metric_or_generic_token(token: &str) -> bool {
    matches!(
        token,
        "roi"
            | "mrr"
            | "arr"
            | "cac"
            | "ltv"
            | "dau"
            | "arpu"
            | "cpi"
            | "cvr"
            | "ctr"
            | "nps"
            | "gmv"
            | "activation"
            | "churn"
            | "retention"
            | "conversion"
            | "revenue"
            | "cost"
            | "users"
            | "user"
            | "客户"
            | "用户"
            | "人群"
            | "场景"
    )
}

fn pm_report_strategy_cohort_covered(answer: &str, cohort: &str) -> bool {
    let answer_lower = answer.to_ascii_lowercase();
    let cohort_lower = cohort.to_ascii_lowercase();
    let normalized_answer = answer_lower
        .split_whitespace()
        .collect::<String>()
        .replace('＋', "+");
    let normalized_cohort = cohort_lower
        .split_whitespace()
        .collect::<String>()
        .replace('＋', "+");
    if !normalized_cohort.is_empty() && normalized_answer.contains(&normalized_cohort) {
        return true;
    }
    let cohort_tokens = pm_strategy_overlap_tokens(cohort);
    if !cohort_tokens.is_empty() {
        let matched_tokens = cohort_tokens
            .iter()
            .filter(|token| {
                answer_lower.contains(token.as_str()) || answer.contains(token.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let numeric_tokens = cohort_tokens
            .iter()
            .filter(|token| token.chars().any(|ch| ch.is_ascii_digit()))
            .cloned()
            .collect::<Vec<_>>();
        let descriptive_tokens = cohort_tokens
            .iter()
            .filter(|token| {
                !token.chars().any(|ch| ch.is_ascii_digit())
                    && !pm_common_metric_or_generic_token(token)
            })
            .cloned()
            .collect::<Vec<_>>();
        let numeric_ok = numeric_tokens.is_empty()
            || numeric_tokens.iter().all(|token| {
                answer_lower.contains(token.as_str()) || answer.contains(token.as_str())
            });
        let descriptive_hits = descriptive_tokens
            .iter()
            .filter(|token| {
                answer_lower.contains(token.as_str()) || answer.contains(token.as_str())
            })
            .count();
        let descriptive_ok = if descriptive_tokens.is_empty() {
            true
        } else {
            descriptive_hits >= descriptive_tokens.len().min(2)
        };
        let required = if numeric_tokens.is_empty() && descriptive_tokens.is_empty() {
            cohort_tokens.len().min(3).max(1)
        } else {
            2
        };
        if numeric_ok && descriptive_ok && matched_tokens.len() >= required {
            return true;
        }
    }
    false
}

fn pm_metric_tokens_present_in_question(question: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = question.to_ascii_lowercase();
    for token in [
        "roi",
        "revenue",
        "cost",
        "ltv",
        "cac",
        "mrr",
        "arr",
        "activation",
        "churn",
        "retention",
        "conversion",
        "nps",
        "gmv",
    ] {
        if lower.contains(token) {
            out.push(token.to_string());
        }
    }
    for (raw, label) in [
        ("次留", "次留"),
        ("留存", "留存"),
        ("时长", "时长"),
        ("成本", "成本"),
        ("收入", "收入"),
    ] {
        if question.contains(raw) {
            out.push(label.to_string());
        }
    }
    for caps in pm_dynamic_metric_regex().captures_iter(question) {
        let Some(name) = caps.name("name").map(|m| m.as_str()) else {
            continue;
        };
        if let Some(cleaned) = pm_normalize_metric_token(name) {
            if !out.iter().any(|item| item.eq_ignore_ascii_case(&cleaned)) {
                out.push(cleaned);
            }
        }
    }
    for caps in pm_dynamic_compact_metric_regex().captures_iter(question) {
        let Some(name) = caps.name("name").map(|m| m.as_str()) else {
            continue;
        };
        if let Some(cleaned) = pm_normalize_metric_token(name) {
            if !out.iter().any(|item| item.eq_ignore_ascii_case(&cleaned)) {
                out.push(cleaned);
            }
        }
    }
    out
}

fn pm_dynamic_metric_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?P<name>[A-Za-z][A-Za-z0-9_./+-]{1,32})(?:\s*(?:=|:|：)\s*|\s+)(?P<value>\$?\d[\d,]*(?:\.\d+)?%?(?:[kmb])?)",
        )
        .expect("valid pm dynamic metric regex")
    })
}

fn pm_dynamic_compact_metric_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?P<name>[A-Za-z][A-Za-z._+-]*[A-Za-z])(?P<value>\$?\d[\d,]*(?:\.\d+)?%?(?:[kmb])?)",
        )
        .expect("valid pm dynamic compact metric regex")
    })
}

fn pm_normalize_metric_token(raw: &str) -> Option<String> {
    let token = raw
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .to_ascii_lowercase();
    if !(2..=32).contains(&token.chars().count()) {
        return None;
    }
    if matches!(
        token.as_str(),
        "the"
            | "and"
            | "for"
            | "with"
            | "past"
            | "last"
            | "next"
            | "current"
            | "previous"
            | "day"
            | "days"
            | "week"
            | "weeks"
            | "month"
            | "months"
            | "year"
            | "years"
            | "report"
            | "data"
            | "users"
            | "user"
            | "trial"
    ) {
        return None;
    }
    Some(token)
}

fn pm_metric_tokens_from_first_party(first_party: Option<&serde_json::Value>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(metrics) = first_party
        .and_then(|value| value.get("metrics"))
        .and_then(|value| value.as_array())
    else {
        return out;
    };
    for item in metrics {
        let Some(raw) = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| item.as_str())
        else {
            continue;
        };
        if let Some(cleaned) = pm_normalize_metric_token(raw) {
            if !out.iter().any(|item| item.eq_ignore_ascii_case(&cleaned)) {
                out.push(cleaned);
            }
        }
    }
    out
}

pub(super) fn apply_pm_report_strategy_quality_gate(
    quality: &mut PmAnswerQualityDto,
    plan: &serde_json::Value,
    question: &str,
    answer_text: &str,
) -> PmReportStrategyGateResult {
    if !pm_is_report_strategy_mode(plan) {
        return PmReportStrategyGateResult {
            enforced: false,
            passed: true,
            ..PmReportStrategyGateResult::default()
        };
    }

    let mut result = PmReportStrategyGateResult {
        enforced: true,
        passed: true,
        ..PmReportStrategyGateResult::default()
    };
    let answer = answer_text.trim();
    if answer.is_empty() {
        result
            .missing_checks
            .push("empty_strategy_answer".to_string());
    }

    let diagnostic_tokens = [
        "durationMs",
        "toolCallCount",
        "contentChars",
        "sourceSlotBudgetSecs",
        "pipelineTimeoutSecs",
        "routeAllowlist",
        "EXEC_CONSTRAINTS",
        "TASK_GRAPH",
        "probeCandidateCount",
    ];
    if diagnostic_tokens.iter().any(|token| answer.contains(token)) {
        result
            .missing_checks
            .push("tool_diagnostic_leaked_into_answer".to_string());
    }

    let first_party = plan
        .get("reportStrategy")
        .and_then(|value| value.get("firstPartyEvidenceJson"));
    let mut metric_tokens = pm_metric_tokens_present_in_question(question);
    for token in pm_metric_tokens_from_first_party(first_party) {
        if !metric_tokens
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&token))
        {
            metric_tokens.push(token);
        }
    }
    let answer_lower = answer.to_ascii_lowercase();
    result.matched_metric_count = metric_tokens
        .iter()
        .filter(|token| answer_lower.contains(&token.to_ascii_lowercase()))
        .count();
    let required_metric_hits = metric_tokens.len().min(4);
    if required_metric_hits > 0 && result.matched_metric_count < required_metric_hits {
        result
            .missing_checks
            .push("insufficient_first_party_metric_usage".to_string());
    }

    result.has_segment_strategy = pm_text_has_any_raw(
        answer,
        &[
            "分层",
            "人群",
            "用户",
            "客户",
            "客群",
            "场景",
            "新用户",
            "老用户",
            "高价值",
            "低活跃",
        ],
    ) || pm_text_has_any_ci(
        answer,
        &[
            "segment",
            "cohort",
            "persona",
            "customer group",
            "scenario",
            "user group",
            "playbook",
        ],
    );
    if !result.has_segment_strategy {
        result
            .missing_checks
            .push("missing_segment_level_strategy".to_string());
    }

    result.has_experiment_plan = pm_text_has_any_raw(
        answer,
        &[
            "实验", "灰度", "规则", "触发", "对照", "A/B", "ab", "上线", "样本",
        ],
    ) || pm_text_has_any_ci(
        answer,
        &["experiment", "holdout", "rollout", "trigger", "rule"],
    );
    if !result.has_experiment_plan {
        result
            .missing_checks
            .push("missing_experiment_ready_plan".to_string());
    }

    result.has_guardrails = pm_text_has_any_raw(
        answer,
        &[
            "保护指标",
            "护栏",
            "止损",
            "停止",
            "回滚",
            "阈值",
            "底线",
            "不低于",
            "不高于",
            "不能下降",
            "kill",
        ],
    ) || pm_text_has_any_ci(
        answer,
        &[
            "guardrail",
            "kill criteria",
            "stop condition",
            "rollback",
            "threshold",
            "holdout",
        ],
    );
    if !result.has_guardrails {
        result
            .missing_checks
            .push("missing_guardrails_or_kill_criteria".to_string());
    }

    let opportunity_count = first_party
        .and_then(|value| value.get("opportunityCohorts"))
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let covered_opportunity_count = first_party
        .and_then(|value| value.get("opportunityCohorts"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("cohort").and_then(serde_json::Value::as_str))
                .filter(|cohort| pm_report_strategy_cohort_covered(answer, cohort))
                .count()
        })
        .unwrap_or(0);
    let required_opportunity_count = opportunity_count.min(2);
    result.has_opportunity_cohorts =
        opportunity_count == 0 || covered_opportunity_count >= required_opportunity_count;
    if !result.has_opportunity_cohorts {
        result
            .missing_checks
            .push("missing_extracted_opportunity_cohorts".to_string());
    }

    let failed_experiment_count = first_party
        .and_then(|value| value.get("failedExperiments"))
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let anti_pattern_count = first_party
        .and_then(|value| value.get("antiPatterns"))
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    result.respects_anti_patterns = failed_experiment_count + anti_pattern_count == 0
        || answer.contains("不要")
        || answer.contains("不能")
        || answer.contains("避免")
        || answer.contains("不加")
        || answer.contains("不再")
        || answer.contains("不继续")
        || answer.contains("不应")
        || answer.contains("不是")
        || answer_lower.contains("avoid")
        || answer_lower.contains("not ")
        || answer_lower.contains("do not");
    if !result.respects_anti_patterns {
        result
            .missing_checks
            .push("missing_failed_experiment_or_anti_pattern_lessons".to_string());
    }

    if !result.missing_checks.is_empty() {
        result.passed = false;
        quality.passed = false;
        quality.deliverable = false;
        quality.quality_level = "low".to_string();
        for check in &result.missing_checks {
            push_pm_quality_missing_once(quality, format!("report_strategy:{check}"));
        }
        push_pm_quality_suggestion_once(
            quality,
            "Report strategy mode requires first-party metric usage, segment-level recommendations, experiment rules, guardrails, and no tool diagnostics in the visible answer."
                .to_string(),
        );
    } else if quality.quality_level == "low" {
        quality.passed = true;
        quality.deliverable = true;
        quality.quality_level = if quality.citation_count > 0 {
            "high".to_string()
        } else {
            "partial".to_string()
        };
    } else {
        quality.passed = true;
        quality.deliverable = true;
    }
    result
}

pub(super) fn pm_retry_strategy(next_attempt: usize) -> PmRepairStrategy {
    match next_attempt {
        2 => PmRepairStrategy::SwitchSource,
        3 => PmRepairStrategy::SwitchQuery,
        _ if next_attempt % 2 == 0 => PmRepairStrategy::SwitchSource,
        _ => PmRepairStrategy::SwitchQuery,
    }
}

pub(super) fn pm_source_slot_timeout_for_strategy(
    strategy: PmRepairStrategy,
    runtime_budget: &PmTimeoutBudget,
) -> u64 {
    match strategy {
        PmRepairStrategy::SwitchSource | PmRepairStrategy::SwitchQuery => {
            runtime_budget.source_slot_search_secs
        }
        PmRepairStrategy::BrowserFallback => runtime_budget.source_slot_search_secs,
        // Degraded summary should converge quickly instead of expanding retrieval.
        PmRepairStrategy::DegradedSummary => pm_force_synth_turn_timeout_secs(),
    }
}

pub(super) fn build_runtime_error_quality() -> PmAnswerQualityDto {
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
        conflict_reason: "runtime execution failed before triad alignment".to_string(),
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
        missing: vec![
            "runtime_error".to_string(),
            "missing_tool_retrieval".to_string(),
            "missing_citations".to_string(),
        ],
        suggestions: vec![
            "Retry with alternate retrieval strategy and simplify query.".to_string(),
            "Switch source type and continue evidence collection.".to_string(),
        ],
    }
}

pub(super) fn degrade_pm_quality_with_reason(
    mut quality: PmAnswerQualityDto,
    reason: &str,
    suggestion: &str,
) -> PmAnswerQualityDto {
    quality.passed = false;
    if !pm_is_deliverable_quality(&quality) {
        quality.quality_level = "low".to_string();
    } else if quality.quality_level == "high" || quality.quality_level.is_empty() {
        quality.quality_level = "partial".to_string();
    }
    if !quality.missing.iter().any(|item| item == reason) {
        quality.missing.push(reason.to_string());
    }
    if !quality.suggestions.iter().any(|item| item == suggestion) {
        quality.suggestions.push(suggestion.to_string());
    }
    quality
}

fn requirement_state_gap_labels(
    state: &pm_domain::requirement_state::RequirementState,
) -> Vec<&'static str> {
    let mut gaps = Vec::new();
    if !state
        .problem_frame
        .as_ref()
        .is_some_and(|frame| frame.confirmed)
    {
        gaps.push("confirmed problem frame");
    }
    if !state.stakeholders.iter().any(|item| item.confirmed) {
        gaps.push("confirmed primary stakeholder");
    }
    if !state.jobs.iter().any(|item| item.confirmed) {
        gaps.push("confirmed job to be done");
    }
    if !state
        .desired_outcomes
        .iter()
        .any(|outcome| outcome.measure.is_some())
    {
        gaps.push("measurable outcome");
    }
    if state.scope.included.is_empty() {
        gaps.push("included scope");
    }
    if !state
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.testable)
    {
        gaps.push("testable acceptance criterion");
    }
    if state.assumptions.iter().any(|assumption| {
        assumption.importance >= 0.7
            && assumption.uncertainty >= 0.5
            && matches!(
                assumption.status,
                pm_domain::requirement_state::AssumptionStatus::Open
            )
            && assumption.falsification_test.is_none()
    }) {
        gaps.push("validation plan for a critical assumption");
    }
    gaps
}

fn enforce_requirement_state_delivery_gate(
    turn: &mut TurnResult,
    quality: &mut PmAnswerQualityDto,
    state: &pm_domain::requirement_state::RequirementState,
) {
    crate::behavior_trace("PM-004");
    use pm_domain::requirement_state::{planning_gate, RequirementPlanningGate};

    let gate = planning_gate(state);
    if matches!(gate, RequirementPlanningGate::ReadyForDelivery) {
        return;
    }

    quality.passed = false;
    quality.deliverable = false;
    quality.quality_level = "needs_clarification".to_string();
    if !quality
        .missing
        .iter()
        .any(|item| item == "requirement_state_not_ready")
    {
        quality
            .missing
            .push("requirement_state_not_ready".to_string());
    }

    let next_question = match &gate {
        RequirementPlanningGate::Ask(question) => Some(question.question.clone()),
        RequirementPlanningGate::ContinueResearch => {
            pm_domain::requirement_state::next_question(state).map(|question| question.question)
        }
        RequirementPlanningGate::ReadyForDelivery => None,
    };
    if let Some(question) = next_question.as_ref() {
        let suggestion = format!("Resolve the highest-value open question: {question}");
        if !quality.suggestions.iter().any(|item| item == &suggestion) {
            quality.suggestions.push(suggestion);
        }
    }

    if matches!(gate, RequirementPlanningGate::Ask(_))
        && next_question
            .as_ref()
            .is_some_and(|question| turn.text.contains(question))
    {
        return;
    }
    let gaps = requirement_state_gap_labels(state);
    let cjk = contains_cjk(&turn.text);
    let status = if cjk {
        let gaps = if gaps.is_empty() {
            "仍有高价值问题需要确认".to_string()
        } else {
            gaps.join("、")
        };
        let question = next_question
            .map(|question| format!("\n\n下一问：{question}"))
            .unwrap_or_default();
        format!(
            "\n\n## 需求状态\n\n当前内容保留为需求简报，尚不能标记为可评审方案。待补齐：{gaps}。{question}"
        )
    } else {
        let gaps = if gaps.is_empty() {
            "a remaining high-value question".to_string()
        } else {
            gaps.join(", ")
        };
        let question = next_question
            .map(|question| format!("\n\nNext question: {question}"))
            .unwrap_or_default();
        format!(
            "\n\n## Requirement status\n\nThis remains a Requirement Brief and is not review-ready. Missing: {gaps}.{question}"
        )
    };
    if !turn.text.contains("## Requirement status") && !turn.text.contains("## 需求状态") {
        turn.text.push_str(&status);
    }
}

fn pm_requirement_evidence_delta(quality: &PmAnswerQualityDto) -> serde_json::Value {
    let links = quality
        .evidence_tree
        .iter()
        .filter_map(|node| {
            let evidence_ids = node
                .evidences
                .iter()
                .filter(|evidence| !evidence.url.trim().is_empty())
                .map(|evidence| sha256_hex(evidence.url.trim().to_ascii_lowercase().as_str()))
                .collect::<Vec<_>>();
            if node.claim.trim().is_empty() || evidence_ids.is_empty() {
                return None;
            }
            let support = match node.status.as_str() {
                "confirmed" => "supported",
                "contradicted" => "contradicted",
                _ => "inconclusive",
            };
            Some(serde_json::json!({
                "claim": node.claim,
                "evidenceIds": evidence_ids,
                "support": support,
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "requirementDelta": {
            "evidenceLinks": links,
        }
    })
}

pub(super) async fn finalize_pm_orchestration_result(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    run_id: &str,
    session_id: &str,
    turn: TurnResult,
    quality: PmAnswerQualityDto,
) -> Result<(TurnResult, PmAnswerQualityDto), GatewayError> {
    let mut turn = turn;
    let mut quality = quality;
    if !quality.passed && quality.quality_level == "high" {
        quality.quality_level = "partial".to_string();
    }
    let (final_text, repaired) =
        finalize_pm_answer_text_with_repair_flag(&turn.text, &quality, &turn.tool_calls);
    turn.text = final_text;
    if repaired {
        quality.deliverable = true;
        if quality.quality_level == "low" {
            quality.quality_level = "partial".to_string();
        }
        if !quality
            .missing
            .iter()
            .any(|item| item == "auto_depth_repair_applied")
        {
            quality
                .missing
                .push("auto_depth_repair_applied".to_string());
        }
        let suggestion =
            "Auto depth-repair converted partial evidence into a structured decision-ready summary."
                .to_string();
        if !quality.suggestions.iter().any(|item| item == &suggestion) {
            quality.suggestions.push(suggestion);
        }
    }
    persist_pm_evidence_graph(db, tenant_id, session_id, &turn, &quality)
        .await
        .map_err(|error| {
            GatewayError::Internal(format!(
                "PM evidence ledger persistence failed before delivery: {error}"
            ))
        })?;
    let evidence_delta = pm_requirement_evidence_delta(&quality);
    if evidence_delta["requirementDelta"]["evidenceLinks"]
        .as_array()
        .is_some_and(|links| !links.is_empty())
    {
        crate::semantic_kernel_store::persist_pm_requirement_state_delta(
            db,
            tenant_id,
            session_id,
            &format!("{run_id}:evidence"),
            "",
            &evidence_delta,
        )
        .await
        .map_err(|error| {
            GatewayError::Internal(format!(
                "PM evidence delta persistence failed before delivery: {error}"
            ))
        })?;
    }
    let requirement_state =
        crate::semantic_kernel_store::load_pm_requirement_state(db, tenant_id, session_id)
            .await
            .map_err(|error| {
                GatewayError::Internal(format!(
                    "PM requirement-state load failed before delivery: {error}"
                ))
            })?
            .ok_or_else(|| {
                GatewayError::Internal(
            "PM requirement state is missing before final delivery; refusing an ungoverned result"
                .to_string(),
        )
            })?;
    enforce_requirement_state_delivery_gate(&mut turn, &mut quality, &requirement_state);
    persist_pm_claim_and_conflict_records(db, tenant_id, run_id, &quality).await;
    let quality_score = pm_quality_delivery_score(&quality);
    let task_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT task_id FROM pm_research_runs WHERE tenant_id = ? AND run_id = ? LIMIT 1",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();
    let missing_json = serde_json::json!(quality.missing.clone());
    let suggestions_json = serde_json::json!(quality.suggestions.clone());
    upsert_pm_quality_gate_metrics(
        db,
        run_id,
        tenant_id,
        task_id.as_deref(),
        Some(session_id),
        quality.passed,
        quality_score,
        quality.tool_call_count,
        quality.citation_count,
        quality.domain_count,
        quality.claim_count,
        quality.claim_alignment_ok,
        quality.triad_total_claims,
        quality.triad_aligned_claims,
        quality.triad_coverage,
        quality.conflict_adjudicated,
        quality.conflict_confidence,
        Some(&missing_json),
        Some(&suggestions_json),
    )
    .await;
    Ok((turn, quality))
}

fn pm_local_compact_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pm_local_take_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect::<String>()
}

fn pm_local_clean_label(raw: &str, max_chars: usize) -> Option<String> {
    let mut value = pm_local_compact_ws(raw)
        .replace("...", "")
        .replace('…', "")
        .trim()
        .to_string();
    let lower = value.to_ascii_lowercase();
    if value.is_empty()
        || lower.contains("detected first-party evidence")
        || lower.contains("runtime execution failed")
        || lower.contains("durationms")
        || lower.contains("toolcallcount")
        || lower.contains("+1 more")
        || lower.contains("+2 more")
        || lower.contains("+3 more")
        || lower.contains("+ more")
    {
        return None;
    }
    if value.chars().count() > max_chars {
        value = pm_local_take_chars(&value, max_chars);
    }
    let value = value
        .trim_matches(|ch: char| ch.is_ascii_punctuation() || matches!(ch, '：' | ':' | '，'))
        .trim()
        .to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn pm_push_unique_local(out: &mut Vec<String>, raw: impl AsRef<str>, max_chars: usize) {
    let Some(value) = pm_local_clean_label(raw.as_ref(), max_chars) else {
        return;
    };
    if !out.iter().any(|item| item.eq_ignore_ascii_case(&value)) {
        out.push(value);
    }
}

fn pm_local_first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(serde_json::Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

fn pm_local_collect_labels(
    evidence: &serde_json::Value,
    key: &str,
    cap: usize,
    max_chars: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(items) = evidence.get(key).and_then(serde_json::Value::as_array) else {
        return out;
    };
    for item in items.iter().take(cap.saturating_mul(3).max(cap)) {
        if let Some(text) = item.as_str() {
            pm_push_unique_local(&mut out, text, max_chars);
        } else if key == "metrics" {
            if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
                let value = item
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if value.trim().is_empty() {
                    pm_push_unique_local(&mut out, name, max_chars);
                } else {
                    pm_push_unique_local(&mut out, format!("{name}={value}"), max_chars);
                }
            }
        } else if let Some(primary) =
            pm_local_first_string(item, &["cohort", "name", "title", "label", "text"])
        {
            let secondary =
                pm_local_first_string(item, &["why", "lesson", "strategyHint", "reason"]);
            let label = if let Some(secondary) = secondary {
                format!("{primary}: {secondary}")
            } else {
                primary
            };
            pm_push_unique_local(&mut out, label, max_chars);
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn pm_local_join_or(items: &[String], fallback: &str) -> String {
    if items.is_empty() {
        fallback.to_string()
    } else {
        items.iter().take(5).cloned().collect::<Vec<_>>().join("、")
    }
}

fn pm_local_collect_guardrail_clauses(input: &str, cap: usize) -> Vec<String> {
    let markers = [
        "不能下降",
        "不能上升",
        "不能降低",
        "不下降",
        "不低于",
        "不高于",
        "保护指标",
        "约束",
        "底线",
        "guardrail",
        "must not",
        "cannot",
        "should not",
        "not decrease",
        "not increase",
        "constraint",
    ];
    let mut out = Vec::new();
    for clause in input.split(['。', '；', ';', '.', '\n']) {
        let lower = clause.to_ascii_lowercase();
        if markers
            .iter()
            .any(|marker| clause.contains(marker) || lower.contains(marker))
        {
            pm_push_unique_local(&mut out, clause, 160);
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn pm_local_collect_existing_mechanism_clauses(input: &str, cap: usize) -> Vec<String> {
    let markers = [
        "当前已有",
        "已有能力",
        "已有玩法",
        "已存在",
        "当前已经存在",
        "existing mechanisms",
        "existing capabilities",
        "current mechanics",
        "current capabilities",
        "already have",
    ];
    let mut out = Vec::new();
    for clause in input.split(['。', '；', ';', '.', '\n']) {
        let lower = clause.to_ascii_lowercase();
        if markers
            .iter()
            .any(|marker| clause.contains(marker) || lower.contains(marker))
        {
            pm_push_unique_local(&mut out, clause, 180);
        }
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn build_pm_deterministic_strategy_package_fallback(
    user_message: &str,
    failure_reason: &str,
    attempt: usize,
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> String {
    let cjk_mode = contains_cjk(user_message);
    let first_party = extract_pm_first_party_evidence(user_message);
    let context_terms = pm_local_collect_labels(&first_party, "contextTerms", 4, 80);
    let objectives = pm_local_collect_labels(&first_party, "objectives", 6, 100);
    let mut guardrails = pm_local_collect_labels(&first_party, "guardrails", 8, 120);
    for guardrail in pm_local_collect_guardrail_clauses(user_message, 8) {
        pm_push_unique_local(&mut guardrails, guardrail, 160);
    }
    let metrics = pm_local_collect_labels(&first_party, "metrics", 10, 80);
    let cohorts = pm_local_collect_labels(&first_party, "opportunityCohorts", 6, 150);
    let mut existing = pm_local_collect_labels(&first_party, "existingMechanics", 6, 120);
    for mechanism in pm_local_collect_existing_mechanism_clauses(user_message, 6) {
        pm_push_unique_local(&mut existing, mechanism, 180);
    }
    let failed = pm_local_collect_labels(&first_party, "failedExperiments", 5, 140);
    let anti_patterns = pm_local_collect_labels(&first_party, "antiPatterns", 6, 120);
    let snippets = pm_local_collect_labels(&first_party, "rawEvidenceSnippets", 4, 170);
    let question_excerpt = pm_local_clean_label(user_message, 260).unwrap_or_else(|| {
        if cjk_mode {
            "用户提出了一个需要基于上下文判断的产运问题".to_string()
        } else {
            "The user asked a product/operations question that needs contextual judgment"
                .to_string()
        }
    });
    tracing::error!(
        attempt = attempt,
        observed_tool_calls = observed_tool_calls.len(),
        failure_reason = %failure_reason,
        first_party_metric_count = metrics.len(),
        first_party_objective_count = objectives.len(),
        first_party_cohort_count = cohorts.len(),
        "PM local first-party synthesis fallback reached after LLM synthesis failed"
    );
    if cjk_mode {
        let mut signal_lines = Vec::new();
        if !context_terms.is_empty() {
            signal_lines.push(format!("- 业务/场景：{}", context_terms.join("、")));
        }
        if !objectives.is_empty() {
            signal_lines.push(format!("- 目标：{}", objectives.join("、")));
        }
        if !metrics.is_empty() {
            signal_lines.push(format!("- 关键指标：{}", metrics.join("、")));
        }
        if !guardrails.is_empty() {
            signal_lines.push(format!("- 保护线：{}", guardrails.join("、")));
        }
        if !existing.is_empty() {
            signal_lines.push(format!("- 已有能力/机制：{}", existing.join("、")));
        }
        if !failed.is_empty() {
            signal_lines.push(format!("- 已验证不优方向：{}", failed.join("、")));
        }
        if !anti_patterns.is_empty() {
            signal_lines.push(format!("- 需要避免：{}", anti_patterns.join("、")));
        }
        if signal_lines.is_empty() {
            signal_lines.push(format!("- 问题摘要：{question_excerpt}"));
        }

        let objective_text = pm_local_join_or(&objectives, "核心业务目标");
        let metric_text = pm_local_join_or(&metrics, "主指标、过程指标和长期指标");
        let guardrail_text = pm_local_join_or(&guardrails, "用户体验、长期留存、成本和质量保护线");
        let mut action_lines = Vec::new();
        if cohorts.is_empty() {
            action_lines.push(format!(
                "- 先把问题拆成可对比的人群/场景/链路，不做统一策略；每个动作都必须说明它预计拉动「{objective_text}」里的哪一项。"
            ));
            action_lines.push(format!(
                "- 用「{metric_text}」做优先级排序：先处理高损耗、高弹性、可快速验证的环节，避免为了单一指标牺牲保护线。"
            ));
        } else {
            for cohort in cohorts.iter().take(4) {
                action_lines.push(format!(
                    "- 针对「{cohort}」单独设计触发规则、资源强度和退出条件；不要把它和其他人群混在同一套策略里评估。"
                ));
            }
        }
        action_lines.push(format!(
            "- 所有方案都保留对照组或 holdout，用「{metric_text}」看收益，用「{guardrail_text}」做止损。"
        ));
        action_lines.push(
            "- 参考资料只用于辅助判断；最终放量以一手数据、对照实验和保护指标是否稳定为准。"
                .to_string(),
        );

        let mut validation_lines = vec![
            "- 每个策略必须写清：目标人群、触发条件、实验组/对照组、观察窗口、主指标、保护指标、停止条件。"
                .to_string(),
            format!(
                "- 优先判断是否同时满足「{objective_text}」和「{guardrail_text}」，只改善单个短期数字但伤害保护线的方案直接淘汰。"
            ),
            "- 先做小流量，稳定后再扩大；如果核心保护指标连续恶化，立即回滚到原策略。".to_string(),
        ];
        if !snippets.is_empty() {
            validation_lines.push(format!(
                "- 复盘时优先回看这些一手线索：{}",
                snippets.join("；")
            ));
        }

        return format!(
            "## 先给可执行结论\n\n\
这版先基于你提供的一手信息做深度归纳。核心原则是：先保护已验证有效的部分，再集中处理最可能拉动目标指标的薄弱环节。\n\n\
## 一手信号\n{}\n\n\
## 建议动作\n{}\n\n\
## 验证与保护线\n{}\n\n\
## 后续验证重点\n- 只补与当前人群、指标和机制直接相关的案例或基准，不用泛泛行业资料。\n- 如果参考资料与一手数据冲突，以一手数据和实验结果为准。",
            signal_lines.join("\n"),
            action_lines.join("\n"),
            validation_lines.join("\n")
        );
    }
    let objective_text = if objectives.is_empty() {
        "the core business objective".to_string()
    } else {
        objectives
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let metric_text = if metrics.is_empty() {
        "primary, process, and long-term metrics".to_string()
    } else {
        metrics
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let guardrail_text = if guardrails.is_empty() {
        "user experience, long-term retention, cost, and quality guardrails".to_string()
    } else {
        guardrails
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut signal_lines = Vec::new();
    if !context_terms.is_empty() {
        signal_lines.push(format!("- Context: {}", context_terms.join(", ")));
    }
    if !objectives.is_empty() {
        signal_lines.push(format!("- Objectives: {}", objectives.join(", ")));
    }
    if !metrics.is_empty() {
        signal_lines.push(format!("- Metrics: {}", metrics.join(", ")));
    }
    if !guardrails.is_empty() {
        signal_lines.push(format!("- Guardrails: {}", guardrails.join(", ")));
    }
    if !existing.is_empty() {
        signal_lines.push(format!("- Existing mechanisms: {}", existing.join(", ")));
    }
    if !failed.is_empty() {
        signal_lines.push(format!("- Prior lessons: {}", failed.join(", ")));
    }
    if !anti_patterns.is_empty() {
        signal_lines.push(format!("- Avoid: {}", anti_patterns.join(", ")));
    }
    if signal_lines.is_empty() {
        signal_lines.push(format!("- Question summary: {question_excerpt}"));
    }
    let mut action_lines = Vec::new();
    if cohorts.is_empty() {
        action_lines.push(format!(
            "- Split the work by cohort, scenario, or funnel step; every action must say which part of {objective_text} it is expected to move."
        ));
        action_lines.push(format!(
            "- Prioritize the largest leakage or highest-elasticity segment using {metric_text}; do not optimize one metric at the expense of the guardrails."
        ));
    } else {
        for cohort in cohorts.iter().take(4) {
            action_lines.push(format!(
                "- For {cohort}, define its own trigger rule, treatment intensity, and exit condition instead of mixing it into a one-size-fits-all policy."
            ));
        }
    }
    action_lines.push(format!(
        "- Keep a control or holdout for each change; use {metric_text} for upside and {guardrail_text} for stop conditions."
    ));
    action_lines.push(
        "- Treat reference material as supporting context only; scale decisions based on first-party data, controlled experiments, and stable guardrails."
            .to_string(),
    );
    let mut validation_lines = vec![
        "- Each experiment needs a target cohort, trigger, treatment/control split, observation window, primary metric, guardrails, and kill criteria."
            .to_string(),
        format!(
            "- Scale only when it improves {objective_text} without breaching {guardrail_text}."
        ),
    ];
    if !snippets.is_empty() {
        validation_lines.push(format!(
            "- Re-check these first-party clues during review: {}",
            snippets.join("; ")
        ));
    }
    format!(
        "## Actionable Conclusion\n\n\
This draft is grounded in the first-party context you provided. The safest path is to protect what already works, then concentrate experiments on the highest-leverage weak spots.\n\n\
## First-Party Signals\n{}\n\n\
## Recommended Actions\n{}\n\n\
## Validation And Guardrails\n{}\n\n\
## Validation Focus\n- Add only references that directly match the current cohorts, metrics, and mechanisms.\n- If references conflict with first-party data, prioritize first-party data and controlled experiment results.",
        signal_lines.join("\n"),
        action_lines.join("\n"),
        validation_lines.join("\n")
    )
}

pub(super) fn build_pm_local_strategy_synthesis_turn(
    session_id: &str,
    model: &str,
    user_message: &str,
    failure_reason: &str,
    attempt: usize,
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> TurnResult {
    TurnResult {
        session_id: session_id.to_string(),
        text: build_pm_deterministic_strategy_package_fallback(
            user_message,
            failure_reason,
            attempt,
            observed_tool_calls,
        ),
        thinking: None,
        tool_calls: observed_tool_calls.to_vec(),
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
    }
}

fn pm_last_chance_synth_timeout_secs() -> u64 {
    pm_env_u64("PM_LAST_CHANCE_SYNTH_TIMEOUT_SECS", 90).clamp(45, 180)
}

fn pm_synthesis_total_budget_secs() -> u64 {
    pm_env_u64(
        "PM_FORCE_SYNTH_TOTAL_BUDGET_SECS",
        pm_force_synth_turn_timeout_secs().min(150),
    )
    .clamp(90, 180)
}

fn pm_synthesis_recovery_reserve_secs(total_budget_secs: u64) -> u64 {
    let upper = (total_budget_secs / 2).max(30);
    pm_env_u64("PM_FORCE_SYNTH_RECOVERY_RESERVE_SECS", 60).clamp(30, upper)
}

pub(super) fn merge_pm_streamed_answer_parts(existing: &str, continuation: &str) -> String {
    let existing = existing.trim_end();
    let continuation = continuation.trim_start();
    if existing.is_empty() {
        return continuation.to_string();
    }
    if continuation.is_empty() {
        return existing.to_string();
    }
    if continuation.contains(existing) {
        return continuation.to_string();
    }
    if existing.contains(continuation) {
        return existing.to_string();
    }

    let left = existing.chars().collect::<Vec<_>>();
    let right = continuation.chars().collect::<Vec<_>>();
    let max_overlap = left.len().min(right.len()).min(4000);
    let overlap = (3..=max_overlap)
        .rev()
        .find(|size| left[left.len() - size..] == right[..*size])
        .unwrap_or(0);
    if overlap > 0 {
        let suffix = right[overlap..].iter().collect::<String>();
        format!("{existing}{suffix}")
    } else {
        format!("{existing}\n\n{continuation}")
    }
}

pub(super) fn build_pm_synthesis_continuation_prompt(
    user_message: &str,
    synthesis_context: &str,
    partial_answer: &str,
) -> String {
    let context = truncate_for_log(synthesis_context.trim(), 12000);
    format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
Continue an interrupted user-facing deep-research report.\n\
Strict rules:\n\
- Return ONLY the missing continuation; never repeat or rewrite text already emitted.\n\
- Preserve the language, structure, evidence standards, and decision depth of the existing report.\n\
- Complete any unfinished sentence or section, then cover material conclusions, tradeoffs, risks, and actions that are still missing.\n\
- Do not call tools and do not invent URLs or facts absent from the supplied context.\n\
- Do not mention interruption, timeout, recovery, token limits, or internal execution.\n\
{PM_ORCH_INTERNAL_END}\n\n\
User question:\n{}\n\n\
Remaining synthesis context:\n{}\n\n\
Already emitted report (do not repeat):\n{}\n\n\
Continue exactly where the report stopped.",
        user_message.trim(),
        context,
        partial_answer.trim_end()
    )
}

pub(super) fn build_pm_preserved_partial_turn(
    session_id: &str,
    model: &str,
    text: String,
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
) -> TurnResult {
    TurnResult {
        session_id: session_id.to_string(),
        text,
        thinking: None,
        tool_calls: observed_tool_calls.to_vec(),
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
    }
}

#[derive(Debug, Clone, Default)]
struct PmForceSynthMapPacket {
    key: String,
    title: String,
    goal: String,
    deliverable: String,
    probe_count: usize,
    routes: Vec<String>,
    variants: Vec<String>,
    citations: Vec<String>,
    domains: Vec<String>,
    evidence_excerpt: String,
}

fn pm_force_synth_subtask_title(outcome: &PmProbeOutcome) -> String {
    let fallback = outcome
        .subtask_title
        .as_deref()
        .or(outcome.subtask_id.as_deref())
        .or(outcome.subtask_key.as_deref())
        .unwrap_or("unscoped")
        .trim();
    if fallback.is_empty() {
        "unscoped".to_string()
    } else {
        fallback.to_string()
    }
}

fn pm_push_unique_limited(target: &mut Vec<String>, raw: &str, cap: usize) {
    if cap == 0 {
        return;
    }
    let value = raw.trim();
    if value.is_empty() {
        return;
    }
    if target.iter().any(|item| item == value) {
        return;
    }
    if target.len() >= cap {
        return;
    }
    target.push(value.to_string());
}

fn pm_append_excerpt_limited(target: &mut String, addition: &str, cap_chars: usize) {
    if cap_chars == 0 {
        return;
    }
    let addition = addition.trim();
    if addition.is_empty() {
        return;
    }
    let existing_chars = target.chars().count();
    if existing_chars >= cap_chars {
        return;
    }
    let remaining = cap_chars - existing_chars;
    if !target.is_empty() {
        target.push_str("\n---\n");
    }
    let clipped: String = addition.chars().take(remaining).collect();
    target.push_str(&clipped);
}

fn build_pm_force_synth_map_packets(
    probe_outcomes: &[PmProbeOutcome],
    excerpt_cap_chars: usize,
) -> Vec<PmForceSynthMapPacket> {
    let mut packets = Vec::<PmForceSynthMapPacket>::new();
    let mut packet_idx = HashMap::<String, usize>::new();
    for outcome in probe_outcomes {
        let title = pm_force_synth_subtask_title(outcome);
        let key = {
            let normalized = normalized_pm_key(&title);
            if normalized.is_empty() {
                "unscoped".to_string()
            } else {
                normalized
            }
        };
        let index = if let Some(existing) = packet_idx.get(&key).copied() {
            existing
        } else {
            let idx = packets.len();
            packets.push(PmForceSynthMapPacket {
                key: key.clone(),
                title: title.clone(),
                goal: outcome.subtask_goal.clone().unwrap_or_default(),
                deliverable: outcome.subtask_deliverable.clone().unwrap_or_default(),
                ..PmForceSynthMapPacket::default()
            });
            packet_idx.insert(key.clone(), idx);
            idx
        };
        let packet = &mut packets[index];
        packet.probe_count = packet.probe_count.saturating_add(1);

        if packet.goal.trim().is_empty() {
            packet.goal = outcome.subtask_goal.clone().unwrap_or_default();
        }
        if packet.deliverable.trim().is_empty() {
            packet.deliverable = outcome.subtask_deliverable.clone().unwrap_or_default();
        }

        let route = outcome
            .route_id
            .as_deref()
            .or(outcome.route_channel.as_deref())
            .unwrap_or("");
        pm_push_unique_limited(&mut packet.routes, route, 4);
        pm_push_unique_limited(&mut packet.variants, &outcome.variant, 4);

        if let Some(quality) = &outcome.quality {
            for citation in quality.citations.iter().take(12) {
                pm_push_unique_limited(&mut packet.citations, citation, 24);
            }
            for domain in quality.domains.iter().take(10) {
                pm_push_unique_limited(&mut packet.domains, domain, 12);
            }
        }

        if let Some(turn) = &outcome.turn {
            let visible = extract_pm_visible_answer_text(&turn.text);
            let clipped = truncate_for_log(visible.trim(), 1400);
            pm_append_excerpt_limited(&mut packet.evidence_excerpt, &clipped, excerpt_cap_chars);
        }
        if outcome.turn.is_none() {
            if let Some(turn) = &outcome.diagnostic_turn {
                let visible = extract_pm_visible_answer_text(&turn.text);
                let clipped = truncate_for_log(visible.trim(), 1200);
                if !clipped.trim().is_empty() {
                    pm_append_excerpt_limited(
                        &mut packet.evidence_excerpt,
                        &format!(
                            "Non-citable research notes. These are weak hints only and must not be cited as external sources:\n{}",
                            clipped.trim()
                        ),
                        excerpt_cap_chars,
                    );
                }
            }
        }
    }

    packets.sort_by(|a, b| {
        b.citations
            .len()
            .cmp(&a.citations.len())
            .then_with(|| b.domains.len().cmp(&a.domains.len()))
            .then_with(|| b.probe_count.cmp(&a.probe_count))
            .then_with(|| a.title.cmp(&b.title))
    });
    packets
}

fn ensure_pm_force_synth_planned_packets(
    packets: &mut Vec<PmForceSynthMapPacket>,
    planned_probe_outcomes: &[PmProbeOutcome],
) {
    let mut known = packets
        .iter()
        .map(|packet| packet.key.clone())
        .collect::<HashSet<_>>();
    for outcome in planned_probe_outcomes {
        let has_planned_scope = outcome
            .subtask_id
            .as_deref()
            .or(outcome.subtask_key.as_deref())
            .or(outcome.subtask_title.as_deref())
            .is_some_and(|value| !value.trim().is_empty());
        if !has_planned_scope {
            continue;
        }
        let title = pm_force_synth_subtask_title(outcome);
        let normalized = normalized_pm_key(&title);
        let key = if normalized.is_empty() {
            "unscoped".to_string()
        } else {
            normalized
        };
        if !known.insert(key.clone()) {
            continue;
        }
        packets.push(PmForceSynthMapPacket {
            key,
            title,
            goal: outcome.subtask_goal.clone().unwrap_or_default(),
            deliverable: outcome.subtask_deliverable.clone().unwrap_or_default(),
            probe_count: 1,
            variants: vec![outcome.variant.clone()],
            ..PmForceSynthMapPacket::default()
        });
    }
}

fn build_pm_non_citable_research_notes(
    probe_outcomes: &[PmProbeOutcome],
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
    cap_chars: usize,
) -> String {
    if cap_chars == 0 {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for outcome in probe_outcomes {
        let Some(turn) = outcome.diagnostic_turn.as_ref() else {
            continue;
        };
        let visible = extract_pm_visible_answer_text(&turn.text);
        let visible = visible.trim();
        if visible.is_empty() {
            continue;
        }
        let subtask = outcome
            .subtask_title
            .as_deref()
            .or(outcome.subtask_id.as_deref())
            .or(outcome.subtask_key.as_deref())
            .unwrap_or("unscoped");
        lines.push(format!(
            "### Non-citable notes for {subtask}\nVariant: {}\n{}\n",
            truncate_for_log(&outcome.variant, 180),
            truncate_for_log(visible, 1400)
        ));
        if lines.join("\n").chars().count() >= cap_chars {
            break;
        }
    }

    if !observed_tool_calls.is_empty() && lines.join("\n").chars().count() < cap_chars {
        let mut snippets = Vec::<String>::new();
        for tc in observed_tool_calls.iter().filter(|tc| !tc.is_error).take(6) {
            let excerpt = first_non_empty_line(&tc.output);
            let excerpt = if excerpt.is_empty() {
                truncate_for_log(&tc.output, 260)
            } else {
                truncate_for_log(&excerpt, 260)
            };
            if !excerpt.trim().is_empty() {
                snippets.push(format!(
                    "- {}:{} {}",
                    tc.source,
                    tc.source_name,
                    excerpt.trim()
                ));
            }
        }
        if !snippets.is_empty() {
            lines.push(format!(
                "### Non-citable observed tool snippets\n{}",
                snippets.join("\n")
            ));
        }
    }

    let joined = lines.join("\n\n");
    if joined.chars().count() > cap_chars {
        joined.chars().take(cap_chars).collect()
    } else {
        joined
    }
}

#[cfg(test)]
mod report_strategy_tests {
    use super::*;

    fn make_quality() -> PmAnswerQualityDto {
        PmAnswerQualityDto {
            passed: false,
            deliverable: false,
            quality_level: "low".to_string(),
            has_tool_calls: true,
            tool_call_count: 1,
            citation_count: 1,
            domain_count: 1,
            claim_count: 3,
            claim_alignment_ok: false,
            triad_total_claims: 3,
            triad_aligned_claims: 0,
            triad_coverage: 0.0,
            conflict_adjudicated: false,
            conflict_confidence: 0.35,
            conflict_reason: String::new(),
            citations: vec!["https://example.com/rewarded-ads".to_string()],
            domains: vec!["example.com".to_string()],
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

    fn report_plan() -> serde_json::Value {
        serde_json::json!({
            "mode": "business_report_strategy",
            "reportStrategy": {
                "firstPartyEvidenceJson": {
                    "evidencePriority": "primary",
                    "opportunityCohorts": [
                        {"cohort": "eCPM 5+ + AIPU 1~4", "why": "高价值低活跃"},
                        {"cohort": "eCPM <1 + AIPU >=16", "why": "低 eCPM 但高活跃"},
                        {"cohort": "new 低 AIPU", "why": "新用户低 AIPU 偏亏"}
                    ],
                    "failedExperiments": [
                        {"name": "EWMA", "lesson": "单纯少发金币不是健康增长策略"}
                    ],
                    "antiPatterns": ["不能只靠少发金币降成本"]
                }
            },
            "taskGraph": {
                "decompositionMode": "light",
                "subtasks": [
                    {"title": "玩法补强"},
                    {"title": "保护指标"}
                ]
            }
        })
    }

    const QUESTION: &str = "印尼网赚单机休闲产品，ROI1.235，AIPU17.11，eCPM3.16，ROAS1/3/7 要提升，按 eCPM 和 AIPU 分层，之前 EWMA 伤了时长和次留。";

    #[test]
    fn report_strategy_gate_accepts_segmented_experiment_answer() {
        let mut quality = make_quality();
        let answer = "优先做三件事：1. eCPM 5+ + AIPU 1~4 做高价值低活跃拉频，规则是每日前两次连击广告给阶梯奖励；2. eCPM <1 + AIPU 0/1~4 做成本止损，不加提现激励；3. eCPM <1 + AIPU >=16 保护时长，尝试换高价广告位。实验灰度 10%，保护指标为 AIPU、游戏时长、次留、ROAS1/3/7，kill criteria 是任一保护指标连续两天下降或 ROI 不升反降。";
        let result =
            apply_pm_report_strategy_quality_gate(&mut quality, &report_plan(), QUESTION, answer);
        assert!(result.enforced);
        assert!(result.passed, "{result:?} {:?}", quality.missing);
        assert!(quality.passed);
        assert!(quality.deliverable);
    }

    #[test]
    fn report_strategy_gate_accepts_non_ad_industry_cohorts() {
        let mut quality = make_quality();
        let plan = serde_json::json!({
            "mode": "business_report_strategy",
            "reportStrategy": {
                "firstPartyEvidenceJson": {
                    "evidencePriority": "primary",
                    "opportunityCohorts": [
                        {"cohort": "solo trial activation 18%", "why": "low activation"},
                        {"cohort": "team trial activation 44%", "why": "higher activation"}
                    ],
                    "failedExperiments": [
                        {"name": "mandatory demo wall", "lesson": "activation 下降"}
                    ],
                    "antiPatterns": ["不要强制预约 demo"]
                }
            }
        });
        let question = "B2B SaaS onboarding，activation 31%，MRR $120k，churn 7.2%，按 solo/team trial 分层，之前 mandatory demo wall 让 activation 下降。";
        let answer = "结论：先按 solo trial 和 team trial 两个人群拆策略。solo trial activation 18% 先做 in-app checklist 和 template gallery 引导，team trial activation 44% 做协作邀请后的 workspace setup 完成率优化。实验规则：5% holdout + A/B，对照现状。保护指标：support tickets、churn、MRR 和 activation 不得差于对照。Kill criteria：任一保护指标连续两个观察窗口恶化就回滚。不要强制预约 demo，避免重复 mandatory demo wall 的失败。";
        let result = apply_pm_report_strategy_quality_gate(&mut quality, &plan, question, answer);
        assert!(result.enforced);
        assert!(result.passed, "{result:?} {:?}", quality.missing);
        assert!(result.has_opportunity_cohorts);
        assert!(result.respects_anti_patterns);
    }

    #[test]
    fn report_strategy_gate_accepts_dynamic_metrics_for_any_industry() {
        let mut quality = make_quality();
        let plan = serde_json::json!({
            "mode": "business_report_strategy",
            "reportStrategy": {
                "firstPartyEvidenceJson": {
                    "evidencePriority": "primary",
                    "metrics": [
                        {"name": "OEE", "value": "71%"},
                        {"name": "scrap_rate", "value": "4.8%"},
                        {"name": "lead_time", "value": "9.2d"}
                    ],
                    "opportunityCohorts": [
                        {"cohort": "line A night shift", "why": "scrap_rate 高"},
                        {"cohort": "supplier X batches", "why": "lead_time 波动"}
                    ],
                    "failedExperiments": [
                        {"name": "blanket overtime", "lesson": "OEE 没提升且返工上升"}
                    ],
                    "antiPatterns": ["不要统一加班"]
                }
            }
        });
        let question = "制造工厂产运报告：OEE71%，scrap_rate4.8%，lead_time9.2d；line A night shift 和 supplier X batches 是关键场景，之前 blanket overtime 失败。";
        let answer = "结论：按 line A night shift 和 supplier X batches 分场景做策略。line A night shift 先做换班前质量点检和首件确认，supplier X batches 做到货批次预警和替代供应阈值。实验规则：10% 产线/批次 holdout，对照现状。保护指标：OEE、scrap_rate、lead_time、返工率都不得差于对照。Kill criteria：scrap_rate 或 lead_time 连续两个观察窗口恶化即回滚。不要统一加班，避免重复 blanket overtime 的失败。";
        let result = apply_pm_report_strategy_quality_gate(&mut quality, &plan, question, answer);
        assert!(result.enforced);
        assert!(result.passed, "{result:?} {:?}", quality.missing);
        assert!(result.matched_metric_count >= 3, "{result:?}");
        assert!(quality.passed);
    }

    #[test]
    fn report_strategy_gate_rejects_tool_diagnostic_pollution() {
        let mut quality = make_quality();
        let answer = "关键结论：\"durationMs\": 1391（来源：https://developers.google.com/admob）。建议补齐用户分层。";
        let result =
            apply_pm_report_strategy_quality_gate(&mut quality, &report_plan(), QUESTION, answer);
        assert!(result.enforced);
        assert!(!result.passed);
        assert!(quality
            .missing
            .iter()
            .any(|item| item.contains("tool_diagnostic_leaked_into_answer")));
    }

    #[test]
    fn report_strategy_gate_rejects_generic_answer_missing_extracted_cohorts() {
        let mut quality = make_quality();
        let answer = "建议做分层运营、提升广告体验，并通过 A/B 实验灰度上线。保护指标看 AIPU、时长、次留、ROAS，kill criteria 是指标下降就停止。";
        let result =
            apply_pm_report_strategy_quality_gate(&mut quality, &report_plan(), QUESTION, answer);
        assert!(result.enforced);
        assert!(!result.passed);
        assert!(!result.has_opportunity_cohorts);
        assert!(quality
            .missing
            .iter()
            .any(|item| item.contains("missing_extracted_opportunity_cohorts")));
    }

    #[test]
    fn report_strategy_gate_requires_multiple_extracted_cohort_hits() {
        let mut quality = make_quality();
        let answer = "优先做三件事：1. eCPM 5+ + AIPU 1~4 拉频，不能简单加金币；2. 泛化优化广告体验；3. 泛化做新手引导。实验灰度 10%，保护指标为 AIPU、游戏时长、次留、ROAS1/3/7，kill criteria 是任一保护指标连续两天下降。";
        let result =
            apply_pm_report_strategy_quality_gate(&mut quality, &report_plan(), QUESTION, answer);
        assert!(result.enforced);
        assert!(!result.passed);
        assert!(!result.has_opportunity_cohorts);
    }

    #[test]
    fn report_strategy_gate_rejects_answer_that_ignores_failed_experiment_lessons() {
        let mut quality = make_quality();
        let answer = "优先做三件事：1. eCPM 5+ + AIPU 1~4 拉频；2. new 低 AIPU 做广告教育；3. eCPM <1 + AIPU >=16 提升广告位价值。实验灰度 10%，保护指标为 AIPU、游戏时长、次留、ROAS1/3/7，kill criteria 是任一保护指标连续两天下降。";
        let result =
            apply_pm_report_strategy_quality_gate(&mut quality, &report_plan(), QUESTION, answer);
        assert!(result.enforced);
        assert!(!result.passed);
        assert!(!result.respects_anti_patterns);
        assert!(quality
            .missing
            .iter()
            .any(|item| item.contains("missing_failed_experiment_or_anti_pattern_lessons")));
    }

    #[test]
    fn depth_gate_does_not_block_report_strategy_mode() {
        let mut quality = make_quality();
        let result =
            apply_pm_depth_coverage_gate(&mut quality, &report_plan(), "短答案", &[], 3, 3, 2);
        assert!(!result.enforced);
        assert!(result.coverage_gate.passed);
        assert!(quality.missing.is_empty());
    }
}

fn render_pm_force_synth_map_packet_context(
    packet: &PmForceSynthMapPacket,
    excerpt_cap_chars: usize,
) -> String {
    let routes = if packet.routes.is_empty() {
        "-".to_string()
    } else {
        packet.routes.join(", ")
    };
    let variants = if packet.variants.is_empty() {
        "-".to_string()
    } else {
        packet.variants.join(" | ")
    };
    let citations = if packet.citations.is_empty() {
        "-".to_string()
    } else {
        packet.citations.join("\n")
    };
    let domains = if packet.domains.is_empty() {
        "-".to_string()
    } else {
        packet.domains.join(", ")
    };
    let excerpt = if packet.evidence_excerpt.trim().is_empty() {
        "(no excerpt)".to_string()
    } else {
        packet
            .evidence_excerpt
            .chars()
            .take(excerpt_cap_chars)
            .collect::<String>()
    };
    format!(
        "SubtaskKey: {}\nSubtaskTitle: {}\nSubtaskGoal: {}\nSubtaskDeliverable: {}\nProbeCount: {}\nRoutes: {}\nVariants: {}\nDomains: {}\nCitations:\n{}\nEvidenceExcerpt:\n{}",
        packet.key,
        packet.title,
        if packet.goal.trim().is_empty() {
            "-"
        } else {
            packet.goal.trim()
        },
        if packet.deliverable.trim().is_empty() {
            "-"
        } else {
            packet.deliverable.trim()
        },
        packet.probe_count,
        routes,
        variants,
        domains,
        citations,
        excerpt
    )
}

fn render_pm_force_synth_local_packet_summary(
    packet: &PmForceSynthMapPacket,
    summary_cap_chars: usize,
) -> String {
    let url_sample = if packet.citations.is_empty() {
        "-".to_string()
    } else {
        packet
            .citations
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let excerpt: String = packet
        .evidence_excerpt
        .chars()
        .take(summary_cap_chars)
        .collect();
    format!(
        "Subtask [{}]\nProbes={} Domains={} URLs={}\nLocalSummarySnippet:\n{}",
        packet.title,
        packet.probe_count,
        packet.domains.len(),
        url_sample,
        if excerpt.trim().is_empty() {
            "(no snippet)"
        } else {
            excerpt.trim()
        }
    )
}

async fn build_pm_force_synthesize_map_reduce_context(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    attempt: usize,
    admitted_probe_outcomes: &[PmProbeOutcome],
    planned_probe_outcomes: &[PmProbeOutcome],
    probe_context: &str,
    observed_context: &str,
    baseline_context: &str,
) -> (String, bool, serde_json::Value) {
    let enabled = pm_flag_enabled("PM_FORCE_SYNTH_MAP_REDUCE_ENABLED", true);
    let llm_map_enabled = pm_flag_enabled("PM_FORCE_SYNTH_MAP_LLM_ENABLED", false);
    let min_subtasks = pm_env_usize("PM_FORCE_SYNTH_MAP_MIN_SUBTASKS", 2).clamp(2, 8);
    let max_subtasks = pm_env_usize("PM_FORCE_SYNTH_MAP_MAX_SUBTASKS", 6).clamp(2, 16);
    let packet_excerpt_chars =
        pm_env_usize("PM_FORCE_SYNTH_PACKET_EXCERPT_CHARS", 2600).clamp(1200, 12000);
    let map_context_chars =
        pm_env_usize("PM_FORCE_SYNTH_MAP_CONTEXT_CHARS", 3200).clamp(1400, 14000);
    let map_summary_chars = pm_env_usize("PM_FORCE_SYNTH_MAP_SUMMARY_CHARS", 1600).clamp(800, 6000);
    let map_timeout_secs = pm_env_u64("PM_FORCE_SYNTH_MAP_TIMEOUT_SECS", 60).clamp(10, 120);
    let reduce_context_chars =
        pm_env_usize("PM_FORCE_SYNTH_REDUCE_CONTEXT_CHARS", 22000).clamp(6000, 60000);

    let mut packets =
        build_pm_force_synth_map_packets(admitted_probe_outcomes, packet_excerpt_chars);
    // Evidence admission decides what may be cited, but it must not erase a
    // planned research dimension merely because its retrieval slot failed.
    ensure_pm_force_synth_planned_packets(&mut packets, planned_probe_outcomes);
    packets.sort_by(|a, b| {
        b.citations
            .len()
            .cmp(&a.citations.len())
            .then_with(|| b.domains.len().cmp(&a.domains.len()))
            .then_with(|| b.probe_count.cmp(&a.probe_count))
            .then_with(|| a.title.cmp(&b.title))
    });
    let total_subtasks = packets.len();
    if !enabled || total_subtasks < min_subtasks {
        return (
            baseline_context.to_string(),
            false,
            serde_json::json!({
                "mapReduceUsed": false,
                "enabled": enabled,
                "llmMapEnabled": llm_map_enabled,
                "reason": if !enabled { "disabled" } else { "insufficient_subtasks" },
                "totalSubtasks": total_subtasks,
                "minSubtasks": min_subtasks,
            }),
        );
    }

    let keep_count = total_subtasks.min(max_subtasks);
    let overflow_titles = packets
        .iter()
        .skip(keep_count)
        .map(|packet| packet.title.clone())
        .collect::<Vec<_>>();
    packets.truncate(keep_count);

    let mut map_sections = Vec::<String>::new();
    let mut map_prompt_chars = 0usize;
    let mut map_summary_chars_total = 0usize;
    let mut map_input_tokens_total = 0u64;
    let mut map_output_tokens_total = 0u64;
    let mut map_success = 0usize;
    let mut map_fallback = 0usize;
    let mut map_local = 0usize;
    let mut map_scratch_session_guard: Option<PmTransientSessionGuard> = None;
    let map_runtime_session_id = if llm_map_enabled {
        match manager.get_session(session_id).await {
            Some(handle) => {
                let model_hint = (!handle.model.trim().is_empty()).then_some(handle.model.as_str());
                match manager
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
                    Ok(session) => {
                        let id = session.session_id.clone();
                        map_scratch_session_guard =
                            Some(PmTransientSessionGuard::new(manager.clone(), id.clone()));
                        Some(id)
                    }
                    Err(error) => {
                        tracing::warn!(
                            session_id = %session_id,
                            "force synth map scratch session creation failed; using local packet summaries: {}",
                            error
                        );
                        None
                    }
                }
            }
            None => None,
        }
    } else {
        None
    };

    for (idx, packet) in packets.iter().enumerate() {
        let subtask_context = render_pm_force_synth_map_packet_context(packet, map_context_chars);
        map_prompt_chars = map_prompt_chars.saturating_add(subtask_context.chars().count());
        let map_summary = if let Some(runtime_session_id) = map_runtime_session_id.as_deref() {
            let map_prompt = wrap_pm_research_prompt(
                session_source,
                build_pm_subtask_map_prompt(
                    user_message,
                    &packet.title,
                    &subtask_context,
                    attempt,
                    idx + 1,
                    keep_count,
                ),
            );
            let map_turn = run_pm_turn_with_timeout_cleanup_and_options(
                manager.clone(),
                runtime_session_id.to_string(),
                map_prompt,
                map_timeout_secs,
                "forced synthesize subtask map turn",
                pm_force_synthesize_no_tool_options(map_timeout_secs),
            )
            .await;
            match map_turn {
                Ok(turn) => {
                    map_success = map_success.saturating_add(1);
                    map_input_tokens_total =
                        map_input_tokens_total.saturating_add(u64::from(turn.usage.input_tokens));
                    map_output_tokens_total =
                        map_output_tokens_total.saturating_add(u64::from(turn.usage.output_tokens));
                    let visible = extract_pm_visible_answer_text(&turn.text);
                    let clipped = truncate_for_log(visible.trim(), map_summary_chars);
                    if clipped.trim().is_empty() {
                        map_fallback = map_fallback.saturating_add(1);
                        render_pm_force_synth_local_packet_summary(packet, map_summary_chars)
                    } else {
                        clipped
                    }
                }
                Err(_) => {
                    map_fallback = map_fallback.saturating_add(1);
                    render_pm_force_synth_local_packet_summary(packet, map_summary_chars)
                }
            }
        } else {
            if llm_map_enabled {
                map_fallback = map_fallback.saturating_add(1);
            } else {
                map_local = map_local.saturating_add(1);
            }
            render_pm_force_synth_local_packet_summary(packet, map_summary_chars)
        };
        map_summary_chars_total =
            map_summary_chars_total.saturating_add(map_summary.chars().count());
        map_sections.push(format!(
            "### Subtask Map [{}]\n{}",
            packet.title, map_summary
        ));
    }

    if !overflow_titles.is_empty() {
        map_sections.push(format!(
            "### Additional Covered Subtasks (not dropped)\n{}",
            overflow_titles.join(", ")
        ));
    }
    if !probe_context.trim().is_empty() {
        map_sections.push(format!(
            "### Probe Coverage Snapshot\n{}",
            truncate_for_log(probe_context.trim(), 2600)
        ));
    }
    if !observed_context.trim().is_empty() {
        map_sections.push(format!(
            "### Observed Tool Snapshot\n{}",
            truncate_for_log(observed_context.trim(), 2000)
        ));
    }

    let mut reduce_context = format!(
        "Subtask MAP summaries generated for global REDUCE.\nTotalSubtasks={} MappedSubtasks={} OverflowSubtasks={}\n{}\n\nBaselineContext:\n{}",
        total_subtasks,
        keep_count,
        overflow_titles.len(),
        map_sections.join("\n\n"),
        baseline_context.trim()
    );
    if reduce_context.chars().count() > reduce_context_chars {
        reduce_context = reduce_context.chars().take(reduce_context_chars).collect();
    }
    if let Some(guard) = map_scratch_session_guard {
        guard.finish().await;
    }
    let diag = serde_json::json!({
        "mapReduceUsed": true,
        "llmMapEnabled": llm_map_enabled,
        "totalSubtasks": total_subtasks,
        "mappedSubtasks": keep_count,
        "overflowSubtasks": overflow_titles.len(),
        "mapPromptChars": map_prompt_chars,
        "mapSummaryChars": map_summary_chars_total,
        "reduceContextChars": reduce_context.chars().count(),
        "mapLlmSuccess": map_success,
        "mapLlmFallback": map_fallback,
        "mapLocalSummary": map_local,
        "mapInputTokens": map_input_tokens_total,
        "mapOutputTokens": map_output_tokens_total,
    });
    (reduce_context, true, diag)
}

pub(super) async fn run_pm_force_synthesize_fallback_turn_with_observed_tools(
    manager: Arc<AgentSessionManager>,
    session_id: &str,
    session_source: &str,
    user_message: &str,
    probe_outcomes: &[PmProbeOutcome],
    attempt: usize,
    observed_tool_calls: &[agent_gateway::ToolCallRecord],
    answer_delta: Option<PmAnswerDeltaCallback>,
) -> Result<TurnResult, GatewayError> {
    let admission = admit_pm_external_evidence(user_message, probe_outcomes, observed_tool_calls);
    let synthesis_probe_outcomes = admission.accepted_probe_outcomes.as_slice();
    let synthesis_tool_calls = admission.accepted_tool_calls.as_slice();
    let probe_context = build_pm_probe_repair_context(synthesis_probe_outcomes);
    let observed_context = build_pm_observed_tool_context(synthesis_tool_calls);
    let non_citable_notes =
        build_pm_non_citable_research_notes(probe_outcomes, observed_tool_calls, 9000);
    let baseline_context_base = if probe_context.is_empty() && observed_context.is_empty() {
        if admission.examined_evidence_count > 0 {
            "External retrieval returned no admitted source-backed evidence. Discard rejected snippets and produce a decision-grade answer from the user's first-party data plus expert reasoning. Clearly mark assumptions, confidence, and validation needs; do not cite rejected sources.".to_string()
        } else {
            "No successful retrieval evidence was captured in prior attempts. Produce a best-effort answer grounded in the user's first-party data and product/operations reasoning; clearly state evidence gaps.".to_string()
        }
    } else if observed_context.is_empty() {
        probe_context.clone()
    } else if probe_context.is_empty() {
        observed_context.clone()
    } else {
        format!("{probe_context}\n\n{observed_context}")
    };
    let baseline_context = if non_citable_notes.trim().is_empty() {
        baseline_context_base
    } else {
        format!(
            "{}\n\nNon-citable research notes:\n{}\n\nUse the notes only as weak hints for ideation or counter-checking. Do not cite them, do not present them as verified external evidence, and prioritize the user's first-party data when they conflict.",
            baseline_context_base,
            non_citable_notes.trim()
        )
    };
    let (previous_answer, use_reduce_prompt, map_reduce_diag) =
        build_pm_force_synthesize_map_reduce_context(
            manager.clone(),
            session_id,
            session_source,
            user_message,
            attempt,
            synthesis_probe_outcomes,
            probe_outcomes,
            &probe_context,
            &observed_context,
            &baseline_context,
        )
        .await;
    let mut force_synth_diag = map_reduce_diag.clone();
    if let Some(obj) = force_synth_diag.as_object_mut() {
        obj.insert("evidenceAdmission".to_string(), admission.to_json());
    }
    pm_set_force_synth_diag(session_id, force_synth_diag.clone());
    tracing::info!(
        session_id = %session_id,
        attempt = attempt,
        use_reduce_prompt = use_reduce_prompt,
        map_reduce_diag = %force_synth_diag,
        "prepared force synth context using map-reduce planning"
    );
    let prompt = wrap_pm_research_prompt(
        session_source,
        if use_reduce_prompt {
            build_pm_force_synthesize_reduce_prompt(user_message, &previous_answer, attempt)
        } else {
            build_pm_force_synthesize_prompt(user_message, &previous_answer, attempt)
        },
    );
    let total_synthesis_budget_secs = pm_synthesis_total_budget_secs();
    let recovery_reserve_secs = pm_synthesis_recovery_reserve_secs(total_synthesis_budget_secs);
    let primary_timeout_secs = total_synthesis_budget_secs
        .saturating_sub(recovery_reserve_secs)
        .max(60);
    let synthesis_started = Instant::now();
    let Some(handle) = manager.get_session(session_id).await else {
        tracing::warn!(
            session_id = %session_id,
            attempt = attempt,
            "forced synthesize caller session is unavailable; returning local first-party synthesis"
        );
        return Ok(build_pm_local_strategy_synthesis_turn(
            session_id,
            "pm-local-first-party-fallback",
            user_message,
            "caller session unavailable before model synthesis",
            attempt,
            synthesis_tool_calls,
        ));
    };
    let model_hint = if handle.model.trim().is_empty() {
        None
    } else {
        Some(handle.model.as_str())
    };
    let synth_options = pm_force_synthesize_no_tool_options(primary_timeout_secs);
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
        Ok(transient_session) => transient_session,
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                attempt = attempt,
                "forced synthesize scratch create_session failed; returning local first-party synthesis: {}",
                error
            );
            return Ok(build_pm_local_strategy_synthesis_turn(
                session_id,
                model_hint.unwrap_or("pm-local-first-party-fallback"),
                user_message,
                &format!("scratch synthesis session creation failed: {error}"),
                attempt,
                synthesis_tool_calls,
            ));
        }
    };
    let transient_session_id = transient_session.session_id.clone();
    let transient_session_guard =
        PmTransientSessionGuard::new(manager.clone(), transient_session_id.clone());
    let primary_answer_delta = answer_delta.clone();
    let (transient_result, primary_partial) =
        run_pm_user_visible_answer_streaming_turn_preserving_partial(
            manager.clone(),
            transient_session_id.clone(),
            prompt,
            primary_timeout_secs,
            "forced synthesize scratch turn",
            synth_options,
            move |delta| {
                if let Some(answer_delta) = primary_answer_delta.as_ref() {
                    answer_delta("synthesize", delta);
                }
            },
        )
        .await;
    transient_session_guard.finish().await;
    let primary_error_text = match &transient_result {
        Ok(turn) if !turn.text.trim().is_empty() => None,
        Ok(_) => Some("scratch synthesis returned empty text".to_string()),
        Err(error) => Some(error.to_string()),
    };
    if let Ok(mut turn) = transient_result {
        if !turn.text.trim().is_empty() {
            turn.session_id = session_id.to_string();
            return Ok(merge_pm_turn_with_observed_tool_calls(
                turn,
                synthesis_tool_calls,
            ));
        }
    }
    let primary_error_text = primary_error_text
        .unwrap_or_else(|| "scratch synthesis returned unusable output".to_string());

    // Never discard user-visible text. If the primary reduce turn reached its
    // budget, continue only the unfinished suffix within the same total
    // synthesis budget. A failed continuation still returns all captured text.
    if !primary_partial.trim().is_empty() {
        let elapsed_secs = synthesis_started.elapsed().as_secs();
        let remaining_secs = total_synthesis_budget_secs.saturating_sub(elapsed_secs);
        tracing::warn!(
            session_id = %session_id,
            attempt,
            partial_chars = primary_partial.chars().count(),
            elapsed_secs,
            remaining_secs,
            error = %primary_error_text,
            "forced synthesis interrupted after visible output; preserving partial report and attempting suffix continuation"
        );
        if remaining_secs >= 30 {
            let continuation_prompt = wrap_pm_research_prompt(
                session_source,
                build_pm_synthesis_continuation_prompt(
                    user_message,
                    &previous_answer,
                    &primary_partial,
                ),
            );
            match manager
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
                Ok(continuation_session) => {
                    let continuation_session_id = continuation_session.session_id.clone();
                    let continuation_guard = PmTransientSessionGuard::new(
                        manager.clone(),
                        continuation_session_id.clone(),
                    );
                    let continuation_answer_delta = answer_delta.clone();
                    let (continuation_result, continuation_partial) =
                        run_pm_user_visible_answer_streaming_turn_preserving_partial(
                            manager.clone(),
                            continuation_session_id,
                            continuation_prompt,
                            remaining_secs,
                            "forced synthesize continuation turn",
                            pm_force_synthesize_no_tool_options(remaining_secs),
                            move |delta| {
                                if let Some(answer_delta) = continuation_answer_delta.as_ref() {
                                    answer_delta("synthesize_continuation", delta);
                                }
                            },
                        )
                        .await;
                    continuation_guard.finish().await;
                    let continuation_text = match &continuation_result {
                        Ok(turn) if !turn.text.trim().is_empty() => turn.text.clone(),
                        _ => continuation_partial,
                    };
                    let merged_text =
                        merge_pm_streamed_answer_parts(&primary_partial, &continuation_text);
                    if let Ok(mut turn) = continuation_result {
                        turn.session_id = session_id.to_string();
                        turn.text = merged_text;
                        return Ok(merge_pm_turn_with_observed_tool_calls(
                            turn,
                            synthesis_tool_calls,
                        ));
                    }
                    return Ok(build_pm_preserved_partial_turn(
                        session_id,
                        model_hint.unwrap_or("pm-preserved-partial"),
                        merged_text,
                        synthesis_tool_calls,
                    ));
                }
                Err(error) => tracing::warn!(
                    session_id = %session_id,
                    attempt,
                    error = %error,
                    "unable to create synthesis continuation session; delivering preserved partial report"
                ),
            }
        }
        return Ok(build_pm_preserved_partial_turn(
            session_id,
            model_hint.unwrap_or("pm-preserved-partial"),
            primary_partial,
            synthesis_tool_calls,
        ));
    }

    tracing::warn!(
        session_id = %session_id,
        attempt = attempt,
        "forced synthesize scratch turn failed; trying last-chance expert synthesis: {}",
        primary_error_text
    );
    let failure_summary = format!("scratch synthesis failed ({primary_error_text})");
    let expert_prompt = wrap_pm_research_prompt(
        session_source,
        build_pm_expert_only_final_prompt(
            user_message,
            &previous_answer,
            &failure_summary,
            attempt,
        ),
    );
    let remaining_synthesis_secs =
        total_synthesis_budget_secs.saturating_sub(synthesis_started.elapsed().as_secs());
    let expert_timeout_secs = remaining_synthesis_secs.min(pm_last_chance_synth_timeout_secs());
    if expert_timeout_secs < 30 {
        tracing::warn!(
            session_id = %session_id,
            attempt,
            total_synthesis_budget_secs,
            "no synthesis budget remains for last-chance model turn; returning local first-party synthesis"
        );
        return Ok(build_pm_local_strategy_synthesis_turn(
            session_id,
            model_hint.unwrap_or("pm-local-first-party-fallback"),
            user_message,
            &failure_summary,
            attempt,
            synthesis_tool_calls,
        ));
    }
    let expert_options = pm_force_synthesize_no_tool_options(expert_timeout_secs);
    let expert_session = manager
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
        .await;
    match expert_session {
        Ok(expert_session) => {
            let expert_session_id = expert_session.session_id.clone();
            let expert_session_guard =
                PmTransientSessionGuard::new(manager.clone(), expert_session_id.clone());
            let expert_answer_delta = answer_delta.clone();
            let (expert_result, expert_partial) =
                run_pm_user_visible_answer_streaming_turn_preserving_partial(
                    manager.clone(),
                    expert_session_id.clone(),
                    expert_prompt,
                    expert_timeout_secs,
                    "last-chance expert synthesis turn",
                    expert_options,
                    move |delta| {
                        if let Some(answer_delta) = expert_answer_delta.as_ref() {
                            answer_delta("last_chance_synthesize", delta);
                        }
                    },
                )
                .await;
            expert_session_guard.finish().await;
            match expert_result {
                Ok(mut turn) if !turn.text.trim().is_empty() => {
                    turn.session_id = session_id.to_string();
                    return Ok(merge_pm_turn_with_observed_tool_calls(
                        turn,
                        synthesis_tool_calls,
                    ));
                }
                Ok(_) => {
                    tracing::warn!(
                        attempt = attempt,
                        session_id = %session_id,
                        "last-chance expert synthesis returned empty text; falling back to local first-party synthesis"
                    );
                    if !expert_partial.trim().is_empty() {
                        return Ok(build_pm_preserved_partial_turn(
                            session_id,
                            model_hint.unwrap_or("pm-preserved-partial"),
                            expert_partial,
                            synthesis_tool_calls,
                        ));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        attempt = attempt,
                        session_id = %session_id,
                        "last-chance expert synthesis failed: {}",
                        error
                    );
                    if !expert_partial.trim().is_empty() {
                        return Ok(build_pm_preserved_partial_turn(
                            session_id,
                            model_hint.unwrap_or("pm-preserved-partial"),
                            expert_partial,
                            synthesis_tool_calls,
                        ));
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                attempt = attempt,
                session_id = %session_id,
                "last-chance expert synthesis create_session failed: {}",
                error
            );
        }
    }
    tracing::error!(
        attempt = attempt,
        session_id = %session_id,
        observed_tool_calls = synthesis_tool_calls.len(),
        primary_error = %primary_error_text,
        "PM LLM synthesis failed on scratch and last-chance expert sessions; returning local first-party synthesis"
    );
    Ok(build_pm_local_strategy_synthesis_turn(
        session_id,
        model_hint.unwrap_or("pm-local-first-party-fallback"),
        user_message,
        &failure_summary,
        attempt,
        synthesis_tool_calls,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_turn(text: &str) -> TurnResult {
        TurnResult {
            session_id: "s1".to_string(),
            text: text.to_string(),
            thinking: None,
            tool_calls: Vec::new(),
            usage: agent_gateway::TokenUsageRecord {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                total_tokens: 30,
                estimated_cost_usd: 0.0,
                model: "test".to_string(),
            },
            compacted: None,
            iterations: 1,
            metadata: None,
            hot_reloaded: false,
        }
    }

    fn mock_web_search_tool(
        query: &str,
        title: &str,
        url: &str,
        snippet: &str,
    ) -> agent_gateway::ToolCallRecord {
        let output = serde_json::json!({
            "query": query,
            "results": [{
                "tool_use_id": "test",
                "content": [{
                    "title": title,
                    "url": url,
                    "snippet": snippet,
                    "content": snippet,
                    "contentChars": snippet.chars().count(),
                    "sourceType": "web",
                    "sourceName": "test",
                    "relevanceScore": 0.82,
                    "confidence": 0.80
                }]
            }]
        });
        agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "native_model_search".to_string(),
            input: serde_json::json!({"query": query}).to_string(),
            output: serde_json::to_string(&output).unwrap(),
            is_error: false,
            duration_ms: 20,
        }
    }

    fn mock_outcome(subtask_title: &str, variant: &str, text: &str) -> PmProbeOutcome {
        PmProbeOutcome {
            variant: variant.to_string(),
            route_id: Some("web.search.general".to_string()),
            route_channel: Some("web_search".to_string()),
            subtask_key: Some(normalized_pm_key(subtask_title)),
            subtask_id: Some(normalized_pm_key(subtask_title)),
            subtask_title: Some(subtask_title.to_string()),
            subtask_goal: Some(format!("goal-{subtask_title}")),
            subtask_deliverable: Some(format!("deliverable-{subtask_title}")),
            subtask_required_evidence_type: None,
            subtask_priority: Some("high".to_string()),
            elapsed_ms: Some(120),
            turn: Some(mock_turn(text)),
            diagnostic_turn: None,
            quality: None,
            error: None,
        }
    }

    fn mock_outcome_with_tool(
        subtask_title: &str,
        variant: &str,
        tool_call: agent_gateway::ToolCallRecord,
    ) -> PmProbeOutcome {
        let mut turn = mock_turn("source-backed probe summary");
        turn.tool_calls = vec![tool_call];
        let quality = evaluate_pm_answer_quality(&turn);
        let mut outcome = mock_outcome(subtask_title, variant, "source-backed probe summary");
        outcome.turn = Some(turn);
        outcome.quality = Some(quality);
        outcome
    }

    #[test]
    fn build_pm_force_synth_map_packets_groups_by_subtask_and_counts_probes() {
        let outcomes = vec![
            mock_outcome("UG 激励成本控制", "v1", "first evidence snippet A"),
            mock_outcome("UG 激励成本控制", "v2", "second evidence snippet B"),
            mock_outcome("AIPU 提升", "v3", "third evidence snippet C"),
        ];
        let packets = build_pm_force_synth_map_packets(&outcomes, 2400);
        assert_eq!(packets.len(), 2);
        let ug_packet = packets
            .iter()
            .find(|packet| packet.title == "UG 激励成本控制")
            .expect("UG packet should exist");
        assert_eq!(ug_packet.probe_count, 2);
        assert!(
            ug_packet
                .evidence_excerpt
                .contains("first evidence snippet A")
                || ug_packet
                    .evidence_excerpt
                    .contains("second evidence snippet B")
        );
        let aipu_packet = packets
            .iter()
            .find(|packet| packet.title == "AIPU 提升")
            .expect("AIPU packet should exist");
        assert_eq!(aipu_packet.probe_count, 1);
    }

    #[test]
    fn planned_subtask_without_retrieval_output_is_not_dropped() {
        let mut failed = mock_outcome("竞争格局", "competitor landscape", "");
        failed.turn = None;
        failed.diagnostic_turn = None;
        failed.quality = None;
        failed.error = Some("source slot timed out".to_string());

        let packets = build_pm_force_synth_map_packets(&[failed], 2400);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].title, "竞争格局");
        assert!(packets[0].evidence_excerpt.is_empty());
    }

    #[test]
    fn synthesis_continuation_merge_preserves_partial_without_duplication() {
        assert_eq!(
            merge_pm_streamed_answer_parts("第一部分\n结论是", "结论是可行的。\n第二部分"),
            "第一部分\n结论是可行的。\n第二部分"
        );
        assert_eq!(
            merge_pm_streamed_answer_parts("已完成的正文", "后续建议"),
            "已完成的正文\n\n后续建议"
        );
        assert_eq!(
            merge_pm_streamed_answer_parts("已完成的正文", "完整报告包含已完成的正文和结论"),
            "完整报告包含已完成的正文和结论"
        );
    }

    #[test]
    fn evidence_admission_rejects_unrelated_source_backed_noise() {
        let question = "基于 SaaS onboarding activation churn 做产品运营策略";
        let noisy = mock_outcome_with_tool(
            "activation strategy",
            "SaaS activation churn onboarding",
            mock_web_search_tool(
                "SaaS activation churn onboarding",
                "Random unrelated travel guide",
                "https://example.com/travel-guide",
                "This page is about hotels, flights, beaches, and restaurant bookings for summer holidays.",
            ),
        );
        let report = admit_pm_external_evidence(question, &[noisy], &[]);
        assert!(!report.external_evidence_usable, "{:?}", report);
        assert!(report.accepted_probe_outcomes.is_empty());
        assert!(report.rejected_evidence_count > 0);
        assert!(report
            .rejection_reasons
            .iter()
            .any(|reason| reason == "low_query_relevance"));
    }

    #[test]
    fn evidence_admission_accepts_relevant_source_backed_evidence() {
        let question = "基于 SaaS onboarding activation churn 做产品运营策略";
        let relevant = mock_outcome_with_tool(
            "activation strategy",
            "SaaS activation churn onboarding",
            mock_web_search_tool(
                "SaaS activation churn onboarding",
                "SaaS onboarding activation and churn benchmarks",
                "https://example.com/saas-onboarding-activation",
                "SaaS onboarding programs often use activation cohorts, checklist completion, churn guardrails, and controlled experiments to improve conversion.",
            ),
        );
        let report = admit_pm_external_evidence(question, &[relevant], &[]);
        assert!(report.external_evidence_usable, "{:?}", report);
        assert_eq!(report.accepted_probe_outcomes.len(), 1);
        assert!(report
            .accepted_urls
            .iter()
            .any(|url| url == "https://example.com/saas-onboarding-activation"));
    }

    #[test]
    fn evidence_admission_prefers_search_intent_over_long_user_context() {
        let question = format!(
            "{}\n\n{}",
            "一、背景 当前产品是印尼网赚单机休闲 App 矩阵，整体商业模式是买量进入、广告变现、激励提现。成本结构以买量为主。".repeat(80),
            "我需要针对新用户首日广告激活、低 AIPU 提升、高价值用户广告扩容提出策略。"
        );
        let evidence = mock_outcome_with_tool(
            "设计点对点爆破玩法组合",
            "rewarded ads ROI strategy frequency guardrails",
            mock_web_search_tool(
                "rewarded ads ROI strategy frequency guardrails",
                "Rewarded ads ROI strategy frequency guardrails",
                "https://example.com/rewarded-ads-roi-strategy",
                "Rewarded ads ROI strategy should use opt-in ad moments, frequency guardrails, churn monitoring, retention metrics, and cohort experiments.",
            ),
        );

        let report = admit_pm_external_evidence(&question, &[evidence], &[]);

        assert!(report.external_evidence_usable, "{report:?}");
        assert!(report
            .accepted_urls
            .iter()
            .any(|url| url == "https://example.com/rewarded-ads-roi-strategy"));
    }

    #[test]
    fn evidence_admission_accepts_trusted_unified_search_relevance_scores() {
        let question = "基于印尼网赚单机休闲矩阵的一手数据，提升 ROI/ROAS 且不伤留存。";
        let mut outcome = mock_outcome("设计点对点爆破玩法组合", "高价值用户广告扩容", "");
        let mut turn = mock_turn("source-backed probe summary");
        turn.tool_calls = vec![agent_gateway::ToolCallRecord {
            index: 0,
            tool_name: "WebSearch".to_string(),
            source: "builtin".to_string(),
            source_name: "native_model_search".to_string(),
            input: serde_json::json!({
                "query": "高价值用户广告扩容",
                "orchestrator": "unified_search"
            })
            .to_string(),
            output: serde_json::json!({
                "results": [{
                    "content": [{
                        "title": "Rewarded ads monetization guardrails",
                        "url": "https://example.com/rewarded-ads-monetization-guardrails",
                        "snippet": "Opt-in rewarded ad moments should be monitored with frequency caps, post-ad exit rate, retention, and cohort-level revenue quality.",
                        "content": "Opt-in rewarded ad moments should be monitored with frequency caps, post-ad exit rate, retention, and cohort-level revenue quality.",
                        "contentChars": 132,
                        "sourceType": "native_model_search",
                        "sourceName": "gpt-5.5",
                        "relevanceScore": 0.62,
                        "confidence": 0.80
                    }]
                }]
            })
            .to_string(),
            is_error: false,
            duration_ms: 20,
        }];
        outcome.quality = Some(evaluate_pm_answer_quality(&turn));
        outcome.turn = Some(turn);

        let report = admit_pm_external_evidence(question, &[outcome], &[]);

        assert!(report.external_evidence_usable, "{report:?}");
        assert!(report
            .accepted_urls
            .iter()
            .any(|url| url == "https://example.com/rewarded-ads-monetization-guardrails"));
    }

    #[test]
    fn evidence_admission_gate_clears_stale_missing_tool_markers() {
        let question = "基于 SaaS onboarding activation churn 做产品运营策略";
        let relevant = mock_outcome_with_tool(
            "activation strategy",
            "SaaS activation churn onboarding",
            mock_web_search_tool(
                "SaaS activation churn onboarding",
                "SaaS onboarding activation and churn benchmarks",
                "https://example.com/saas-onboarding-activation",
                "SaaS onboarding programs use activation cohorts, checklist completion, churn guardrails, and controlled experiments to improve conversion.",
            ),
        );
        let admission = admit_pm_external_evidence(question, &[relevant], &[]);
        let mut quality = evaluate_pm_answer_quality(&mock_turn(
            "Strategy answer without direct local tool records.",
        ));
        assert!(quality
            .missing
            .iter()
            .any(|item| item == "missing_tool_retrieval"));

        apply_pm_evidence_admission_gate(
            &mut quality,
            "Strategy answer without direct local tool records.",
            &admission,
        );

        assert!(quality.has_tool_calls);
        assert_eq!(quality.tool_call_count, 1);
        assert_eq!(quality.citation_count, 0);
        assert!(!quality
            .missing
            .iter()
            .any(|item| item == "missing_tool_retrieval"));
        assert!(!quality
            .missing
            .iter()
            .any(|item| item == "external_evidence_not_admitted"));
        assert!(quality
            .missing
            .iter()
            .any(|item| item == "thin_admitted_external_evidence"));
    }

    #[test]
    fn evidence_admission_gate_treats_probe_evidence_as_retrieval_for_final_quality() {
        let question = "基于 SaaS onboarding activation churn 做产品运营策略";
        let relevant = mock_outcome_with_tool(
            "activation strategy",
            "SaaS activation churn onboarding",
            mock_web_search_tool(
                "SaaS activation churn onboarding",
                "SaaS onboarding activation and churn benchmarks",
                "https://example.com/saas-onboarding-activation",
                "SaaS onboarding programs use activation cohorts, checklist completion, churn guardrails, and controlled experiments to improve conversion.",
            ),
        );
        let admission = admit_pm_external_evidence(question, &[relevant], &[]);
        let final_text = "- 最终答案由编辑器重写，正文没有本地 tool call，但前序 probe 已经有 source-backed evidence。\n- 策略建议应围绕 activation cohorts、churn guardrails 和 controlled experiments 展开。";
        let mut final_quality = evaluate_pm_answer_quality(&mock_turn(final_text));

        apply_pm_evidence_admission_gate(&mut final_quality, final_text, &admission);

        assert!(final_quality.has_tool_calls);
        assert_eq!(final_quality.tool_call_count, 1);
        assert_eq!(final_quality.citation_count, 0);
        assert_eq!(final_quality.domain_count, 0);
        assert!(final_quality.deliverable);
        assert_eq!(final_quality.quality_level, "partial");
        assert!(!final_quality
            .missing
            .iter()
            .any(|item| item == "missing_tool_retrieval"));
        assert!(final_quality
            .missing
            .iter()
            .any(|item| item == "missing_citations"));
        assert!(final_quality
            .missing
            .iter()
            .any(|item| item == "thin_admitted_external_evidence"));
    }

    #[test]
    fn evidence_admission_gate_does_not_make_empty_answer_deliverable() {
        let question = "基于 SaaS onboarding activation churn 做产品运营策略";
        let relevant = mock_outcome_with_tool(
            "activation strategy",
            "SaaS activation churn onboarding",
            mock_web_search_tool(
                "SaaS activation churn onboarding",
                "SaaS onboarding activation and churn benchmarks",
                "https://example.com/saas-onboarding-activation",
                "SaaS onboarding programs use activation cohorts, checklist completion, churn guardrails, and controlled experiments to improve conversion.",
            ),
        );
        let admission = admit_pm_external_evidence(question, &[relevant], &[]);
        let mut final_quality = evaluate_pm_answer_quality(&mock_turn(""));

        apply_pm_evidence_admission_gate(&mut final_quality, "", &admission);

        assert!(final_quality.has_tool_calls);
        assert!(!final_quality.deliverable);
        assert_eq!(final_quality.quality_level, "low");
    }

    #[test]
    fn long_research_answer_cannot_pass_with_only_three_visible_citations() {
        let urls = [
            "https://docs.example.com/capability",
            "https://research.example.org/comparison",
            "https://vendor.example.net/release",
        ];
        let mut turn = mock_turn(&format!(
            "{}\n\nSources: [official]({}) [comparison]({}) [release]({})",
            "A substantial externally verifiable research finding with several product capability comparisons and decision implications. ".repeat(80),
            urls[0],
            urls[1],
            urls[2],
        ));
        turn.tool_calls = urls
            .iter()
            .enumerate()
            .map(|(index, url)| {
                let mut call = mock_web_search_tool(
                    "product capability comparison",
                    "Product capability source",
                    url,
                    "Detailed product capability documentation with externally verifiable facts, constraints, release behavior, and comparison evidence.",
                );
                call.index = u32::try_from(index).expect("test source index fits in u32");
                call
            })
            .collect();

        let quality = evaluate_pm_answer_quality(&turn);

        assert!(turn.text.chars().count() > 8_000);
        assert_eq!(quality.citation_count, 3);
        assert!(!quality.passed);
        assert_eq!(quality.quality_level, "partial");
        assert!(quality
            .missing
            .iter()
            .any(|item| item == "insufficient_visible_citation_density"));
    }

    #[test]
    fn cross_domain_answer_without_detected_conflict_does_not_require_matrix() {
        let mut quality = build_pm_direct_answer_quality();
        quality.passed = true;
        quality.quality_level = "high".to_string();
        quality.claim_count = 4;
        quality.domain_count = 4;
        quality.conflict_confidence = 0.60;

        apply_pm_conflict_gate(&mut quality);

        assert!(quality.passed);
        assert_eq!(quality.quality_level, "high");
        assert!(!quality
            .missing
            .iter()
            .any(|item| item == "missing_conflict_matrix"));
    }

    #[test]
    fn conflict_gate_rejects_detected_but_unresolved_conflicts() {
        let mut quality = build_pm_direct_answer_quality();
        quality.passed = true;
        quality.quality_level = "high".to_string();
        quality.conflict_graph.edge_count = 2;
        quality.conflict_graph.unresolved_count = 1;

        apply_pm_conflict_gate(&mut quality);

        assert!(!quality.passed);
        assert_eq!(quality.quality_level, "partial");
        assert!(quality
            .missing
            .iter()
            .any(|item| item == "unresolved_source_conflicts"));
    }

    #[test]
    fn dimension_coverage_matches_subject_without_generic_planner_suffix() {
        let answer = "## OpenAI Codex\n长时任务恢复与取消能力。\n## Claude Code\n进度跟踪。\n## Kiro\n检查点。\n## AOS Watchdog\n差异化路径。";
        assert!(pm_contains_dimension_text(answer, "OpenAI Codex 能力调查"));
        assert!(pm_contains_dimension_text(answer, "Claude Code 能力分析"));
        assert!(pm_contains_dimension_text(answer, "Kiro 专题研究"));
        assert!(pm_contains_dimension_text(
            answer,
            "横向对比与 AOS Watchdog 差异化建议"
        ));
        assert!(!pm_contains_dimension_text(answer, "MongoDB 兼容性调查"));
    }

    #[test]
    fn render_pm_force_synth_map_packet_context_contains_urls_and_domains() {
        let packet = PmForceSynthMapPacket {
            key: "ug".to_string(),
            title: "UG 激励成本控制".to_string(),
            goal: "降低提现成本".to_string(),
            deliverable: "策略清单".to_string(),
            probe_count: 2,
            routes: vec!["web.search.general".to_string()],
            variants: vec!["cash reward app fraud".to_string()],
            citations: vec![
                "https://example.com/a".to_string(),
                "https://example.com/b".to_string(),
            ],
            domains: vec!["example.com".to_string()],
            evidence_excerpt: "snippet".to_string(),
        };
        let rendered = render_pm_force_synth_map_packet_context(&packet, 1200);
        assert!(rendered.contains("SubtaskTitle: UG 激励成本控制"));
        assert!(rendered.contains("https://example.com/a"));
        assert!(rendered.contains("Domains: example.com"));
    }

    #[test]
    fn local_strategy_synthesis_fallback_uses_first_party_context_not_failure_template() {
        let question = "We run a B2B SaaS self-serve onboarding product. In the last 30 days there were 18,420 trial users, activation is 31%, MRR is $120k, churn is 7.2%, and CAC is $86. The goal is to improve activation, reduce churn, and grow MRR, but support tickets must not increase. By segment: solo trial activation is 18%, team trial activation is 44%, enterprise trial activation is 27%. Existing mechanisms include email onboarding and an in-app checklist.";
        let turn = build_pm_local_strategy_synthesis_turn(
            "s1",
            "test-model",
            question,
            "forced synthesize turn timed out after 300s",
            3,
            &[],
        );
        assert!(turn.text.contains("Actionable Conclusion"));
        assert!(turn.text.contains("activation"));
        assert!(turn.text.contains("MRR"));
        assert!(turn.text.contains("churn"));
        assert!(turn.text.contains("support tickets"));
        assert!(turn.text.contains("email onboarding") || turn.text.contains("checklist"));
        assert!(!turn.text.contains("Unable To Produce"));
        assert!(!turn.text.contains("Please retry"));
        assert!(!turn.text.contains("rewarded"));
        assert!(!turn.text.contains("Indonesia"));
    }

    #[test]
    fn local_strategy_synthesis_fallback_chinese_keeps_useful_strategy_shape() {
        let question = "当前大盘 DAU 25,352，广告收入 $1,369/天，UA+UG 成本 $1,108/天，ROI 1.235，AIPU 17.11。目标是提升 ROI 和 ROAS，但 AIPU、游戏时长、次留不能下降。低 AIPU 1 到 4 人群 ROI 0.348，新用户 AIPU 1 到 4 ROI 0.144，高 AIPU 用户 ROI 更高。当前已有 eCPM 分层、连击玩法和悬浮宝箱。之前 EWMA 让 ROI 小幅上涨但 AIPU、时长、留存下降。";
        let turn = build_pm_local_strategy_synthesis_turn(
            "s1",
            "test-model",
            question,
            "primary synthesis failed; transient synthesis failed",
            4,
            &[],
        );
        assert!(turn.text.contains("## 先给可执行结论"));
        assert!(turn.text.contains("ROI"));
        assert!(turn.text.contains("ROAS"));
        assert!(turn.text.contains("AIPU"));
        assert!(turn.text.contains("不能下降"));
        assert!(turn.text.contains("对照组") || turn.text.contains("holdout"));
        assert!(!turn.text.contains("当前无法生成完整深度报告"));
        assert!(!turn.text.contains("请稍后重试"));
        assert!(!turn.text.contains("+2 more"));
        assert!(!turn.text.contains("Detected first-party evidence"));
    }

    fn review_ready_requirement_state() -> pm_domain::requirement_state::RequirementState {
        use pm_domain::requirement_state::{
            AcceptanceCriterion, JobToBeDone, Outcome, ProblemFrame, RequirementReadiness,
            Stakeholder,
        };
        let mut state = pm_domain::requirement_state::RequirementState {
            problem_frame: Some(ProblemFrame {
                statement: "improve onboarding".into(),
                confirmed: true,
            }),
            readiness: RequirementReadiness::ReadyForReview,
            ..pm_domain::requirement_state::RequirementState::default()
        };
        state.stakeholders.push(Stakeholder {
            name: "product owner".into(),
            role: Some("decision maker".into()),
            confirmed: true,
        });
        state.jobs.push(JobToBeDone {
            statement: "activate new users".into(),
            evidence_ids: vec![],
            confirmed: true,
        });
        state.desired_outcomes.push(Outcome {
            statement: "increase activation".into(),
            measure: Some("activation rate".into()),
        });
        state.scope.included.push("onboarding".into());
        state.acceptance_criteria.push(AcceptanceCriterion {
            id: "ac-1".into(),
            statement: "activation rate improves in holdout test".into(),
            testable: true,
        });
        state
    }

    #[test]
    fn requirement_state_delivery_gate_rejects_unverified_critical_assumptions() {
        use pm_domain::requirement_state::{Assumption, AssumptionStatus, AssumptionType};

        let ready = review_ready_requirement_state();
        let mut ready_turn = mock_turn("Review-ready plan");
        let mut ready_quality = build_pm_direct_answer_quality();
        enforce_requirement_state_delivery_gate(&mut ready_turn, &mut ready_quality, &ready);
        assert!(ready_quality.deliverable);
        assert_eq!(ready_turn.text, "Review-ready plan");

        let mut blocked = ready;
        blocked.readiness = pm_domain::requirement_state::RequirementReadiness::NeedsClarification;
        blocked.assumptions.push(Assumption {
            statement: "capacity is sufficient".into(),
            type_: AssumptionType::Technical,
            importance: 0.9,
            uncertainty: 0.8,
            status: AssumptionStatus::Open,
            supporting_evidence: vec![],
            counter_evidence: vec![],
            falsification_test: None,
        });
        let mut blocked_turn = mock_turn("Draft plan");
        let mut blocked_quality = build_pm_direct_answer_quality();
        enforce_requirement_state_delivery_gate(&mut blocked_turn, &mut blocked_quality, &blocked);
        assert!(!blocked_quality.deliverable);
        assert!(!blocked_quality.passed);
        assert_eq!(blocked_quality.quality_level, "needs_clarification");
        assert!(blocked_turn.text.contains("Requirement Brief"));
        assert!(blocked_turn
            .text
            .contains("validation plan for a critical assumption"));
    }

    #[tokio::test]
    async fn durable_requirement_state_controls_final_delivery_status() {
        let db = crate::test_sqlite_pool().await;
        let ready_plan = serde_json::json!({
            "taskGraph": {"subtasks": [{
                "goal": "improve onboarding",
                "deliverable": "measurable rollout plan"
            }]},
            "requirementDelta": {
                "problemFrame": {"statement": "improve onboarding", "confirmed": true},
                "stakeholders": [{"name": "product owner", "role": "decision maker", "confirmed": true}],
                "jobs": [{"statement": "activate new users", "evidenceIds": [], "confirmed": true}],
                "desiredOutcomes": [{"statement": "increase activation", "measure": "activation rate"}],
                "scope": {"included": ["onboarding"], "excluded": []},
                "acceptanceCriteria": [{
                    "id": "ac-1",
                    "statement": "activation improves in a holdout test",
                    "testable": true
                }],
                "readiness": "ready_for_review"
            }
        });
        crate::semantic_kernel_store::persist_pm_requirement_state_delta(
            &db,
            "tenant",
            "ready-session",
            "ready-event",
            "improve onboarding for product owner to activate new users",
            &ready_plan,
        )
        .await
        .unwrap();
        let (ready_turn, ready_quality) = finalize_pm_orchestration_result(
            &db,
            "tenant",
            "ready-run",
            "ready-session",
            mock_turn("Review-ready plan"),
            build_pm_direct_answer_quality(),
        )
        .await
        .unwrap();
        assert!(ready_quality.deliverable);
        assert!(!ready_turn.text.contains("Requirement Brief"));

        let mut blocked_plan = ready_plan;
        blocked_plan["requirementDelta"]["readiness"] = serde_json::json!("needs_clarification");
        blocked_plan["requirementDelta"]["assumptions"] = serde_json::json!([{
            "statement": "capacity is sufficient",
            "type": "technical",
            "importance": 0.9,
            "uncertainty": 0.8,
            "status": "open",
            "supportingEvidence": [],
            "counterEvidence": [],
            "falsificationTest": null
        }]);
        crate::semantic_kernel_store::persist_pm_requirement_state_delta(
            &db,
            "tenant",
            "blocked-session",
            "blocked-event",
            "improve onboarding for product owner to activate new users",
            &blocked_plan,
        )
        .await
        .unwrap();
        let (blocked_turn, blocked_quality) = finalize_pm_orchestration_result(
            &db,
            "tenant",
            "blocked-run",
            "blocked-session",
            mock_turn("Draft plan"),
            build_pm_direct_answer_quality(),
        )
        .await
        .unwrap();
        assert!(!blocked_quality.deliverable);
        assert!(!blocked_quality.passed);
        assert_eq!(blocked_quality.quality_level, "needs_clarification");
        assert!(blocked_turn.text.contains("Requirement Brief"));
        assert!(blocked_turn
            .text
            .contains("validation plan for a critical assumption"));
    }

    #[test]
    fn admitted_research_evidence_becomes_a_requirement_delta() {
        let mut quality = build_pm_direct_answer_quality();
        quality
            .evidence_tree
            .push(pm_report::PmEvidenceTreeNodeDto {
                claim: "Activation improved".into(),
                status: "confirmed".into(),
                evidence_count: 1,
                evidences: vec![pm_report::PmEvidenceLeafDto {
                    url: "https://example.com/evidence".into(),
                    domain: "example.com".into(),
                    excerpt: "A controlled experiment improved activation".into(),
                }],
            });
        let delta = pm_requirement_evidence_delta(&quality);
        assert_eq!(
            delta["requirementDelta"]["evidenceLinks"][0]["claim"],
            "Activation improved"
        );
        assert_eq!(
            delta["requirementDelta"]["evidenceLinks"][0]["support"],
            "supported"
        );
        assert_eq!(
            delta["requirementDelta"]["evidenceLinks"][0]["evidenceIds"][0],
            sha256_hex("https://example.com/evidence")
        );
    }
}
