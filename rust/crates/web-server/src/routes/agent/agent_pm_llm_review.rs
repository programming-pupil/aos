use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PmLlmReviewDecision {
    Finalize,
    ContinueResearch,
    Rewrite,
}

impl PmLlmReviewDecision {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Finalize => "finalize",
            Self::ContinueResearch => "continue_research",
            Self::Rewrite => "rewrite",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PmLlmExpertReview {
    #[serde(default)]
    pub evidence_quality_score: f64,
    #[serde(default)]
    pub source_relevance_score: f64,
    #[serde(default)]
    pub answer_depth_score: f64,
    #[serde(default)]
    pub actionability_score: f64,
    #[serde(default)]
    pub first_party_alignment_score: f64,
    #[serde(default)]
    pub decision_readiness_score: f64,
    pub decision: PmLlmReviewDecision,
    #[serde(default)]
    pub missing_evidence: Vec<String>,
    #[serde(default)]
    pub weak_claims: Vec<String>,
    #[serde(default)]
    pub next_queries: Vec<String>,
    #[serde(default)]
    pub rewrite_instructions: Vec<String>,
    #[serde(default)]
    pub source_quality_notes: Vec<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: String,
}

impl PmLlmExpertReview {
    fn clamp(mut self) -> Self {
        self.evidence_quality_score = self.evidence_quality_score.clamp(0.0, 1.0);
        self.source_relevance_score = self.source_relevance_score.clamp(0.0, 1.0);
        self.answer_depth_score = self.answer_depth_score.clamp(0.0, 1.0);
        self.actionability_score = self.actionability_score.clamp(0.0, 1.0);
        self.first_party_alignment_score = self.first_party_alignment_score.clamp(0.0, 1.0);
        self.decision_readiness_score = self.decision_readiness_score.clamp(0.0, 1.0);
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self.missing_evidence = sanitize_review_list(self.missing_evidence, 8, 180);
        self.weak_claims = sanitize_review_list(self.weak_claims, 8, 180);
        self.next_queries = sanitize_review_list(self.next_queries, 8, 180);
        self.rewrite_instructions = sanitize_review_list(self.rewrite_instructions, 8, 220);
        self.source_quality_notes = sanitize_review_list(self.source_quality_notes, 8, 220);
        self.reason = sanitize_review_text(&self.reason, 360);
        self
    }

    pub(super) fn to_json(&self) -> serde_json::Value {
        serde_json::json!(self)
    }

    pub(super) fn recommends_finalize(&self) -> bool {
        matches!(self.decision, PmLlmReviewDecision::Finalize)
            && self.decision_readiness_score >= 0.82
            && self.answer_depth_score >= 0.72
            && self.actionability_score >= 0.72
            && self.first_party_alignment_score >= 0.72
            && self.confidence >= 0.55
    }

    pub(super) fn recommends_rewrite(&self) -> bool {
        matches!(self.decision, PmLlmReviewDecision::Rewrite)
            && self.answer_depth_score >= 0.50
            && self.actionability_score >= 0.50
            && self.confidence >= 0.50
    }

    pub(super) fn recommends_targeted_research(&self) -> bool {
        matches!(self.decision, PmLlmReviewDecision::ContinueResearch)
            && !self.next_queries.is_empty()
            && (!self.missing_evidence.is_empty() || !self.weak_claims.is_empty())
            && self.confidence >= 0.55
    }
}

#[derive(Debug, Clone)]
pub(super) struct PmLlmReviewedQuality {
    pub(super) quality: PmAnswerQualityDto,
    pub(super) review: Option<PmLlmExpertReview>,
    pub(super) trace: serde_json::Value,
}

fn sanitize_review_text(raw: &str, max_chars: usize) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn sanitize_review_list(items: Vec<String>, max_items: usize, max_chars: usize) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for item in items {
        let sanitized = sanitize_review_text(&item, max_chars);
        if sanitized.is_empty() || out.iter().any(|existing| existing == &sanitized) {
            continue;
        }
        out.push(sanitized);
        if out.len() >= max_items {
            break;
        }
    }
    out
}

fn pm_llm_review_timeout_secs() -> u64 {
    pm_env_u64("PM_LLM_EXPERT_REVIEW_TIMEOUT_SECS", 90).clamp(20, 240)
}

fn pm_llm_final_editor_timeout_secs() -> u64 {
    pm_env_u64("PM_LLM_FINAL_EDITOR_TIMEOUT_SECS", 300).clamp(60, 600)
}

fn pm_llm_final_editor_max_attempts() -> usize {
    pm_env_usize("PM_LLM_FINAL_EDITOR_MAX_ATTEMPTS", 1).clamp(1, 3)
}

fn pm_llm_final_editor_uses_remaining_budget() -> bool {
    pm_flag_enabled("PM_LLM_FINAL_EDITOR_USE_PIPELINE_BUDGET", true)
}

fn pm_llm_final_editor_timeout_for_attempt(
    remaining_secs: u64,
    attempt: usize,
    max_attempts: usize,
) -> u64 {
    let configured = pm_llm_final_editor_timeout_secs();
    if !pm_llm_final_editor_uses_remaining_budget() {
        return configured;
    }
    let remaining_attempts = max_attempts.saturating_sub(attempt).saturating_add(1) as u64;
    let usable_remaining = remaining_secs.saturating_sub(15);
    if usable_remaining < 45 {
        return usable_remaining.max(1);
    }
    if usable_remaining >= configured {
        return configured;
    }
    let fair_share = usable_remaining / remaining_attempts.max(1);
    fair_share.clamp(45, configured)
}

fn pm_llm_model_only_options() -> agent_gateway::AgentTurnOptions {
    agent_gateway::AgentTurnOptions {
        disable_tools: true,
        disable_provider_thinking: true,
        reasoning_budget: agent_gateway::InternalReasoningBudget::Fast,
        prefer_native_web_search: false,
        suppress_native_web_search: true,
        ..Default::default()
    }
}

fn pm_take_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

fn pm_tail_chars(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    input
        .chars()
        .skip(count.saturating_sub(max_chars))
        .collect()
}

fn pm_final_editor_structural_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with('#')
        || trimmed.starts_with('|')
        || trimmed.starts_with("- [")
        || trimmed.starts_with("* [")
        || trimmed.starts_with('>')
        || trimmed.starts_with("一、")
        || trimmed.starts_with("二、")
        || trimmed.starts_with("三、")
        || trimmed.starts_with("四、")
        || trimmed.starts_with("五、")
        || trimmed.starts_with("六、")
        || trimmed.starts_with("七、")
        || trimmed.starts_with("八、")
        || trimmed.starts_with("九、")
        || trimmed.starts_with("十、")
        || trimmed.contains("http://")
        || trimmed.contains("https://")
        || lower.contains("source")
        || lower.contains("citation")
        || lower.contains("evidence")
        || trimmed.contains("来源")
        || trimmed.contains("引用")
        || trimmed.contains("证据")
}

fn pm_final_editor_critical_source_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    trimmed.contains("http://")
        || trimmed.contains("https://")
        || lower.contains("source")
        || lower.contains("citation")
        || lower.contains("evidence")
        || trimmed.contains("来源")
        || trimmed.contains("引用")
        || trimmed.contains("证据")
}

fn push_pm_editor_excerpt_line(
    out: &mut Vec<String>,
    used_chars: &mut usize,
    line: &str,
    max_chars: usize,
) {
    let line = sanitize_review_text(line, 260);
    if line.is_empty() || out.iter().any(|existing| existing == &line) {
        return;
    }
    let line_chars = line.chars().count();
    if used_chars.saturating_add(line_chars).saturating_add(1) > max_chars {
        return;
    }
    *used_chars = used_chars.saturating_add(line_chars).saturating_add(1);
    out.push(line);
}

fn build_pm_final_editor_answer_excerpt(answer: &str, max_chars: usize) -> String {
    let visible = extract_pm_visible_answer_text(answer);
    let visible_chars = visible.chars().count();
    if visible_chars <= max_chars {
        return visible;
    }

    let max_chars = max_chars.clamp(4000, 22000);
    let head_budget = (max_chars * 45 / 100).max(1600);
    let tail_budget = (max_chars * 25 / 100).max(1200);
    let structural_budget = max_chars.saturating_sub(head_budget + tail_budget + 600);
    let head = pm_take_chars(&visible, head_budget);
    let tail = pm_tail_chars(&visible, tail_budget);

    let mut structural = Vec::<String>::new();
    let mut structural_chars = 0usize;
    let critical_budget = (structural_budget / 2).max(600).min(structural_budget);
    for line in visible.lines() {
        if pm_final_editor_critical_source_line(line) {
            push_pm_editor_excerpt_line(
                &mut structural,
                &mut structural_chars,
                line,
                critical_budget,
            );
        }
    }
    for line in visible.lines() {
        if !pm_final_editor_structural_line(line) {
            continue;
        }
        push_pm_editor_excerpt_line(
            &mut structural,
            &mut structural_chars,
            line,
            structural_budget,
        );
    }

    let structural_text = if structural.is_empty() {
        "(no structural middle lines detected)".to_string()
    } else {
        structural.join("\n")
    };
    let excerpt = format!(
        "[Draft excerpt note: the current answer has {visible_chars} characters, exceeding the final-editor context budget. The excerpt preserves the opening, structural middle lines such as headings/tables/sources, and the ending.]\n\n\
Opening:\n{head}\n\n\
Structural middle lines:\n{structural_text}\n\n\
Ending:\n{tail}"
    );
    if excerpt.chars().count() > max_chars {
        let safe_head = pm_take_chars(&excerpt, max_chars.saturating_sub(700));
        format!(
            "{safe_head}\n\n[Excerpt clipped to fit final-editor context; preserve visible conclusions, numbers, URLs, and caveats from the available draft.]"
        )
    } else {
        excerpt
    }
}

fn pm_llm_final_editor_should_run(
    answer: &str,
    quality: &PmAnswerQualityDto,
    remaining_secs: u64,
) -> (bool, String) {
    let visible = extract_pm_visible_answer_text(answer);
    let char_count = visible.chars().count();
    if char_count > pm_env_usize("PM_LLM_FINAL_EDITOR_MAX_ANSWER_CHARS", 30000) {
        return (false, "answer_too_long_for_optional_editor".to_string());
    }
    let min_remaining = pm_env_u64("PM_LLM_FINAL_EDITOR_MIN_REMAINING_SECS", 75);
    if min_remaining > 0 && remaining_secs < min_remaining {
        return (false, "insufficient_remaining_time".to_string());
    }
    let has_markdown_or_noise_issue = quality
        .missing
        .iter()
        .chain(quality.suggestions.iter())
        .any(|item| {
            let lower = item.to_ascii_lowercase();
            lower.contains("markdown")
                || lower.contains("format")
                || lower.contains("formatting")
                || lower.contains("readability")
                || lower.contains("structure")
                || lower.contains("noise")
                || lower.contains("internal")
                || lower.contains("排版")
                || lower.contains("格式")
                || lower.contains("标题")
        });
    let looks_messy = visible.contains("+ more")
        || visible.contains("...")
        || visible.contains("……")
        || visible.contains("PM_LLM_EXPERT_REVIEW")
        || visible.contains(PM_ORCH_INTERNAL_BEGIN);
    let review_wants_rewrite = quality.missing.iter().any(|item| {
        item.contains("llm_expert_review_rewrite")
            || item.contains("deep_loop_requested_llm_rewrite")
    });
    if has_markdown_or_noise_issue || looks_messy || review_wants_rewrite {
        return (true, "format_or_review_signal".to_string());
    }
    if char_count < pm_env_usize("PM_LLM_FINAL_EDITOR_MIN_ANSWER_CHARS", 900) {
        return (false, "answer_short_enough".to_string());
    }
    (false, "optional_editor_not_needed".to_string())
}

fn pm_llm_review_answer_excerpt(answer: &str) -> String {
    extract_pm_visible_answer_text(answer)
        .chars()
        .take(pm_env_usize("PM_LLM_REVIEW_ANSWER_CHARS", 7000).clamp(2000, 12000))
        .collect()
}

fn pm_llm_review_plan_excerpt(plan: &serde_json::Value) -> String {
    let mut compact = serde_json::Map::new();
    for key in [
        "mode",
        "turnRoute",
        "taskGraph",
        "queryVariants",
        "reportStrategy",
        "selectedRouteIds",
    ] {
        if let Some(value) = plan.get(key) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    serde_json::to_string(&serde_json::Value::Object(compact))
        .unwrap_or_else(|_| "{}".to_string())
        .chars()
        .take(pm_env_usize("PM_LLM_REVIEW_PLAN_CHARS", 7000).clamp(1000, 16000))
        .collect()
}

fn pm_llm_review_tool_excerpt(tool_calls: &[agent_gateway::ToolCallRecord]) -> String {
    if tool_calls.is_empty() {
        return "[]".to_string();
    }
    let items = tool_calls
        .iter()
        .take(16)
        .map(|tc| {
            serde_json::json!({
                "tool": tc.tool_name,
                "source": tc.source,
                "sourceName": tc.source_name,
                "isError": tc.is_error,
                "input": tc.input.chars().take(500).collect::<String>(),
                "output": tc.output.chars().take(1400).collect::<String>(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

fn build_pm_llm_expert_review_prompt(
    question: &str,
    plan: &serde_json::Value,
    answer: &str,
    quality: &PmAnswerQualityDto,
    tool_calls: &[agent_gateway::ToolCallRecord],
    attempt: usize,
    max_attempts: usize,
) -> String {
    format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
You are an expert product/operations research reviewer for AOS PM Assistant.\n\
Your job is to judge whether the current answer is genuinely useful, deeply reasoned, source-grounded when needed, and faithful to first-party user data.\n\
Do not be a rigid checklist scorer. Judge semantic quality, source relevance, missing counterarguments, decision usefulness, and whether more research would materially improve the answer.\n\
Do not require external evidence for every private first-party metric; first-party user data is primary evidence. External sources are supporting context only.\n\
For claims about named products, APIs, models, policies, or platform capabilities, verify that official vendor documentation or release notes are used when available. Community issues, package registries, repositories, and third-party articles may support a finding but cannot alone prove an official capability.\n\
Do not recommend finalize when deterministic metrics report insufficient_visible_citation_density or insufficient_domain_diversity.\n\
Never block delivery: if evidence is imperfect, recommend continue_research or rewrite, but preserve usable conclusions.\n\
Return exactly one JSON object named PM_LLM_EXPERT_REVIEW with this shape:\n\
{{\n\
  \"evidenceQualityScore\": 0.0-1.0,\n\
  \"sourceRelevanceScore\": 0.0-1.0,\n\
  \"answerDepthScore\": 0.0-1.0,\n\
  \"actionabilityScore\": 0.0-1.0,\n\
  \"firstPartyAlignmentScore\": 0.0-1.0,\n\
  \"decisionReadinessScore\": 0.0-1.0,\n\
  \"decision\": \"finalize\" | \"continue_research\" | \"rewrite\",\n\
  \"missingEvidence\": [\"specific missing evidence or angle\"],\n\
  \"weakClaims\": [\"claim that is unsupported, generic, or wrongly sourced\"],\n\
  \"nextQueries\": [\"short query from user intent, not broad keywords\"],\n\
  \"rewriteInstructions\": [\"how to improve final answer without changing facts\"],\n\
  \"sourceQualityNotes\": [\"which source types/domains are strong or weak and why\"],\n\
  \"confidence\": 0.0-1.0,\n\
  \"reason\": \"brief rationale\"\n\
}}\n\
Decision policy:\n\
- finalize only if the answer is decision-usable and no further search is likely to change the conclusion.\n\
- continue_research if source relevance or coverage is weak and another targeted search angle is likely to help within budget.\n\
- rewrite if evidence is enough but answer structure/depth/readability is weak, or if internal/tool diagnostics leaked.\n\
{PM_ORCH_INTERNAL_END}\n\n\
User question:\n{question}\n\n\
Plan excerpt:\n{}\n\n\
Current quality metrics:\n{}\n\n\
Tool/evidence excerpt:\n{}\n\n\
Current answer excerpt:\n{}\n\n\
Attempt: {attempt}/{max_attempts}\n\
Return PM_LLM_EXPERT_REVIEW JSON only.",
        pm_llm_review_plan_excerpt(plan),
        serde_json::to_string(quality).unwrap_or_else(|_| "{}".to_string()),
        pm_llm_review_tool_excerpt(tool_calls),
        pm_llm_review_answer_excerpt(answer),
    )
}

fn parse_pm_llm_expert_review(text: &str) -> Option<PmLlmExpertReview> {
    let value = extract_named_json_object(text, "PM_LLM_EXPERT_REVIEW").or_else(|| {
        extract_first_json_object(text).and_then(|raw| parse_json_object_relaxed(&raw))
    })?;
    serde_json::from_value::<PmLlmExpertReview>(value)
        .ok()
        .map(PmLlmExpertReview::clamp)
}

fn push_quality_missing_once(quality: &mut PmAnswerQualityDto, key: String) {
    if !quality.missing.iter().any(|item| item == &key) {
        quality.missing.push(key);
    }
}

fn push_quality_suggestion_once(quality: &mut PmAnswerQualityDto, suggestion: String) {
    if !quality.suggestions.iter().any(|item| item == &suggestion) {
        quality.suggestions.push(suggestion);
    }
}

fn apply_pm_llm_expert_review_to_quality(
    mut quality: PmAnswerQualityDto,
    review: &PmLlmExpertReview,
    attempt: usize,
    max_attempts: usize,
) -> PmAnswerQualityDto {
    for item in &review.missing_evidence {
        push_quality_missing_once(&mut quality, format!("llm_missing_evidence:{item}"));
    }
    for item in &review.weak_claims {
        push_quality_missing_once(&mut quality, format!("llm_weak_claim:{item}"));
    }
    for item in review
        .rewrite_instructions
        .iter()
        .chain(review.source_quality_notes.iter())
        .chain(review.next_queries.iter())
        .take(12)
    {
        push_quality_suggestion_once(&mut quality, format!("LLM expert review: {item}"));
    }
    if !review.reason.is_empty() {
        quality.conflict_reason = format!(
            "{}; llm_expert_review={}",
            quality.conflict_reason, review.reason
        )
        .chars()
        .take(700)
        .collect();
    }

    let llm_ready = review.decision_readiness_score >= 0.82
        && review.answer_depth_score >= 0.72
        && review.actionability_score >= 0.72
        && review.first_party_alignment_score >= 0.72
        && review.confidence >= 0.55;
    let llm_warns = review.decision_readiness_score < 0.62
        || review.source_relevance_score < 0.45
        || review.answer_depth_score < 0.55;

    match review.decision {
        PmLlmReviewDecision::Finalize if llm_ready => {
            quality.deliverable = true;
            if quality.quality_level == "low" {
                quality.quality_level = "partial".to_string();
            }
            // Semantic review may preserve or downgrade deterministic quality,
            // but it must not waive missing visible citations, source domains,
            // or other hard evidence contracts.
            if quality.passed {
                quality.quality_level = "high".to_string();
            }
        }
        PmLlmReviewDecision::ContinueResearch if attempt < max_attempts => {
            quality.passed = false;
            push_quality_missing_once(
                &mut quality,
                "llm_expert_review_continue_research".to_string(),
            );
        }
        PmLlmReviewDecision::Rewrite => {
            quality.passed = false;
            quality.deliverable = true;
            if quality.quality_level == "low" {
                quality.quality_level = "partial".to_string();
            }
            push_quality_missing_once(&mut quality, "llm_expert_review_rewrite".to_string());
        }
        _ => {}
    }

    if llm_warns && attempt < max_attempts {
        quality.passed = false;
        if quality.quality_level == "high" {
            quality.quality_level = "partial".to_string();
        }
        push_quality_missing_once(&mut quality, "llm_expert_review_low_confidence".to_string());
    }
    quality
}

async fn run_pm_internal_llm_turn(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    prompt: String,
    timeout_secs: u64,
    timeout_label: &str,
    options: agent_gateway::AgentTurnOptions,
) -> Result<TurnResult, GatewayError> {
    let session = manager
        .create_session(
            user_id,
            tenant_id,
            None,
            Some(model),
            PM_INTERNAL_TRANSIENT_SESSION_SOURCE,
            Some("pm"),
            None,
            None,
        )
        .await?;
    let session_id = session.session_id.clone();
    let session_guard = PmTransientSessionGuard::new(manager.clone(), session_id.clone());
    let result = run_pm_turn_with_timeout_cleanup_and_options(
        manager.clone(),
        session_id.clone(),
        prompt,
        timeout_secs,
        timeout_label,
        options,
    )
    .await;
    session_guard.finish().await;
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pm_llm_expert_review(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    question: &str,
    plan: &serde_json::Value,
    turn: &TurnResult,
    quality: &PmAnswerQualityDto,
    attempt: usize,
    max_attempts: usize,
) -> PmLlmReviewedQuality {
    if !pm_flag_enabled("PM_LLM_EXPERT_REVIEW_ENABLED", true) {
        return PmLlmReviewedQuality {
            quality: quality.clone(),
            review: None,
            trace: serde_json::json!({
                "enabled": false,
                "reason": "PM_LLM_EXPERT_REVIEW_ENABLED=false"
            }),
        };
    }
    let started = Instant::now();
    let prompt = build_pm_llm_expert_review_prompt(
        question,
        plan,
        &turn.text,
        quality,
        &turn.tool_calls,
        attempt,
        max_attempts,
    );
    let timeout_secs = pm_llm_review_timeout_secs();
    let result = run_pm_internal_llm_turn(
        manager,
        tenant_id,
        user_id,
        model,
        wrap_pm_research_prompt("pm", prompt),
        timeout_secs,
        "pm llm expert review turn",
        pm_llm_model_only_options(),
    )
    .await;
    match result {
        Ok(review_turn) => {
            let parsed = parse_pm_llm_expert_review(&review_turn.text);
            if let Some(review) = parsed {
                let reviewed_quality = apply_pm_llm_expert_review_to_quality(
                    quality.clone(),
                    &review,
                    attempt,
                    max_attempts,
                );
                PmLlmReviewedQuality {
                    quality: reviewed_quality,
                    review: Some(review.clone()),
                    trace: serde_json::json!({
                        "enabled": true,
                        "status": "ok",
                        "durationMs": started.elapsed().as_millis(),
                        "timeoutSecs": timeout_secs,
                        "review": review.to_json(),
                        "usage": review_turn.usage,
                    }),
                }
            } else {
                PmLlmReviewedQuality {
                    quality: quality.clone(),
                    review: None,
                    trace: serde_json::json!({
                        "enabled": true,
                        "status": "parse_failed",
                        "durationMs": started.elapsed().as_millis(),
                        "timeoutSecs": timeout_secs,
                        "rawPreview": review_turn.text.chars().take(900).collect::<String>(),
                    }),
                }
            }
        }
        Err(error) => PmLlmReviewedQuality {
            quality: quality.clone(),
            review: None,
            trace: serde_json::json!({
                "enabled": true,
                "status": "unavailable",
                "durationMs": started.elapsed().as_millis(),
                "timeoutSecs": timeout_secs,
                "error": error.to_string(),
            }),
        },
    }
}

fn build_pm_llm_final_editor_prompt(
    question: &str,
    plan: &serde_json::Value,
    answer: &str,
    review: Option<&PmLlmExpertReview>,
    quality: &PmAnswerQualityDto,
    retry_note: Option<&str>,
) -> String {
    let answer_excerpt = build_pm_final_editor_answer_excerpt(
        answer,
        pm_env_usize("PM_LLM_FINAL_EDITOR_ANSWER_CHARS", 18000).clamp(4000, 22000),
    );
    let compact_plan = serde_json::json!({
        "mode": plan.get("mode").cloned().unwrap_or(serde_json::Value::Null),
        "turnRoute": plan.get("turnRoute").cloned().unwrap_or(serde_json::Value::Null),
        "reportStrategy": plan
            .get("reportStrategy")
            .and_then(|value| value.as_object())
            .map(|obj| {
                let mut out = serde_json::Map::new();
                for key in [
                    "primaryTerms",
                    "externalEvidenceRole",
                    "firstPartyEvidencePriority",
                ] {
                    if let Some(value) = obj.get(key) {
                        out.insert(key.to_string(), value.clone());
                    }
                }
                serde_json::Value::Object(out)
            })
            .unwrap_or(serde_json::Value::Null),
    });
    let retry_instruction = retry_note
        .filter(|note| !note.trim().is_empty())
        .map(|note| {
            format!(
                "\nRetry context:\n- Previous final-editor attempt did not produce an acceptable final answer: {}.\n- Try again from the original current answer below. Do not repeat the failed shape. Keep the answer complete, source-faithful, readable, and non-template.\n",
                sanitize_review_text(note, 600)
            )
        })
        .unwrap_or_default();
    format!(
        "{PM_ORCH_INTERNAL_BEGIN}\n\
You are the final editor for AOS PM Assistant.\n\
Rewrite the current answer into a polished, decision-grade article/report for a senior product or operations expert.\n\
Hard constraints:\n\
- Preserve conclusions, numbers, caveats, URLs, and confidence. Do not invent new facts or sources.\n\
- Improve readability only: hierarchy, natural headings, paragraph spacing, bullet/table formatting, and narrative flow.\n\
- Do not use one fixed template. Choose headings from the user's question and the answer's actual logic.\n\
- Remove internal diagnostics, JSON, tool logs, duplicate headings, '+N more', ellipses used as placeholders, and glued metric text.\n\
- If the original answer is Chinese, output all visible prose in Chinese. If English, use English.\n\
- Markdown must be clean: H2/H3 headings, blank lines between sections, tables only when cells are readable.\n\
- Keep hierarchy consistent: do not mix plain numbered items like \"1.\" with markdown headings like \"### 2.\" inside the same logical list.\n\
- Keep enough depth; do not shorten into a generic summary.\n\
Return only the final visible answer, no JSON, no commentary.\n\
{}\
{PM_ORCH_INTERNAL_END}\n\n\
User question:\n{question}\n\n\
Plan excerpt:\n{}\n\n\
Quality metrics:\n{}\n\n\
LLM expert review:\n{}\n\n\
Current answer:\n{}\n\n\
Return the polished final answer.",
        retry_instruction,
        serde_json::to_string(&compact_plan).unwrap_or_else(|_| "{}".to_string()),
        serde_json::to_string(quality).unwrap_or_else(|_| "{}".to_string()),
        review
            .map(PmLlmExpertReview::to_json)
            .unwrap_or(serde_json::Value::Null),
        answer_excerpt,
    )
}

fn pm_editor_output_is_usable(original: &str, edited: &str) -> bool {
    let edited_visible = extract_pm_visible_answer_text(edited);
    let edited_trimmed = edited_visible.trim();
    if edited_trimmed.chars().count() < 240 {
        return false;
    }
    let original_visible = extract_pm_visible_answer_text(original);
    if edited_trimmed.chars().count() < original_visible.trim().chars().count() / 3 {
        return false;
    }
    !is_pm_visible_output_noise(edited_trimmed)
        && !edited_trimmed.contains("PM_LLM_EXPERT_REVIEW")
        && !edited_trimmed.contains(PM_ORCH_INTERNAL_BEGIN)
}

fn pm_merge_editor_usage(target: &mut TurnResult, usage: &TokenUsageRecord) {
    target.usage.input_tokens = target.usage.input_tokens.saturating_add(usage.input_tokens);
    target.usage.output_tokens = target
        .usage
        .output_tokens
        .saturating_add(usage.output_tokens);
    target.usage.cache_creation_tokens = target
        .usage
        .cache_creation_tokens
        .saturating_add(usage.cache_creation_tokens);
    target.usage.cache_read_tokens = target
        .usage
        .cache_read_tokens
        .saturating_add(usage.cache_read_tokens);
    target.usage.total_tokens = target.usage.total_tokens.saturating_add(usage.total_tokens);
    target.usage.estimated_cost_usd += usage.estimated_cost_usd;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pm_llm_final_editor_if_needed(
    manager: Arc<AgentSessionManager>,
    tenant_id: &str,
    user_id: &str,
    model: &str,
    question: &str,
    plan: &serde_json::Value,
    turn: TurnResult,
    quality: &PmAnswerQualityDto,
    review: Option<&PmLlmExpertReview>,
    remaining_secs: u64,
) -> (TurnResult, serde_json::Value) {
    if !pm_flag_enabled("PM_LLM_FINAL_EDITOR_ENABLED", true) {
        return (
            turn,
            serde_json::json!({
                "enabled": false,
                "reason": "PM_LLM_FINAL_EDITOR_ENABLED=false"
            }),
        );
    }
    let (should_run, should_run_reason) =
        pm_llm_final_editor_should_run(&turn.text, quality, remaining_secs);
    if !should_run {
        return (
            turn,
            serde_json::json!({
                "enabled": true,
                "status": "skipped",
                "reason": should_run_reason,
                "remainingSecs": remaining_secs,
            }),
        );
    }
    let started = Instant::now();
    let max_attempts = pm_llm_final_editor_max_attempts();
    let mut attempts = Vec::<serde_json::Value>::new();
    let mut rejected_usage = Vec::<TokenUsageRecord>::new();
    let mut retry_note: Option<String> = None;

    for attempt in 1..=max_attempts {
        let elapsed_secs = started.elapsed().as_secs();
        let remaining_for_attempts = remaining_secs.saturating_sub(elapsed_secs);
        if pm_llm_final_editor_uses_remaining_budget() && remaining_for_attempts < 75 {
            attempts.push(serde_json::json!({
                "attempt": attempt,
                "status": "skipped_budget_exhausted",
                "remainingSecs": remaining_for_attempts,
            }));
            break;
        }
        let timeout_secs =
            pm_llm_final_editor_timeout_for_attempt(remaining_for_attempts, attempt, max_attempts);
        let prompt = build_pm_llm_final_editor_prompt(
            question,
            plan,
            &turn.text,
            review,
            quality,
            retry_note.as_deref(),
        );
        let result = run_pm_internal_llm_turn(
            manager.clone(),
            tenant_id,
            user_id,
            model,
            wrap_pm_research_prompt("pm", prompt),
            timeout_secs,
            "pm llm final editor turn",
            pm_llm_model_only_options(),
        )
        .await;

        match result {
            Ok(editor_turn) if pm_editor_output_is_usable(&turn.text, &editor_turn.text) => {
                let mut edited_turn = turn;
                for usage in rejected_usage.iter() {
                    pm_merge_editor_usage(&mut edited_turn, usage);
                }
                pm_merge_editor_usage(&mut edited_turn, &editor_turn.usage);
                edited_turn.text = extract_pm_visible_answer_text(&editor_turn.text);
                attempts.push(serde_json::json!({
                    "attempt": attempt,
                    "status": "ok",
                    "timeoutSecs": timeout_secs,
                    "usage": editor_turn.usage,
                    "outputChars": editor_turn.text.chars().count(),
                }));
                return (
                    edited_turn,
                    serde_json::json!({
                        "enabled": true,
                        "status": if attempt == 1 { "ok" } else { "ok_after_retry" },
                        "durationMs": started.elapsed().as_millis(),
                        "timeoutSecs": timeout_secs,
                        "attempt": attempt,
                        "maxAttempts": max_attempts,
                        "attempts": attempts,
                        "remainingSecsAtStart": remaining_secs,
                    }),
                );
            }
            Ok(editor_turn) => {
                let raw_preview = editor_turn.text.chars().take(700).collect::<String>();
                retry_note = Some(format!(
                    "attempt {attempt} output rejected by usability gate; preview: {raw_preview}"
                ));
                attempts.push(serde_json::json!({
                    "attempt": attempt,
                    "status": "rejected",
                    "timeoutSecs": timeout_secs,
                    "usage": editor_turn.usage,
                    "rawPreview": raw_preview,
                }));
                rejected_usage.push(editor_turn.usage);
            }
            Err(error) => {
                let error_text = error.to_string();
                retry_note = Some(format!("attempt {attempt} failed: {error_text}"));
                attempts.push(serde_json::json!({
                    "attempt": attempt,
                    "status": "unavailable",
                    "timeoutSecs": timeout_secs,
                    "error": error_text,
                }));
            }
        }
    }

    let mut fallback_turn = turn;
    for usage in rejected_usage.iter() {
        pm_merge_editor_usage(&mut fallback_turn, usage);
    }
    (
        fallback_turn,
        serde_json::json!({
            "enabled": true,
            "status": "failed_after_retries",
            "durationMs": started.elapsed().as_millis(),
            "configuredTimeoutSecs": pm_llm_final_editor_timeout_secs(),
            "maxAttempts": max_attempts,
            "attempts": attempts,
            "remainingSecsAtStart": remaining_secs,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static PM_LLM_REVIEW_ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarRestore {
        values: Vec<(String, Option<String>)>,
    }

    impl EnvVarRestore {
        fn capture(keys: &[&str]) -> Self {
            Self {
                values: keys
                    .iter()
                    .map(|key| ((*key).to_string(), std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            for (key, value) in &self.values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn with_pm_llm_review_env_vars<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _lock = PM_LLM_REVIEW_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let keys = vars.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let _restore = EnvVarRestore::capture(&keys);
        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        f()
    }

    #[test]
    fn parses_named_llm_expert_review_json() {
        let raw = r#"PM_LLM_EXPERT_REVIEW {"evidenceQualityScore":0.7,"sourceRelevanceScore":0.8,"answerDepthScore":0.9,"actionabilityScore":0.85,"firstPartyAlignmentScore":0.9,"decisionReadinessScore":0.86,"decision":"finalize","missingEvidence":[],"weakClaims":[],"nextQueries":["pricing benchmark"],"rewriteInstructions":["improve headings"],"sourceQualityNotes":["sources relevant"],"confidence":0.8,"reason":"good enough"}"#;
        let parsed = parse_pm_llm_expert_review(raw).expect("review");
        assert!(matches!(parsed.decision, PmLlmReviewDecision::Finalize));
        assert_eq!(parsed.next_queries, vec!["pricing benchmark"]);
        assert!((parsed.decision_readiness_score - 0.86).abs() < 0.001);
    }

    #[test]
    fn review_can_downgrade_for_more_research_without_blocking_delivery() {
        let mut quality = build_pm_direct_answer_quality();
        quality.passed = true;
        quality.deliverable = true;
        let review = PmLlmExpertReview {
            evidence_quality_score: 0.35,
            source_relevance_score: 0.30,
            answer_depth_score: 0.50,
            actionability_score: 0.60,
            first_party_alignment_score: 0.80,
            decision_readiness_score: 0.45,
            decision: PmLlmReviewDecision::ContinueResearch,
            missing_evidence: vec!["credible source-backed competitor examples".to_string()],
            weak_claims: Vec::new(),
            next_queries: vec!["competitor case study pricing".to_string()],
            rewrite_instructions: Vec::new(),
            source_quality_notes: Vec::new(),
            confidence: 0.75,
            reason: "sources are off-topic".to_string(),
        }
        .clamp();
        let updated = apply_pm_llm_expert_review_to_quality(quality, &review, 1, 3);
        assert!(!updated.passed);
        assert!(updated.deliverable);
        assert!(updated
            .missing
            .iter()
            .any(|item| item == "llm_expert_review_continue_research"));
    }

    #[test]
    fn semantic_finalize_cannot_waive_deterministic_evidence_gate() {
        let mut quality = build_pm_direct_answer_quality();
        quality.passed = false;
        quality.deliverable = true;
        quality.quality_level = "partial".to_string();
        quality
            .missing
            .push("insufficient_visible_citation_density".to_string());
        let review = PmLlmExpertReview {
            evidence_quality_score: 0.95,
            source_relevance_score: 0.95,
            answer_depth_score: 0.95,
            actionability_score: 0.95,
            first_party_alignment_score: 0.95,
            decision_readiness_score: 0.95,
            decision: PmLlmReviewDecision::Finalize,
            missing_evidence: Vec::new(),
            weak_claims: Vec::new(),
            next_queries: Vec::new(),
            rewrite_instructions: Vec::new(),
            source_quality_notes: Vec::new(),
            confidence: 0.95,
            reason: "semantically strong".to_string(),
        };

        let updated = apply_pm_llm_expert_review_to_quality(quality, &review, 1, 3);

        assert!(!updated.passed);
        assert_eq!(updated.quality_level, "partial");
        assert!(updated
            .missing
            .iter()
            .any(|item| item == "insufficient_visible_citation_density"));
    }

    #[test]
    fn final_editor_defaults_to_remaining_pipeline_budget() {
        with_pm_llm_review_env_vars(
            &[
                ("PM_LLM_FINAL_EDITOR_USE_PIPELINE_BUDGET", None),
                ("PM_LLM_FINAL_EDITOR_TIMEOUT_SECS", Some("300")),
            ],
            || {
                assert_eq!(pm_llm_final_editor_timeout_for_attempt(30, 1, 3), 15);
                assert_eq!(pm_llm_final_editor_timeout_for_attempt(120, 2, 3), 52);
            },
        );
    }

    #[test]
    fn final_editor_can_opt_into_pipeline_budget_splitting() {
        with_pm_llm_review_env_vars(
            &[
                ("PM_LLM_FINAL_EDITOR_USE_PIPELINE_BUDGET", Some("true")),
                ("PM_LLM_FINAL_EDITOR_TIMEOUT_SECS", Some("300")),
            ],
            || {
                assert_eq!(pm_llm_final_editor_timeout_for_attempt(180, 1, 3), 55);
                assert_eq!(pm_llm_final_editor_timeout_for_attempt(600, 1, 2), 300);
            },
        );
    }

    #[test]
    fn review_and_final_editor_are_fast_model_only_turns() {
        let options = pm_llm_model_only_options();
        assert!(options.disable_tools);
        assert!(options.disable_provider_thinking);
        assert_eq!(
            options.reasoning_budget,
            agent_gateway::InternalReasoningBudget::Fast
        );
        assert!(options.suppress_native_web_search);
        assert!(!options.prefer_native_web_search);
    }

    #[test]
    fn final_editor_runs_for_short_but_messy_answers() {
        with_pm_llm_review_env_vars(
            &[("PM_LLM_FINAL_EDITOR_MIN_ANSWER_CHARS", Some("900"))],
            || {
                let mut quality = build_pm_direct_answer_quality();
                quality
                    .suggestions
                    .push("排版包含内部截断噪声，需要清理".to_string());
                let (should_run, reason) =
                    pm_llm_final_editor_should_run("结论可用，但还有 +2 more。", &quality, 300);
                assert!(should_run);
                assert_eq!(reason, "format_or_review_signal");
            },
        );
    }

    #[test]
    fn final_editor_excerpt_preserves_tail_and_sources_for_long_answers() {
        let long_middle = "中段分析内容。".repeat(2000);
        let answer = format!(
            "## 开头结论\n先给出核心结论。\n\n{long_middle}\n\n## 关键证据来源\n- https://example.com/source-a\n\n## 最终收敛\n这是最后的执行建议。"
        );
        let excerpt = build_pm_final_editor_answer_excerpt(&answer, 5000);
        assert!(excerpt.contains("Draft excerpt note"));
        assert!(excerpt.contains("## 开头结论"));
        assert!(excerpt.contains("https://example.com/source-a"));
        assert!(excerpt.contains("## 最终收敛"));
        assert!(excerpt.contains("这是最后的执行建议"));
        assert!(excerpt.chars().count() <= 5000);
    }
}
