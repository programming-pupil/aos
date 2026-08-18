use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmDeepResearchState {
    Initialize,
    ExtractFirstPartyEvidence,
    BuildExpertLensMatrix,
    GenerateHypotheses,
    PlanResearchTasks,
    RetrieveEvidence,
    ScoreEvidence,
    SynthesizeClaims,
    CritiqueAnswer,
    DetectGaps,
    BranchFollowupResearch,
    RewriteOrFinalize,
}

impl PmDeepResearchState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ExtractFirstPartyEvidence => "extract_first_party_evidence",
            Self::BuildExpertLensMatrix => "build_expert_lens_matrix",
            Self::GenerateHypotheses => "generate_hypotheses",
            Self::PlanResearchTasks => "plan_research_tasks",
            Self::RetrieveEvidence => "retrieve_evidence",
            Self::ScoreEvidence => "score_evidence",
            Self::SynthesizeClaims => "synthesize_claims",
            Self::CritiqueAnswer => "critique_answer",
            Self::DetectGaps => "detect_gaps",
            Self::BranchFollowupResearch => "branch_followup_research",
            Self::RewriteOrFinalize => "rewrite_or_finalize",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmDeepResearchAction {
    ContinueResearch,
    Rewrite,
    Finalize,
    AskClarification,
}

impl PmDeepResearchAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContinueResearch => "continue_research",
            Self::Rewrite => "rewrite",
            Self::Finalize => "finalize",
            Self::AskClarification => "ask_clarification",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmDeepResearchScore {
    pub evidence_coverage_score: f64,
    pub first_party_alignment_score: f64,
    pub claim_confidence_score: f64,
    pub counter_evidence_coverage_score: f64,
    pub expert_lens_coverage_score: f64,
    pub actionability_score: f64,
    pub decision_readiness_score: f64,
}

impl PmDeepResearchScore {
    pub fn clamp(mut self) -> Self {
        self.evidence_coverage_score = clamp01(self.evidence_coverage_score);
        self.first_party_alignment_score = clamp01(self.first_party_alignment_score);
        self.claim_confidence_score = clamp01(self.claim_confidence_score);
        self.counter_evidence_coverage_score = clamp01(self.counter_evidence_coverage_score);
        self.expert_lens_coverage_score = clamp01(self.expert_lens_coverage_score);
        self.actionability_score = clamp01(self.actionability_score);
        self.decision_readiness_score = clamp01(self.decision_readiness_score);
        self
    }

    pub fn decision_ready(&self) -> bool {
        self.decision_readiness_score >= 0.82
            && self.actionability_score >= 0.80
            && self.first_party_alignment_score >= 0.85
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmDeepResearchDecision {
    pub action: PmDeepResearchAction,
    pub reason: String,
    pub missing_evidence: Vec<String>,
    pub weak_claims: Vec<String>,
    pub next_queries: Vec<String>,
    pub stop_confidence: f64,
}

impl PmDeepResearchDecision {
    pub fn to_json(&self) -> Value {
        json!({
            "action": self.action.as_str(),
            "reason": self.reason,
            "missingEvidence": self.missing_evidence,
            "weakClaims": self.weak_claims,
            "nextQueries": self.next_queries,
            "stopConfidence": clamp01(self.stop_confidence),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmEvidenceScore {
    pub source_credibility: f64,
    pub freshness: f64,
    pub domain_relevance: f64,
    pub first_party_alignment: f64,
    pub claim_support: f64,
    pub conflict_level: String,
    pub usable_for_decision: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmExpertReviewScore {
    pub overall_score: f64,
    pub insight_depth_score: f64,
    pub breadth_score: f64,
    pub decision_package_score: f64,
    pub evidence_preservation_score: f64,
    pub non_blocking: bool,
    pub should_continue_research: bool,
    pub should_rewrite: bool,
    pub strengths: Vec<String>,
    pub improvement_areas: Vec<String>,
    pub preservation_notes: Vec<String>,
    pub next_research_prompts: Vec<String>,
}

impl PmExpertReviewScore {
    pub fn clamp(mut self) -> Self {
        self.overall_score = clamp01(self.overall_score);
        self.insight_depth_score = clamp01(self.insight_depth_score);
        self.breadth_score = clamp01(self.breadth_score);
        self.decision_package_score = clamp01(self.decision_package_score);
        self.evidence_preservation_score = clamp01(self.evidence_preservation_score);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmResearchBranch {
    pub id: String,
    pub title: String,
    pub lens: String,
    pub purpose: String,
    pub priority: u8,
    pub status: String,
    pub requires_external_search: bool,
    pub queries: Vec<String>,
    pub expected_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmResearchBranchQueue {
    pub branches: Vec<PmResearchBranch>,
    pub next_branch_ids: Vec<String>,
    pub queue_reason: String,
    pub dynamic_stop_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmHypothesisNode {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub confidence: f64,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmHypothesisEdge {
    pub from: String,
    pub to: String,
    pub relation: String,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmHypothesisEvidenceGraph {
    pub nodes: Vec<PmHypothesisNode>,
    pub edges: Vec<PmHypothesisEdge>,
    pub primary_evidence_node_ids: Vec<String>,
    pub unresolved_node_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmGoldenEvalHint {
    pub key: String,
    pub satisfied: bool,
    pub severity: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmGoldenEvalHints {
    pub scenario_family: String,
    pub hints: Vec<PmGoldenEvalHint>,
    pub should_add_fixture: bool,
}

impl PmResearchBranchQueue {
    pub fn select_next_branch(&self) -> Option<&PmResearchBranch> {
        self.select_next_external_branch()
            .or_else(|| self.branches.iter().min_by_key(|branch| branch.priority))
    }

    pub fn select_next_external_branch(&self) -> Option<&PmResearchBranch> {
        self.next_branch_ids
            .iter()
            .filter_map(|id| self.branches.iter().find(|branch| &branch.id == id))
            .filter(|branch| branch.requires_external_search && !branch.queries.is_empty())
            .min_by_key(|branch| branch.priority)
            .or_else(|| {
                self.branches
                    .iter()
                    .filter(|branch| branch.requires_external_search && !branch.queries.is_empty())
                    .min_by_key(|branch| branch.priority)
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmExpertLens {
    pub key: String,
    pub label: String,
    pub covered: bool,
    pub critique_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmExpertLensMatrix {
    pub lenses: Vec<PmExpertLens>,
}

impl Default for PmExpertLensMatrix {
    fn default() -> Self {
        let lenses = [
            (
                "growth",
                "Growth",
                "Does this create incremental scalable growth?",
            ),
            (
                "monetization",
                "Monetization",
                "Does this improve revenue quality without hiding cost?",
            ),
            (
                "retention",
                "Retention",
                "Does this preserve engagement, session length, and return behavior?",
            ),
            (
                "user_segmentation",
                "User Segmentation",
                "Is every major user/cohort treated differently enough?",
            ),
            (
                "value_exchange_economics",
                "Value Exchange Economics",
                "Does the value exchange improve the target outcome without hiding cost, harming trust, or creating abuse?",
            ),
            (
                "experiment_design",
                "Experiment Design",
                "Can this be launched as a controlled experiment with guardrails?",
            ),
            (
                "risk_fraud_compliance",
                "Risk/Fraud/Compliance",
                "What can be exploited, blocked by policy, or mis-measured?",
            ),
            (
                "ux_user_psychology",
                "UX/User Psychology",
                "Will users understand the value, effort, timing, and trade-off of the experience?",
            ),
            (
                "business_model_unit_economics",
                "Business Model/Unit Economics",
                "How do revenue, acquisition cost, operating cost, resource cost, and payback move?",
            ),
            (
                "platform_policy",
                "Platform Policy",
                "Does the recommendation respect relevant platform, channel, marketplace, regulator, or partner constraints for this domain?",
            ),
        ]
        .into_iter()
        .map(|(key, label, critique_prompt)| PmExpertLens {
            key: key.to_string(),
            label: label.to_string(),
            covered: false,
            critique_prompt: critique_prompt.to_string(),
        })
        .collect();
        Self { lenses }
    }
}

impl PmExpertLensMatrix {
    pub fn score_answer(&self, answer: &str) -> f64 {
        let lower = answer.to_ascii_lowercase();
        let mut covered = 0usize;
        for lens in &self.lenses {
            let hit = match lens.key.as_str() {
                "growth" => contains_any(&lower, &["growth", "增长", "扩量", "拉新"]),
                "monetization" => {
                    contains_any(&lower, &["monetization", "收入", "变现", "revenue", "roi"])
                }
                "retention" => contains_any(&lower, &["retention", "留存", "次留", "时长"]),
                "user_segmentation" => contains_any(&lower, &["segment", "cohort", "分层", "人群"]),
                "value_exchange_economics" => contains_any(
                    &lower,
                    &[
                        "value exchange",
                        "resource",
                        "cost",
                        "trust",
                        "价值",
                        "资源",
                        "成本",
                        "权益",
                    ],
                ),
                "experiment_design" => contains_any(
                    &lower,
                    &["experiment", "a/b", "holdout", "实验", "灰度", "对照"],
                ),
                "risk_fraud_compliance" => {
                    contains_any(&lower, &["risk", "fraud", "policy", "风险", "作弊", "合规"])
                }
                "ux_user_psychology" => {
                    contains_any(&lower, &["ux", "user", "体验", "用户心理", "打扰"])
                }
                "business_model_unit_economics" => contains_any(
                    &lower,
                    &["unit economics", "ltv", "成本", "回收", "收益模型"],
                ),
                "platform_policy" => contains_any(
                    &lower,
                    &[
                        "platform",
                        "policy",
                        "marketplace",
                        "channel",
                        "regulator",
                        "partner",
                        "平台",
                        "政策",
                        "渠道",
                        "监管",
                        "规则",
                        "伙伴",
                    ],
                ),
                _ => false,
            };
            if hit {
                covered = covered.saturating_add(1);
            }
        }
        if self.lenses.is_empty() {
            1.0
        } else {
            covered as f64 / self.lenses.len() as f64
        }
    }

    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PmStrategyPackageArtifact {
    pub executive_conclusion: String,
    pub priority_strategies: Vec<String>,
    pub segment_playbooks: Vec<String>,
    pub experiment_plan: Vec<String>,
    pub expected_impact_model: Vec<String>,
    pub guardrails: Vec<String>,
    pub kill_criteria: Vec<String>,
    pub rollout_plan: Vec<String>,
    pub tracking_plan: Vec<String>,
    pub counterfactuals: Vec<String>,
    pub evidence: Vec<String>,
    pub confidence: String,
    pub open_questions: Vec<String>,
}

impl PmStrategyPackageArtifact {
    pub fn to_json(&self) -> Value {
        json!(self)
    }

    pub fn render_markdown(&self, cjk: bool) -> String {
        if cjk {
            render_strategy_package_cn(self)
        } else {
            render_strategy_package_en(self)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PmDeepResearchLoopInput<'a> {
    pub plan: &'a Value,
    pub question: &'a str,
    pub answer: &'a str,
    pub quality_passed: bool,
    pub deliverable: bool,
    pub citation_count: usize,
    pub domain_count: usize,
    pub claim_count: usize,
    pub triad_coverage: f64,
    pub conflict_confidence: f64,
    pub missing: &'a [String],
    pub suggestions: &'a [String],
    pub attempt: usize,
    pub max_attempts: usize,
    pub elapsed_secs: u64,
    pub max_wall_secs: u64,
    pub no_new_evidence_repeats: usize,
    pub no_new_evidence_limit: usize,
    pub external_search_available: bool,
    pub admitted_external_evidence: bool,
    pub rejected_external_evidence_count: usize,
}

#[derive(Debug, Clone)]
pub struct PmDeepResearchLoopOutput {
    pub enabled: bool,
    pub state: PmDeepResearchState,
    pub scores: PmDeepResearchScore,
    pub decision: PmDeepResearchDecision,
    pub expert_lens_matrix: PmExpertLensMatrix,
    pub expert_review_score: PmExpertReviewScore,
    pub research_branch_queue: PmResearchBranchQueue,
    pub hypothesis_evidence_graph: PmHypothesisEvidenceGraph,
    pub golden_eval_hints: PmGoldenEvalHints,
    pub evidence_score: PmEvidenceScore,
    pub strategy_package: Option<PmStrategyPackageArtifact>,
    pub degraded: bool,
}

impl PmDeepResearchLoopOutput {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "loopState": self.state.as_str(),
            "scores": self.scores,
            "decision": self.decision.to_json(),
            "expertLensMatrix": self.expert_lens_matrix.to_json(),
            "expertReviewScore": self.expert_review_score,
            "researchBranchQueue": self.research_branch_queue,
            "hypothesisEvidenceGraph": self.hypothesis_evidence_graph,
            "goldenEvalHints": self.golden_eval_hints,
            "evidenceScore": self.evidence_score,
            "strategyPackage": self.strategy_package.as_ref().map(PmStrategyPackageArtifact::to_json),
            "degraded": self.degraded,
        })
    }
}

pub struct PmDeepResearchLoop;

impl PmDeepResearchLoop {
    pub fn should_enable(plan: &Value, _question: &str) -> bool {
        let mode = plan
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .eq_ignore_ascii_case("business_report_strategy");
        if mode {
            return true;
        }
        let task_graph = plan.get("taskGraph").unwrap_or(&Value::Null);
        let complexity = task_graph
            .get("complexityScore")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let complexity_100 = if (0.0..=1.0).contains(&complexity) {
            complexity * 100.0
        } else {
            complexity
        };
        let decomposition = task_graph
            .get("decompositionMode")
            .and_then(Value::as_str)
            .unwrap_or("none");
        complexity_100 >= 72.0 || matches!(decomposition, "full" | "deep")
    }

    pub fn evaluate(input: PmDeepResearchLoopInput<'_>) -> PmDeepResearchLoopOutput {
        let enabled = Self::should_enable(input.plan, input.question);
        let lens_matrix = PmExpertLensMatrix::default();
        let expert_review_score = compute_expert_review_score(&input, &lens_matrix).clamp();
        let scores = compute_scores(&input, &lens_matrix, &expert_review_score).clamp();
        let evidence_score = compute_evidence_score(&input);
        let research_branch_queue =
            build_research_branch_queue(&input, &lens_matrix, &expert_review_score);
        let hypothesis_evidence_graph =
            build_hypothesis_evidence_graph(&input, &scores, &evidence_score);
        let golden_eval_hints = build_golden_eval_hints(&input, &expert_review_score);
        let hard_limit_reached = input.elapsed_secs >= input.max_wall_secs
            || input.attempt >= input.max_attempts
            || input.no_new_evidence_repeats >= input.no_new_evidence_limit;
        let diagnostic_leaked = contains_diagnostic_noise(input.answer);
        let missing = input.missing.iter().take(8).cloned().collect::<Vec<_>>();
        let mut weak_claims = input
            .suggestions
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>();
        for item in expert_review_score.improvement_areas.iter().take(4) {
            push_unique(&mut weak_claims, item);
        }
        let mut next_queries = extract_next_queries(input.plan, input.missing);
        for prompt in expert_review_score.next_research_prompts.iter().take(4) {
            push_unique(&mut next_queries, prompt);
        }
        for branch in research_branch_queue.branches.iter().take(3) {
            if !branch.requires_external_search {
                continue;
            }
            for query in branch.queries.iter().take(2) {
                push_unique(&mut next_queries, query);
            }
        }
        let external_search_failed_admission = input.external_search_available
            && !input.admitted_external_evidence
            && input.rejected_external_evidence_count > 0;
        let first_party_is_primary = has_first_party_evidence(input.plan);
        let external_search_exhausted_for_first_party = first_party_is_primary
            && external_search_failed_admission
            && input.no_new_evidence_repeats > 0;
        let has_external_followup_branch = research_branch_queue
            .branches
            .iter()
            .any(|branch| branch.requires_external_search && !branch.queries.is_empty());
        let no_new_external_progress = input.no_new_evidence_repeats >= input.no_new_evidence_limit
            && first_party_is_primary
            && !input.admitted_external_evidence;
        let action = if !enabled
            || (scores.decision_ready() && input.quality_passed && !diagnostic_leaked)
        {
            PmDeepResearchAction::Finalize
        } else if hard_limit_reached
            || diagnostic_leaked
            || no_new_external_progress
            || external_search_exhausted_for_first_party
        {
            PmDeepResearchAction::Rewrite
        } else if (external_search_failed_admission
            && input.attempt < input.max_attempts
            && !next_queries.is_empty())
            || (expert_review_score.should_continue_research && has_external_followup_branch)
        {
            PmDeepResearchAction::ContinueResearch
        } else if scores.actionability_score < 0.55 || expert_review_score.should_rewrite {
            if input.external_search_available
                && has_external_followup_branch
                && !next_queries.is_empty()
            {
                PmDeepResearchAction::ContinueResearch
            } else {
                PmDeepResearchAction::Rewrite
            }
        } else {
            PmDeepResearchAction::Rewrite
        };
        let state = match action {
            PmDeepResearchAction::Finalize => PmDeepResearchState::RewriteOrFinalize,
            PmDeepResearchAction::Rewrite => {
                if hard_limit_reached {
                    PmDeepResearchState::RewriteOrFinalize
                } else {
                    PmDeepResearchState::CritiqueAnswer
                }
            }
            PmDeepResearchAction::ContinueResearch => {
                if next_queries.is_empty() {
                    PmDeepResearchState::DetectGaps
                } else {
                    PmDeepResearchState::BranchFollowupResearch
                }
            }
            PmDeepResearchAction::AskClarification => PmDeepResearchState::DetectGaps,
        };
        let reason = if !enabled {
            "deep research loop not required for this turn".to_string()
        } else if action == PmDeepResearchAction::Finalize {
            "decision-ready thresholds met".to_string()
        } else if hard_limit_reached {
            "safety budget reached; rewrite best available evidence into strategy package"
                .to_string()
        } else if diagnostic_leaked {
            "visible answer contains tool/runtime diagnostics; rewrite required".to_string()
        } else if no_new_external_progress {
            "no new admitted external evidence after repeated attempts; rewrite from first-party data and best available evidence".to_string()
        } else if external_search_exhausted_for_first_party {
            "external search did not produce admitted source-backed evidence; first-party data is primary, so rewrite the best answer instead of continuing low-yield retrieval".to_string()
        } else if external_search_failed_admission {
            "external search returned evidence but none passed admission; continue with diversified retrieval or discard it for expert-only synthesis".to_string()
        } else if expert_review_score.should_continue_research {
            "expert review found non-blocking depth or coverage gaps; continue targeted research without dropping existing evidence".to_string()
        } else if expert_review_score.should_rewrite {
            "expert review recommends rewriting the best available evidence into a stronger strategy package".to_string()
        } else {
            "quality gaps remain; continue targeted research or rewrite weak claims".to_string()
        };
        let decision = PmDeepResearchDecision {
            action,
            reason,
            missing_evidence: missing,
            weak_claims,
            next_queries,
            stop_confidence: scores.decision_readiness_score,
        };
        let degraded = enabled
            && matches!(decision.action, PmDeepResearchAction::Rewrite)
            && !scores.decision_ready();
        let strategy_package =
            if enabled && matches!(decision.action, PmDeepResearchAction::Finalize) {
                Some(build_strategy_package(&input, &scores, &evidence_score))
            } else {
                None
            };

        PmDeepResearchLoopOutput {
            enabled,
            state,
            scores,
            decision,
            expert_lens_matrix: lens_matrix,
            expert_review_score,
            research_branch_queue,
            hypothesis_evidence_graph,
            golden_eval_hints,
            evidence_score,
            strategy_package,
            degraded,
        }
    }
}

pub fn pm_deep_loop_max_wall_secs() -> u64 {
    std::env::var("PM_DEEP_LOOP_MAX_WALL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(225)
        .max(120)
}

pub fn pm_deep_loop_no_new_evidence_limit() -> usize {
    std::env::var("PM_DEEP_LOOP_NO_NEW_EVIDENCE_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

pub fn pm_deep_loop_min_synthesis_window_secs() -> u64 {
    std::env::var("PM_DEEP_LOOP_MIN_SYNTHESIS_WINDOW_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        // Reserve enough wall time for a long, cited decision report. Retrieval
        // must stop before it consumes the window needed to synthesize.
        .unwrap_or(120)
        .max(60)
}

pub fn contains_diagnostic_noise(text: &str) -> bool {
    [
        "durationMs",
        "duration_ms",
        "toolCallCount",
        "contentChars",
        "sourceSlotBudgetSecs",
        "pipelineTimeoutSecs",
        "routeAllowlist",
        "EXEC_CONSTRAINTS",
        "TASK_GRAPH",
        "runtime execution failed",
        "direct answer turn timed out",
    ]
    .iter()
    .any(|token| text.contains(token))
}

fn compute_scores(
    input: &PmDeepResearchLoopInput<'_>,
    lens_matrix: &PmExpertLensMatrix,
    expert_review: &PmExpertReviewScore,
) -> PmDeepResearchScore {
    let answer = input.answer;
    let evidence_coverage = if input.admitted_external_evidence {
        ((input.citation_count.min(6) as f64 / 6.0) * 0.55
            + (input.domain_count.min(3) as f64 / 3.0) * 0.25
            + input.triad_coverage.clamp(0.0, 1.0) * 0.20)
            .clamp(0.0, 1.0)
    } else if input.external_search_available && input.rejected_external_evidence_count > 0 {
        if has_first_party_evidence(input.plan) {
            0.58
        } else {
            0.22
        }
    } else if has_first_party_evidence(input.plan) {
        0.72
    } else if input.deliverable {
        0.55
    } else {
        0.25
    };
    let first_party_alignment = score_first_party_alignment(input.plan, input.question, answer);
    let claim_confidence = if input.quality_passed {
        0.90_f64
    } else if input.deliverable {
        0.66_f64
    } else {
        0.36_f64
    }
    .max(input.triad_coverage * 0.85)
    .min(1.0);
    let counter_evidence = if input.conflict_confidence >= 0.55 {
        input.conflict_confidence
    } else if input.domain_count >= 2 {
        0.58
    } else if input.external_search_available && input.rejected_external_evidence_count > 0 {
        0.22
    } else if input.external_search_available {
        0.35
    } else {
        0.48
    };
    let lens_score =
        lens_matrix
            .score_answer(answer)
            .max(if has_strategy_package_sections(answer) {
                0.78
            } else {
                0.0
            });
    let actionability = score_actionability(answer);
    let decision_readiness = evidence_coverage * 0.18
        + first_party_alignment * 0.22
        + claim_confidence * 0.16
        + counter_evidence * 0.10
        + lens_score * 0.14
        + actionability * 0.14
        + expert_review.overall_score * 0.06;
    PmDeepResearchScore {
        evidence_coverage_score: evidence_coverage,
        first_party_alignment_score: first_party_alignment,
        claim_confidence_score: claim_confidence,
        counter_evidence_coverage_score: counter_evidence,
        expert_lens_coverage_score: lens_score,
        actionability_score: actionability,
        decision_readiness_score: decision_readiness,
    }
}

fn compute_expert_review_score(
    input: &PmDeepResearchLoopInput<'_>,
    lens_matrix: &PmExpertLensMatrix,
) -> PmExpertReviewScore {
    let answer = input.answer.trim();
    let lens_score = lens_matrix.score_answer(answer);
    let strategy_sections = score_strategy_package_completeness(answer);
    let depth_score = score_expert_depth(answer, input);
    let evidence_preservation = score_evidence_preservation(input);
    let breadth_score = lens_score.max(if strategy_sections >= 0.70 { 0.72 } else { 0.0 });
    let overall = depth_score * 0.30
        + breadth_score * 0.22
        + strategy_sections * 0.28
        + evidence_preservation * 0.20;
    let mut strengths = Vec::new();
    let mut improvement_areas = Vec::new();
    let mut preservation_notes = Vec::new();
    let mut next_research_prompts = Vec::new();

    if strategy_sections >= 0.80 {
        push_unique(
            &mut strengths,
            "answer contains an executable strategy-package structure",
        );
    } else {
        push_unique(
            &mut improvement_areas,
            "strengthen executable strategy package sections: priority, segments, experiments, impact, guardrails, kill criteria, rollout, tracking, counterfactuals",
        );
    }
    if lens_score >= 0.70 {
        push_unique(&mut strengths, "multiple expert lenses are represented");
    } else {
        push_unique(
            &mut improvement_areas,
            "expand expert-lens coverage across growth, monetization, retention, segmentation, value exchange economics, experiment design, risk, UX, and policy",
        );
        push_unique(
            &mut next_research_prompts,
            "find evidence or mechanisms for missing PM expert lenses and their trade-offs",
        );
    }
    if depth_score >= 0.72 {
        push_unique(
            &mut strengths,
            "recommendations include decision logic beyond generic advice",
        );
    } else {
        push_unique(
            &mut improvement_areas,
            "add causal paths, trade-offs, assumptions, expected impact, and counterfactual explanations for each recommendation",
        );
    }
    if evidence_preservation >= 0.82 {
        push_unique(
            &mut preservation_notes,
            "first-party evidence is preserved in the visible answer or fallback artifact",
        );
    } else {
        push_unique(
            &mut improvement_areas,
            "preserve and cite first-party metrics, cohorts, constraints, and historical experiment lessons before adding external context",
        );
        push_unique(
            &mut preservation_notes,
            "expert review is non-blocking: weak preservation triggers rewrite/research, not evidence deletion",
        );
    }
    if input.external_search_available && !input.admitted_external_evidence {
        push_unique(
            &mut improvement_areas,
            "external search was available but produced no admitted source-backed evidence; discard weak snippets and either retry with diversified queries or label external evidence as unavailable",
        );
        if input.rejected_external_evidence_count > 0 {
            push_unique(
                &mut next_research_prompts,
                "retry with a different evidence angle because prior external snippets failed source/relevance admission",
            );
        }
    }
    if !input.external_search_available {
        push_unique(
            &mut preservation_notes,
            "external evidence unavailable; use first-party evidence plus conservative expert reasoning",
        );
    }
    for missing in input.missing.iter().take(3) {
        push_unique(
            &mut next_research_prompts,
            format!("close quality gap: {}", missing.replace('_', " ")),
        );
    }
    if contains_diagnostic_noise(answer) {
        push_unique(
            &mut improvement_areas,
            "remove tool/runtime diagnostics from the visible answer",
        );
    }
    let should_rewrite =
        contains_diagnostic_noise(answer) || (strategy_sections < 0.25 && input.deliverable);
    let should_continue_research = !should_rewrite
        && input.external_search_available
        && input.attempt < input.max_attempts
        && (overall < 0.74
            || lens_score < 0.58
            || evidence_preservation < 0.74
            || (input.rejected_external_evidence_count > 0 && !input.admitted_external_evidence));

    PmExpertReviewScore {
        overall_score: overall,
        insight_depth_score: depth_score,
        breadth_score,
        decision_package_score: strategy_sections,
        evidence_preservation_score: evidence_preservation,
        non_blocking: true,
        should_continue_research,
        should_rewrite,
        strengths,
        improvement_areas,
        preservation_notes,
        next_research_prompts,
    }
}

fn score_strategy_package_completeness(answer: &str) -> f64 {
    let lower = answer.to_ascii_lowercase();
    let checks = [
        contains_any(&lower, &["executive", "结论", "关键结论"]),
        contains_any(&lower, &["priority", "优先", "先做"]),
        contains_any(&lower, &["segment", "cohort", "分层", "人群"]),
        contains_any(
            &lower,
            &["experiment", "a/b", "holdout", "实验", "灰度", "对照"],
        ),
        contains_any(&lower, &["impact", "收益", "成本", "预期", "影响"]),
        contains_any(&lower, &["guardrail", "保护指标", "护栏"]),
        contains_any(&lower, &["kill", "stop", "回滚", "停止", "止损"]),
        contains_any(&lower, &["rollout", "灰度", "上线", "节奏"]),
        contains_any(&lower, &["tracking", "instrument", "埋点", "指标", "看板"]),
        contains_any(&lower, &["counterfactual", "反事实", "如果"]),
    ];
    checks.iter().filter(|item| **item).count() as f64 / checks.len() as f64
}

fn score_expert_depth(answer: &str, input: &PmDeepResearchLoopInput<'_>) -> f64 {
    let lower = answer.to_ascii_lowercase();
    let mut score = 0.0_f64;
    if input.claim_count >= 5 || answer.chars().count() >= 900 {
        score += 0.18;
    }
    if contains_any(&lower, &["because", "why", "原因", "路径", "因果"]) {
        score += 0.14;
    }
    if contains_any(&lower, &["trade-off", "tradeoff", "权衡", "代价"]) {
        score += 0.12;
    }
    if contains_any(&lower, &["assumption", "hypothesis", "假设"]) {
        score += 0.12;
    }
    if contains_any(
        &lower,
        &["risk", "fraud", "compliance", "风险", "作弊", "合规"],
    ) {
        score += 0.12;
    }
    if contains_any(&lower, &["counterfactual", "反事实", "如果"]) {
        score += 0.12;
    }
    if contains_any(
        &lower,
        &["expected impact", "impact model", "预期影响", "收益模型"],
    ) {
        score += 0.10;
    }
    if contains_any(&lower, &["metric", "tracking", "埋点", "指标"]) {
        score += 0.10;
    }
    score.min(1.0)
}

fn score_evidence_preservation(input: &PmDeepResearchLoopInput<'_>) -> f64 {
    if !has_first_party_evidence(input.plan) {
        return if input.answer.trim().is_empty() {
            0.0
        } else {
            0.62
        };
    }
    let first_party_alignment =
        score_first_party_alignment(input.plan, input.question, input.answer);
    let ctx = FirstPartyStrategyContext::from_plan(input.plan);
    let mut evidence_hits = 0usize;
    let mut evidence_total = 0usize;
    let answer_lower = input.answer.to_ascii_lowercase();
    for item in ctx
        .metrics
        .iter()
        .chain(ctx.cohorts.iter())
        .chain(ctx.objectives.iter())
        .chain(ctx.guardrails.iter())
        .take(12)
    {
        evidence_total = evidence_total.saturating_add(1);
        let compact = item.to_ascii_lowercase();
        let first_token = compact
            .split(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '=' | '，' | ',' | ';'))
            .find(|part| part.chars().count() >= 2)
            .unwrap_or("");
        if (!first_token.is_empty() && answer_lower.contains(first_token))
            || input.answer.contains(item)
        {
            evidence_hits = evidence_hits.saturating_add(1);
        }
    }
    let explicit_evidence_ratio = if evidence_total == 0 {
        0.72
    } else {
        evidence_hits as f64 / evidence_total as f64
    };
    (first_party_alignment * 0.65 + explicit_evidence_ratio * 0.35).clamp(0.0, 1.0)
}

fn build_research_branch_queue(
    input: &PmDeepResearchLoopInput<'_>,
    lens_matrix: &PmExpertLensMatrix,
    expert_review: &PmExpertReviewScore,
) -> PmResearchBranchQueue {
    let mut branches = Vec::new();
    let ctx = FirstPartyStrategyContext::from_plan(input.plan);
    let base_queries = extract_next_queries(input.plan, input.missing);
    let mut next_id = 1usize;

    if expert_review.evidence_preservation_score < 0.82 && ctx.has_evidence() {
        branches.push(PmResearchBranch {
            id: format!("branch-{next_id}"),
            title: "Preserve and validate first-party evidence".to_string(),
            lens: "first_party_alignment".to_string(),
            purpose: "Ensure extracted metrics, cohorts, constraints, and historical lessons remain primary before adding external context.".to_string(),
            priority: 1,
            status: "planned".to_string(),
            requires_external_search: false,
            queries: Vec::new(),
            expected_evidence: vec![
                "metric/cohort references from user report".to_string(),
                "constraints and failed-experiment lessons".to_string(),
            ],
        });
        next_id = next_id.saturating_add(1);
    }

    if expert_review.breadth_score < 0.70 {
        for lens in lens_matrix.lenses.iter().take(10) {
            if branches.len() >= 4 {
                break;
            }
            let title = format!("Close {} lens gap", lens.label);
            branches.push(PmResearchBranch {
                id: format!("branch-{next_id}"),
                title,
                lens: lens.key.clone(),
                purpose: lens.critique_prompt.clone(),
                priority: if matches!(
                    lens.key.as_str(),
                    "experiment_design" | "business_model_unit_economics" | "risk_fraud_compliance"
                ) {
                    1
                } else {
                    2
                },
                status: "planned".to_string(),
                requires_external_search: lens_requires_external_search(&lens.key),
                queries: build_branch_queries(&ctx, &lens.key, &base_queries),
                expected_evidence: expected_evidence_for_lens(&lens.key),
            });
            next_id = next_id.saturating_add(1);
        }
    }

    if expert_review.decision_package_score < 0.80 {
        branches.push(PmResearchBranch {
            id: format!("branch-{next_id}"),
            title: "Turn weak answer into executable strategy package".to_string(),
            lens: "decision_package".to_string(),
            purpose: "Fill priority, segment playbooks, experiments, impact model, guardrails, kill criteria, rollout, tracking, and counterfactuals.".to_string(),
            priority: 1,
            status: "planned".to_string(),
            requires_external_search: false,
            queries: Vec::new(),
            expected_evidence: vec![
                "strategy sections mapped to first-party cohorts".to_string(),
                "explicit validation and rollback rules".to_string(),
            ],
        });
    }

    branches.sort_by_key(|branch| branch.priority);
    branches.truncate(6);
    let next_branch_ids = branches
        .iter()
        .filter(|branch| branch.priority <= 1 && branch.requires_external_search)
        .take(3)
        .map(|branch| branch.id.clone())
        .collect::<Vec<_>>();
    let queue_reason = if branches.is_empty() {
        "No follow-up branches required by the current non-blocking expert review.".to_string()
    } else {
        "Branches are planned from evidence preservation, missing expert lenses, and executable-package gaps.".to_string()
    };
    PmResearchBranchQueue {
        branches,
        next_branch_ids,
        queue_reason,
        dynamic_stop_note:
            "Continue while branch evidence improves decision readiness; stop when core hypotheses, counterevidence, and actionability are sufficient or safety budget is reached."
                .to_string(),
    }
}

fn lens_requires_external_search(lens_key: &str) -> bool {
    matches!(
        lens_key,
        "growth"
            | "monetization"
            | "retention"
            | "value_exchange_economics"
            | "risk_fraud_compliance"
            | "ux_user_psychology"
            | "business_model_unit_economics"
            | "platform_policy"
    )
}

fn build_branch_queries(
    ctx: &FirstPartyStrategyContext,
    lens_key: &str,
    base_queries: &[String],
) -> Vec<String> {
    let context = ctx
        .context_terms
        .first()
        .cloned()
        .or_else(|| ctx.objectives.first().cloned())
        .unwrap_or_else(|| "product strategy".to_string());
    let lens_phrase = match lens_key {
        "growth" => "growth mechanism and activation lever",
        "monetization" => "monetization quality revenue cost tradeoff",
        "retention" => "retention engagement guardrail",
        "user_segmentation" => "cohort segmentation playbook",
        "value_exchange_economics" => "value exchange resource cost trust abuse risk",
        "experiment_design" => "experiment design holdout kill criteria",
        "risk_fraud_compliance" => "risk fraud compliance policy",
        "ux_user_psychology" => "user psychology UX friction",
        "business_model_unit_economics" => "unit economics sensitivity model",
        "platform_policy" => "platform policy constraints",
        _ => "strategy evidence",
    };
    let mut queries = Vec::new();
    push_unique(&mut queries, format!("{context} {lens_phrase}"));
    for query in base_queries.iter().take(2) {
        push_unique(&mut queries, query);
    }
    queries
}

fn expected_evidence_for_lens(lens_key: &str) -> Vec<String> {
    match lens_key {
        "experiment_design" => vec![
            "sample split and holdout design".to_string(),
            "success metric and kill criteria".to_string(),
        ],
        "business_model_unit_economics" => vec![
            "revenue/cost sensitivity assumptions".to_string(),
            "primary metric movement path".to_string(),
        ],
        "risk_fraud_compliance" => vec![
            "risk or abuse failure modes".to_string(),
            "policy/compliance constraints".to_string(),
        ],
        "user_segmentation" => vec![
            "cohort-specific behavior signal".to_string(),
            "segment-specific recommendation".to_string(),
        ],
        _ => vec![
            "domain-relevant mechanism evidence".to_string(),
            "counterexample or guardrail".to_string(),
        ],
    }
}

fn build_hypothesis_evidence_graph(
    input: &PmDeepResearchLoopInput<'_>,
    scores: &PmDeepResearchScore,
    evidence_score: &PmEvidenceScore,
) -> PmHypothesisEvidenceGraph {
    let ctx = FirstPartyStrategyContext::from_plan(input.plan);
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut primary = Vec::new();
    let mut unresolved = Vec::new();

    nodes.push(PmHypothesisNode {
        id: "first_party_report".to_string(),
        kind: "evidence".to_string(),
        title: if ctx.metrics.is_empty() && ctx.cohorts.is_empty() {
            "User-provided first-party context".to_string()
        } else {
            format!(
                "First-party report: {} metrics, {} cohorts",
                ctx.metrics.len(),
                ctx.cohorts.len()
            )
        },
        confidence: evidence_score.first_party_alignment,
        evidence_refs: ctx
            .metrics
            .iter()
            .chain(ctx.cohorts.iter())
            .take(6)
            .cloned()
            .collect(),
    });
    primary.push("first_party_report".to_string());

    nodes.push(PmHypothesisNode {
        id: "core_strategy_hypothesis".to_string(),
        kind: "hypothesis".to_string(),
        title: "Segmented strategy can improve the primary objective without violating guardrails"
            .to_string(),
        confidence: scores.decision_readiness_score,
        evidence_refs: ctx
            .objectives
            .iter()
            .chain(ctx.guardrails.iter())
            .take(6)
            .cloned()
            .collect(),
    });
    edges.push(PmHypothesisEdge {
        from: "first_party_report".to_string(),
        to: "core_strategy_hypothesis".to_string(),
        relation: "supports".to_string(),
        strength: scores.first_party_alignment_score,
    });

    nodes.push(PmHypothesisNode {
        id: "counterevidence_check".to_string(),
        kind: "counterevidence".to_string(),
        title:
            "Check whether the strategy is only shifting cost, hiding risk, or hurting guardrails"
                .to_string(),
        confidence: scores.counter_evidence_coverage_score,
        evidence_refs: ctx
            .failed_experiments
            .iter()
            .chain(ctx.anti_patterns.iter())
            .take(6)
            .cloned()
            .collect(),
    });
    edges.push(PmHypothesisEdge {
        from: "counterevidence_check".to_string(),
        to: "core_strategy_hypothesis".to_string(),
        relation: "challenges".to_string(),
        strength: 1.0 - scores.counter_evidence_coverage_score,
    });
    if scores.counter_evidence_coverage_score < 0.55 {
        unresolved.push("counterevidence_check".to_string());
    }

    nodes.push(PmHypothesisNode {
        id: "decision_package".to_string(),
        kind: "decision".to_string(),
        title: "Executable strategy package with experiments, rollout, tracking, and kill criteria"
            .to_string(),
        confidence: scores.actionability_score,
        evidence_refs: Vec::new(),
    });
    edges.push(PmHypothesisEdge {
        from: "core_strategy_hypothesis".to_string(),
        to: "decision_package".to_string(),
        relation: "drives".to_string(),
        strength: scores.actionability_score,
    });
    if scores.actionability_score < 0.80 {
        unresolved.push("decision_package".to_string());
    }

    PmHypothesisEvidenceGraph {
        nodes,
        edges,
        primary_evidence_node_ids: primary,
        unresolved_node_ids: unresolved,
    }
}

fn build_golden_eval_hints(
    input: &PmDeepResearchLoopInput<'_>,
    expert_review: &PmExpertReviewScore,
) -> PmGoldenEvalHints {
    let answer = input.answer;
    let lower = answer.to_ascii_lowercase();
    let scenario_family = if has_first_party_evidence(input.plan) {
        "business_report_strategy".to_string()
    } else if input.external_search_available {
        "external_research_strategy".to_string()
    } else {
        "direct_strategy_reasoning".to_string()
    };
    let checks = [
        (
            "first_party_preserved",
            expert_review.evidence_preservation_score >= 0.74,
            "high",
            "First-party metrics/cohorts/constraints should be visible and primary.",
        ),
        (
            "segment_playbooks",
            contains_any(&lower, &["segment", "cohort", "分层", "人群"]),
            "high",
            "Complex strategy answers should contain segment or scenario playbooks.",
        ),
        (
            "experiment_rules",
            contains_any(
                &lower,
                &["experiment", "a/b", "holdout", "实验", "灰度", "对照"],
            ),
            "high",
            "Answer should include experiment rules and holdout logic.",
        ),
        (
            "guardrails_kill_criteria",
            contains_any(&lower, &["guardrail", "kill", "保护指标", "回滚", "停止"]),
            "high",
            "Answer should include guardrails and kill criteria.",
        ),
        (
            "counterfactuals",
            contains_any(&lower, &["counterfactual", "反事实", "如果"]),
            "medium",
            "Answer should challenge itself with counterfactuals or failure modes.",
        ),
    ];
    let hints = checks
        .into_iter()
        .map(|(key, satisfied, severity, note)| PmGoldenEvalHint {
            key: key.to_string(),
            satisfied,
            severity: severity.to_string(),
            note: note.to_string(),
        })
        .collect::<Vec<_>>();
    let failed_high = hints
        .iter()
        .filter(|hint| hint.severity == "high" && !hint.satisfied)
        .count();
    PmGoldenEvalHints {
        scenario_family,
        hints,
        should_add_fixture: failed_high > 0 || expert_review.overall_score < 0.68,
    }
}

fn compute_evidence_score(input: &PmDeepResearchLoopInput<'_>) -> PmEvidenceScore {
    let first_party = has_first_party_evidence(input.plan);
    let conflict_level = if input.conflict_confidence >= 0.70 {
        "minor"
    } else if input.domain_count >= 2 && input.conflict_confidence < 0.45 {
        "major"
    } else {
        "none"
    };
    PmEvidenceScore {
        source_credibility: if input.admitted_external_evidence && input.domain_count >= 2 {
            0.76
        } else if input.rejected_external_evidence_count > 0 {
            0.22
        } else {
            0.55
        },
        freshness: if input.admitted_external_evidence && input.citation_count > 0 {
            0.72
        } else {
            0.50
        },
        domain_relevance: if input.admitted_external_evidence && input.citation_count > 0 {
            0.72
        } else if input.rejected_external_evidence_count > 0 {
            0.25
        } else {
            0.58
        },
        first_party_alignment: if first_party { 0.92 } else { 0.45 },
        claim_support: input
            .triad_coverage
            .max(if input.deliverable { 0.55 } else { 0.25 }),
        conflict_level: conflict_level.to_string(),
        usable_for_decision: input.deliverable || first_party,
    }
}

#[derive(Debug, Clone, Default)]
struct FirstPartyStrategyContext {
    context_terms: Vec<String>,
    objectives: Vec<String>,
    metrics: Vec<String>,
    guardrails: Vec<String>,
    cohorts: Vec<String>,
    existing_mechanics: Vec<String>,
    failed_experiments: Vec<String>,
    anti_patterns: Vec<String>,
    snippets: Vec<String>,
}

impl FirstPartyStrategyContext {
    fn from_plan(plan: &Value) -> Self {
        let first_party = first_party_evidence(plan);
        let Some(first_party) = first_party else {
            return Self::default();
        };
        Self {
            context_terms: collect_value_labels(first_party.get("contextTerms"), 5),
            objectives: collect_value_labels(first_party.get("objectives"), 5),
            metrics: collect_metric_labels(first_party.get("metrics"), 8),
            guardrails: collect_value_labels(first_party.get("guardrails"), 8),
            cohorts: collect_value_labels(first_party.get("opportunityCohorts"), 6),
            existing_mechanics: collect_value_labels(first_party.get("existingMechanics"), 8),
            failed_experiments: collect_value_labels(first_party.get("failedExperiments"), 6),
            anti_patterns: collect_value_labels(first_party.get("antiPatterns"), 8),
            snippets: collect_value_labels(first_party.get("rawEvidenceSnippets"), 5),
        }
    }

    fn has_evidence(&self) -> bool {
        !self.context_terms.is_empty()
            || !self.objectives.is_empty()
            || !self.metrics.is_empty()
            || !self.guardrails.is_empty()
            || !self.cohorts.is_empty()
            || !self.snippets.is_empty()
    }

    fn focus_cn(&self) -> String {
        if !self.context_terms.is_empty() {
            format!(
                "业务上下文：{}。",
                join_limited(&self.context_terms, 3, "、")
            )
        } else {
            "业务上下文以用户报告为准。".to_string()
        }
    }

    fn focus_en(&self) -> String {
        if !self.context_terms.is_empty() {
            format!(
                "Business context: {}.",
                join_limited(&self.context_terms, 3, ", ")
            )
        } else {
            "Business context is taken from the user's report.".to_string()
        }
    }

    fn metric_sentence_cn(&self) -> String {
        if self.metrics.is_empty() {
            return "一手报告未抽取到稳定指标值；策略需先补齐基线指标口径。".to_string();
        }
        format!("一手指标：{}。", join_limited(&self.metrics, 8, "、"))
    }

    fn metric_sentence_en(&self) -> String {
        if self.metrics.is_empty() {
            return "No stable first-party metric values were extracted; establish baseline definitions before scaling.".to_string();
        }
        format!(
            "First-party metrics: {}.",
            join_limited(&self.metrics, 8, ", ")
        )
    }

    fn objective_sentence_cn(&self) -> String {
        if self.objectives.is_empty() {
            "核心目标：提升用户报告中的主指标，同时保持关键健康指标不劣化。".to_string()
        } else {
            format!("核心目标：{}。", join_limited(&self.objectives, 5, "、"))
        }
    }

    fn objective_sentence_en(&self) -> String {
        if self.objectives.is_empty() {
            "Core objective: improve the report's primary metric while protecting health metrics."
                .to_string()
        } else {
            format!(
                "Core objectives: {}.",
                join_limited(&self.objectives, 5, ", ")
            )
        }
    }

    fn guardrail_items_cn(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.guardrails.is_empty() {
            out.push("保护用户报告中的核心健康指标，不允许主指标提升但体验、留存、收入质量或成本结构恶化。".to_string());
        } else {
            for item in self.guardrails.iter().take(6) {
                out.push(format!("保护 {item}，实验组不得显著差于对照组。"));
            }
        }
        out.push(
            "每个策略必须保留 holdout，对照组和分层日志，防止把自然波动误判为策略收益。"
                .to_string(),
        );
        out
    }

    fn guardrail_items_en(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.guardrails.is_empty() {
            out.push("Protect the report's core health metrics; the primary metric must not improve by degrading experience, retention, revenue quality, or cost structure.".to_string());
        } else {
            for item in self.guardrails.iter().take(6) {
                out.push(format!(
                    "{item} must not be significantly worse than control."
                ));
            }
        }
        out.push("Keep holdouts and cohort-level logs for every strategy so natural volatility is not mistaken for lift.".to_string());
        out
    }
}

fn first_party_evidence(plan: &Value) -> Option<&Value> {
    plan.get("reportStrategy")
        .and_then(|value| value.get("firstPartyEvidenceJson"))
        .filter(|value| !value.is_null())
}

fn collect_metric_labels(value: Option<&Value>, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let Some(Value::Array(items)) = value else {
        return out;
    };
    for item in items.iter().take(limit.saturating_mul(2)) {
        match item {
            Value::Object(obj) => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("").trim();
                let metric_value = obj
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() && !metric_value.is_empty() {
                    if let Some(label) =
                        clean_first_party_display_label(&format!("{name}={metric_value}"), 96)
                    {
                        push_unique(&mut out, label);
                    }
                } else if !name.is_empty() {
                    if let Some(label) = clean_first_party_display_label(name, 96) {
                        push_unique(&mut out, label);
                    }
                }
            }
            Value::String(text) => {
                if let Some(label) = clean_first_party_display_label(text, 96) {
                    push_unique(&mut out, label);
                }
            }
            _ => {}
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn collect_value_labels(value: Option<&Value>, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Some(Value::Array(items)) => {
            for item in items.iter().take(limit.saturating_mul(2)) {
                if let Some(label) = label_from_value(item) {
                    push_unique(&mut out, label);
                }
                if out.len() >= limit {
                    break;
                }
            }
        }
        Some(value) => {
            if let Some(label) = label_from_value(value) {
                push_unique(&mut out, label);
            }
        }
        None => {}
    }
    out
}

fn label_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => clean_first_party_display_label(text, 180),
        Value::Number(value) => Some(value.to_string()),
        Value::Object(obj) => {
            for key in ["cohort", "title", "label", "name", "objective", "metric"] {
                if let Some(text) = obj.get(key).and_then(Value::as_str) {
                    let Some(mut label) = clean_first_party_display_label(text, 160) else {
                        continue;
                    };
                    if let Some(value) = obj.get("value").and_then(Value::as_str) {
                        if let Some(clean_value) = clean_first_party_display_label(value, 80) {
                            label.push('=');
                            label.push_str(&clean_value);
                        }
                    }
                    if let Some(why) = obj
                        .get("why")
                        .or_else(|| obj.get("lesson"))
                        .or_else(|| obj.get("result"))
                        .or_else(|| obj.get("strategyHint"))
                        .and_then(Value::as_str)
                    {
                        if let Some(why) = clean_first_party_display_label(why, 140) {
                            label.push_str(": ");
                            label.push_str(&why);
                        }
                    }
                    return clean_first_party_display_label(&label, 220);
                }
            }
            obj.values()
                .filter_map(Value::as_str)
                .find_map(|text| clean_first_party_display_label(text, 180))
        }
        _ => None,
    }
}

fn join_limited(items: &[String], limit: usize, sep: &str) -> String {
    items
        .iter()
        .filter(|item| !item.trim().is_empty())
        .take(limit)
        .cloned()
        .collect::<Vec<_>>()
        .join(sep)
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max_chars {
        return clean;
    }
    clean
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation() || matches!(ch, '，' | '。' | '；' | '：' | '、')
        })
        .trim()
        .to_string()
}

fn clean_first_party_display_label(raw: &str, max_chars: usize) -> Option<String> {
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalize_display_metric_spacing(&clean);
    let trimmed = normalized
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'))
        .trim();
    if trimmed.is_empty() || is_dirty_first_party_display_fragment(trimmed) {
        return None;
    }
    let truncated = truncate_text(trimmed, max_chars);
    if truncated.is_empty() || is_dirty_first_party_display_fragment(&truncated) {
        None
    } else {
        Some(truncated)
    }
}

fn normalize_display_metric_spacing(input: &str) -> String {
    let mut out = input.to_string();
    for acronym in [
        "ROI", "ROAS", "ARPU", "ARPPU", "AIPU", "eCPM", "CPM", "CPC", "CPA", "CPI", "CTR", "CVR",
        "DAU", "MAU", "LTV", "CAC", "MRR", "ARR", "GMV", "NPS",
    ] {
        out = insert_boundary_before_token(&out, acronym);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn insert_boundary_before_token(input: &str, token: &str) -> String {
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < input.len() {
        let rest = &input[idx..];
        if rest.starts_with(token) && !out.is_empty() {
            let prev = out.chars().next_back().unwrap_or(' ');
            let is_embedded_ecpm = token == "CPM" && prev == 'e';
            if prev.is_ascii_alphanumeric() && !prev.is_whitespace() && !is_embedded_ecpm {
                out.push(' ');
            }
        }
        let ch = rest.chars().next().unwrap_or_default();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn is_dirty_first_party_display_fragment(input: &str) -> bool {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return true;
    }
    let lower = compact.to_ascii_lowercase();
    if lower.contains("+ more")
        || lower.contains(" more.")
        || compact.contains("+1 more")
        || compact.contains("+2 more")
        || compact.contains("+3 more")
        || compact.contains("+4 more")
        || compact.contains("+5 more")
        || compact.contains("...")
        || compact.contains('…')
    {
        return true;
    }
    if lower.contains("detected first-party evidence")
        || lower.contains("pm.deep_loop")
        || lower.contains("runtime execution failed")
        || lower.contains("durationms")
        || lower.contains("toolcallcount")
    {
        return true;
    }
    if looks_like_repeated_uppercase_fragment(&compact) {
        return true;
    }
    if contains_repeated_uppercase_run(&compact) {
        return true;
    }
    let digit_text_transitions = count_digit_text_transitions(&compact);
    if compact.chars().count() > 24 && !compact.contains(' ') && digit_text_transitions >= 4 {
        return true;
    }
    if digit_text_transitions >= 7 {
        return true;
    }
    if compact.chars().count() > 110 && looks_like_dense_metric_table_fragment(&compact) {
        return true;
    }
    false
}

fn contains_repeated_uppercase_run(input: &str) -> bool {
    let mut run = String::new();
    for ch in input.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_uppercase() {
            run.push(ch);
            continue;
        }
        if looks_like_repeated_uppercase_fragment(&run) {
            return true;
        }
        run.clear();
    }
    false
}

fn looks_like_repeated_uppercase_fragment(input: &str) -> bool {
    let compact = input
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .collect::<String>();
    let len = compact.chars().count();
    if !(4..=12).contains(&len)
        || len % 2 != 0
        || !compact.chars().all(|ch| ch.is_ascii_uppercase())
    {
        return false;
    }
    let half = len / 2;
    let left = compact.chars().take(half).collect::<String>();
    let right = compact.chars().skip(half).collect::<String>();
    left == right
}

fn looks_like_dense_metric_table_fragment(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    let table_markers = [
        "日均",
        "占比",
        "分层",
        "结论",
        "指标",
        "成本",
        "收入",
        "用户类型",
        "策略价值",
        "current",
        "metric",
        "segment",
        "revenue",
        "cost",
        "table",
    ];
    let marker_hits = table_markers
        .iter()
        .filter(|marker| input.contains(**marker) || lower.contains(**marker))
        .count();
    let digits = input.chars().filter(|ch| ch.is_ascii_digit()).count();
    let separators = input
        .chars()
        .filter(|ch| {
            matches!(
                ch,
                ',' | '，' | ';' | '；' | ':' | '：' | '/' | '+' | '%' | '$' | '<' | '>' | '='
            )
        })
        .count();
    let glued_transitions = count_digit_text_transitions(input);
    marker_hits >= 5 || (digits >= 10 && separators >= 4) || glued_transitions >= 7
}

fn count_digit_text_transitions(input: &str) -> usize {
    let mut prev: Option<char> = None;
    let mut transitions = 0usize;
    for ch in input.chars() {
        if let Some(last) = prev {
            let last_text = last.is_ascii_alphabetic() || ('\u{4e00}'..='\u{9fff}').contains(&last);
            let ch_text = ch.is_ascii_alphabetic() || ('\u{4e00}'..='\u{9fff}').contains(&ch);
            if (last.is_ascii_digit() && ch_text) || (last_text && ch.is_ascii_digit()) {
                transitions = transitions.saturating_add(1);
            }
        }
        prev = Some(ch);
    }
    transitions
}

fn build_priority_strategies_cn(ctx: &FirstPartyStrategyContext) -> Vec<String> {
    let mut out = Vec::new();
    if !ctx.cohorts.is_empty() {
        out.push(format!(
            "优先围绕一手报告中机会最高的人群做差异化策略：{}。每个人群都要有独立触发规则、资源强度、体验节奏和保护指标。",
            join_limited(&ctx.cohorts, 3, "；")
        ));
    } else {
        out.push("先把报告中的用户/场景拆成高机会、待验证、需止损三类，再分别配置触发规则、资源强度和退出阈值。".to_string());
    }
    if !ctx.failed_experiments.is_empty() || !ctx.anti_patterns.is_empty() {
        let lessons = ctx
            .failed_experiments
            .iter()
            .chain(ctx.anti_patterns.iter())
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        out.push(format!(
            "把历史失败和反模式转成硬约束：{}。新策略必须说明为什么不会重复这些问题。",
            join_limited(&lessons, 3, "；")
        ));
    } else {
        out.push(
            "把所有增长动作拆成收益路径、成本路径、体验路径和风险路径，避免只优化单一指标。"
                .to_string(),
        );
    }
    out.push("用小流量实验先验证最高杠杆假设，再把有效规则沉淀成可配置策略，而不是一次性上线大而全方案。".to_string());
    out
}

fn build_priority_strategies_en(ctx: &FirstPartyStrategyContext) -> Vec<String> {
    let mut out = Vec::new();
    if !ctx.cohorts.is_empty() {
        out.push(format!(
            "Prioritize differentiated strategies for the highest-opportunity cohorts in the report: {}. Each cohort needs its own trigger rules, resource intensity, experience rhythm, and guardrails.",
            join_limited(&ctx.cohorts, 3, "; ")
        ));
    } else {
        out.push("Split the reported users/scenarios into high-opportunity, needs-validation, and loss-control groups, then assign separate triggers, resource intensity, and exit thresholds.".to_string());
    }
    if !ctx.failed_experiments.is_empty() || !ctx.anti_patterns.is_empty() {
        let lessons = ctx
            .failed_experiments
            .iter()
            .chain(ctx.anti_patterns.iter())
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        out.push(format!(
            "Turn prior failures and anti-patterns into hard constraints: {}. Every new strategy must explain why it does not repeat those failure modes.",
            join_limited(&lessons, 3, "; ")
        ));
    } else {
        out.push("Decompose each growth move into revenue, cost, experience, and risk paths so the plan does not optimize one metric in isolation.".to_string());
    }
    out.push("Validate the highest-leverage hypothesis with a small controlled rollout before turning it into a configurable operating rule.".to_string());
    out
}

fn build_segment_playbooks_cn(ctx: &FirstPartyStrategyContext) -> Vec<String> {
    if ctx.cohorts.is_empty() {
        return vec![
            "高机会人群：增加最小打扰的触达/转化/价值交换机会，并设置单用户资源上限。".to_string(),
            "待验证人群：只做低成本实验，先验证行为弹性和收入/留存影响。".to_string(),
            "需止损人群：收敛高成本动作，保留基础体验和回流路径。".to_string(),
        ];
    }
    ctx.cohorts
        .iter()
        .take(5)
        .map(|cohort| {
            format!(
                "{cohort}：单独设计策略假设、触发条件、资源强度/上限、保护指标和回滚阈值；不要与其他人群共用同一套规则。"
            )
        })
        .collect()
}

fn build_segment_playbooks_en(ctx: &FirstPartyStrategyContext) -> Vec<String> {
    if ctx.cohorts.is_empty() {
        return vec![
            "High-opportunity group: add low-interruption conversion/value-exchange opportunities with per-user resource caps.".to_string(),
            "Needs-validation group: run low-cost tests first to measure behavioral elasticity and revenue/retention impact.".to_string(),
            "Loss-control group: constrain expensive actions while preserving baseline UX and reactivation paths.".to_string(),
        ];
    }
    ctx.cohorts
        .iter()
        .take(5)
        .map(|cohort| {
            format!(
                "{cohort}: define a distinct hypothesis, trigger condition, resource intensity/cap, guardrail, and rollback threshold; do not reuse one global rule."
            )
        })
        .collect()
}

fn build_strategy_package(
    input: &PmDeepResearchLoopInput<'_>,
    scores: &PmDeepResearchScore,
    evidence_score: &PmEvidenceScore,
) -> PmStrategyPackageArtifact {
    let cjk = contains_cjk(input.question) || contains_cjk(input.answer);
    let ctx = FirstPartyStrategyContext::from_plan(input.plan);
    let first_party = extract_first_party_summary(input.plan);
    let evidence_note = if input.admitted_external_evidence && input.citation_count > 0 {
        if cjk {
            format!(
                "外部证据已作为补强使用：{} 个引用、{} 个域名；一手报告仍是主证据。",
                input.citation_count, input.domain_count
            )
        } else {
            format!(
                "External evidence supplemented the analysis: {} citations across {} domains; first-party evidence remains primary.",
                input.citation_count, input.domain_count
            )
        }
    } else if input.external_search_available && input.rejected_external_evidence_count > 0 {
        if cjk {
            format!(
                "{} 条候选参考资料未进入引用区；本答案以一手报告和专家推演为主。",
                input.rejected_external_evidence_count
            )
        } else {
            format!(
                "{} reference candidate(s) stayed outside the citation set; this answer relies on first-party context plus expert reasoning.",
                input.rejected_external_evidence_count
            )
        }
    } else if cjk {
        "本答案以你提供的一手资料和专家推演为主；未进入引用区的外部资料不作为依据，扩量前需要小流量验证。".to_string()
    } else {
        "This package is grounded in first-party context plus expert reasoning; external material that did not enter the citation set is not treated as evidence, so validate with a controlled rollout.".to_string()
    };
    let confidence = if scores.decision_readiness_score >= 0.82 {
        "high"
    } else if scores.decision_readiness_score >= 0.62 {
        "medium"
    } else {
        "low"
    }
    .to_string();
    let package = if cjk {
        let mut evidence = vec![
            evidence_note,
            ctx.metric_sentence_cn(),
            ctx.objective_sentence_cn(),
        ];
        if !first_party.is_empty() {
            evidence.push(first_party);
        }
        let mut tracking_plan = vec![
            "按策略、人群/场景、触发原因、资源强度、对照组、核心指标和保护指标记录全链路日志。"
                .to_string(),
            "每天看主指标、收入质量、成本结构、留存/体验、风险事件；每个分层都单独出实验看板。"
                .to_string(),
        ];
        if !ctx.metrics.is_empty() {
            tracking_plan.push(format!(
                "指标看板必须至少包含：{}。",
                join_limited(&ctx.metrics, 8, "、")
            ));
        }
        let mut rollout_plan = vec![
            "先补齐配置、埋点和 holdout，再上线策略；避免同时发布多个不可归因变化。".to_string(),
            "灰度 5% -> 20% -> 50%；每一档只在保护指标稳定后进入下一档。".to_string(),
        ];
        if !ctx.existing_mechanics.is_empty() {
            rollout_plan.push(format!(
                "优先改造已有能力：{}；除非实验证明必要，不新增突兀入口。",
                join_limited(&ctx.existing_mechanics, 4, "、")
            ));
        }
        PmStrategyPackageArtifact {
            executive_conclusion: format!(
                "优先把报告中的一手事实转成分人群/分场景策略包，先验证最高杠杆假设，再扩量。{} {}",
                ctx.focus_cn(),
                ctx.objective_sentence_cn()
            ),
            priority_strategies: build_priority_strategies_cn(&ctx),
            segment_playbooks: build_segment_playbooks_cn(&ctx),
            experiment_plan: vec![
                "A/B/n：保留现状对照，新增 1 个激进变体和 1 个保守变体；按报告中的人群/场景分层随机。".to_string(),
                "每个实验写清触发规则、资源强度/上限、目标指标、保护指标、样本口径和观察窗口。".to_string(),
            ],
            expected_impact_model: vec![
                "收益路径：最高机会人群的转化/活跃/收入质量提升，带动主指标改善。".to_string(),
                "成本路径：低贡献或高风险场景减少无效资源消耗，但不以牺牲保护指标换短期主指标。".to_string(),
                "体验路径：通过分层资源控制、清晰触发和可感知价值降低打扰感。".to_string(),
            ],
            guardrails: ctx.guardrail_items_cn(),
            kill_criteria: vec![
                "任一保护指标连续 2 个观察窗口显著差于对照组，停止扩量并回滚该分层策略。".to_string(),
                "主指标提升但收益路径无法解释，或成本/体验/风险指标恶化，判定为不可扩量。".to_string(),
            ],
            rollout_plan,
            tracking_plan,
            counterfactuals: vec![
                "如果主指标提升主要来自短期成本压缩，而不是收入质量、留存或转化改善，不应继续扩量。".to_string(),
                "如果最高机会人群没有显著响应，优先怀疑触发位置、资源感知或人群定义，而不是继续加大强度。".to_string(),
            ],
            evidence,
            confidence,
            open_questions: vec![
                format!(
                    "需要下一轮用真实实验数据确认：分层规则对收入、成本、留存、体验的净影响。证据可用性={:.2}，一手对齐={:.2}，冲突={}.",
                    scores.evidence_coverage_score,
                    evidence_score.first_party_alignment,
                    evidence_score.conflict_level
                ),
            ],
        }
    } else {
        let mut evidence = vec![
            evidence_note,
            ctx.metric_sentence_en(),
            ctx.objective_sentence_en(),
        ];
        if !first_party.is_empty() {
            evidence.push(first_party);
        }
        let mut tracking_plan = vec![
            "Log strategy, cohort/scenario, trigger reason, resource intensity, holdout state, primary metrics, and guardrails end to end.".to_string(),
            "Review primary metric, revenue quality, cost structure, retention/experience, and risk events daily; keep one dashboard per segment.".to_string(),
        ];
        if !ctx.metrics.is_empty() {
            tracking_plan.push(format!(
                "The dashboard should include at least: {}.",
                join_limited(&ctx.metrics, 8, ", ")
            ));
        }
        let mut rollout_plan = vec![
            "Ship configuration, instrumentation, and holdouts first; avoid releasing multiple un-attributable changes at once.".to_string(),
            "Roll out 5% -> 20% -> 50%; only advance when guardrails remain stable.".to_string(),
        ];
        if !ctx.existing_mechanics.is_empty() {
            rollout_plan.push(format!(
                "Prefer upgrading existing mechanics: {}; avoid new disruptive entry points unless the experiment proves they are necessary.",
                join_limited(&ctx.existing_mechanics, 4, ", ")
            ));
        }
        PmStrategyPackageArtifact {
            executive_conclusion: format!(
                "Turn the report's first-party facts into a segmented strategy package, validate the highest-leverage hypothesis first, then scale. {} {}",
                ctx.focus_en(),
                ctx.objective_sentence_en()
            ),
            priority_strategies: build_priority_strategies_en(&ctx),
            segment_playbooks: build_segment_playbooks_en(&ctx),
            experiment_plan: vec![
                "Run A/B/n: keep the current control, add one aggressive variant and one conservative variant; randomize within the report's cohorts/scenarios.".to_string(),
                "Specify trigger rules, resource intensity, caps, target metric, guardrails, sample definition, and observation window for each experiment.".to_string(),
            ],
            expected_impact_model: vec![
                "Revenue path: the highest-opportunity cohorts improve conversion, activity, or revenue quality, lifting the primary metric.".to_string(),
                "Cost path: low-contribution or high-risk scenarios consume fewer ineffective resources without sacrificing guardrails.".to_string(),
                "Experience path: segmented caps, clear triggers, and visible value reduce user friction.".to_string(),
            ],
            guardrails: ctx.guardrail_items_en(),
            kill_criteria: vec![
                "Stop expansion if any guardrail is significantly worse than control for two consecutive observation windows.".to_string(),
                "Do not scale if the primary metric improves but the causal path is unexplained or cost, experience, or risk metrics deteriorate.".to_string(),
            ],
            rollout_plan,
            tracking_plan,
            counterfactuals: vec![
                "If the primary metric lift is mostly short-term cost compression rather than better revenue quality, retention, or conversion, do not scale.".to_string(),
                "If the highest-opportunity cohort does not respond, question the trigger position, perceived value, or cohort definition before increasing intensity.".to_string(),
            ],
            evidence,
            confidence,
            open_questions: vec![format!(
                "Validate with real experiment data. evidenceCoverage={:.2}, firstPartyAlignment={:.2}, conflict={}.",
                scores.evidence_coverage_score,
                evidence_score.first_party_alignment,
                evidence_score.conflict_level
            )],
        }
    };
    sanitize_strategy_package_artifact(package, cjk)
}

fn sanitize_strategy_package_artifact(
    mut package: PmStrategyPackageArtifact,
    cjk: bool,
) -> PmStrategyPackageArtifact {
    package.executive_conclusion = sanitize_strategy_sentence(
        &package.executive_conclusion,
        if cjk {
            "优先基于一手事实做分人群/分场景策略，先验证最高杠杆假设，再逐步扩量。"
        } else {
            "Prioritize first-party facts, build segmented strategies, validate the highest-leverage hypothesis first, then scale."
        },
    );
    package.priority_strategies = sanitize_strategy_list(
        package.priority_strategies,
        if cjk {
            &[
                "优先处理一手报告中收益弹性最高、风险可控的人群或场景。",
                "把历史失败经验转成硬约束，避免为了单一指标牺牲保护指标。",
                "先用小流量实验验证因果路径，再沉淀成可配置策略。",
            ]
        } else {
            &[
                "Prioritize the first-party cohorts or scenarios with the highest upside and controllable risk.",
                "Turn prior failures into hard constraints so one metric is not improved by damaging guardrails.",
                "Validate the causal path in a small rollout before turning it into configurable policy.",
            ]
        },
    );
    package.segment_playbooks = sanitize_strategy_list(
        package.segment_playbooks,
        if cjk {
            &[
                "高机会人群：提高有效触达或价值交换机会，并设置资源上限和保护指标。",
                "待验证人群：只做低成本实验，先确认行为弹性和收益路径。",
                "需止损人群：收敛高成本动作，保留基础体验和回流路径。",
            ]
        } else {
            &[
                "High-opportunity segment: increase effective conversion or value-exchange opportunities with caps and guardrails.",
                "Needs-validation segment: run low-cost tests first to confirm behavioral elasticity and the value path.",
                "Loss-control segment: reduce expensive actions while preserving baseline UX and recovery paths.",
            ]
        },
    );
    package.experiment_plan = sanitize_strategy_list(package.experiment_plan, &[]);
    package.expected_impact_model = sanitize_strategy_list(package.expected_impact_model, &[]);
    package.guardrails = sanitize_strategy_list(
        package.guardrails,
        if cjk {
            &["保护一手报告中的核心健康指标；实验组不得显著差于对照组。"]
        } else {
            &["Protect the report's core health metrics; treatment must not be significantly worse than control."]
        },
    );
    package.kill_criteria = sanitize_strategy_list(package.kill_criteria, &[]);
    package.rollout_plan = sanitize_strategy_list(package.rollout_plan, &[]);
    package.tracking_plan = sanitize_strategy_list(package.tracking_plan, &[]);
    package.counterfactuals = sanitize_strategy_list(package.counterfactuals, &[]);
    package.evidence = sanitize_strategy_list(
        package.evidence,
        if cjk {
            &["依据以用户提供的一手报告为主；扩量前必须用小流量实验验证。"]
        } else {
            &["Basis is primarily grounded in the user's first-party report; validate with a controlled rollout before scaling."]
        },
    );
    package.open_questions = sanitize_strategy_list(package.open_questions, &[]);
    package
}

fn sanitize_strategy_list(items: Vec<String>, fallback: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        let clean = sanitize_strategy_sentence(&item, "");
        if clean.is_empty() {
            continue;
        }
        push_unique(&mut out, clean);
    }
    if out.is_empty() {
        out.extend(fallback.iter().map(|item| (*item).to_string()));
    }
    out
}

fn sanitize_strategy_sentence(raw: &str, fallback: &str) -> String {
    let clean = normalize_display_metric_spacing(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() || is_dirty_first_party_display_fragment(&clean) {
        fallback.to_string()
    } else {
        clean
    }
}

fn render_strategy_package_cn(package: &PmStrategyPackageArtifact) -> String {
    render_strategy_package(
        package,
        &[
            ("结论", &package.executive_conclusion),
            ("优先策略", &package.priority_strategies.join("\n")),
            ("分人群打法", &package.segment_playbooks.join("\n")),
            ("实验方案", &package.experiment_plan.join("\n")),
            ("预期影响假设", &package.expected_impact_model.join("\n")),
            ("保护指标", &package.guardrails.join("\n")),
            ("Kill Criteria", &package.kill_criteria.join("\n")),
            ("灰度节奏", &package.rollout_plan.join("\n")),
            ("埋点验证", &package.tracking_plan.join("\n")),
            ("反事实检查", &package.counterfactuals.join("\n")),
            ("证据状态", &package.evidence.join("\n")),
            ("待补问题", &package.open_questions.join("\n")),
        ],
    )
}

fn render_strategy_package_en(package: &PmStrategyPackageArtifact) -> String {
    render_strategy_package(
        package,
        &[
            ("Conclusion", &package.executive_conclusion),
            (
                "Priority Strategies",
                &package.priority_strategies.join("\n"),
            ),
            ("Segment Playbooks", &package.segment_playbooks.join("\n")),
            ("Experiment Plan", &package.experiment_plan.join("\n")),
            ("Expected Impact", &package.expected_impact_model.join("\n")),
            ("Guardrails", &package.guardrails.join("\n")),
            ("Kill Criteria", &package.kill_criteria.join("\n")),
            ("Rollout", &package.rollout_plan.join("\n")),
            ("Tracking", &package.tracking_plan.join("\n")),
            ("Counterfactuals", &package.counterfactuals.join("\n")),
            ("Evidence Status", &package.evidence.join("\n")),
            ("Open Questions", &package.open_questions.join("\n")),
        ],
    )
}

fn render_strategy_package(
    package: &PmStrategyPackageArtifact,
    sections: &[(&str, &String)],
) -> String {
    let mut out = Vec::new();
    for (title, body) in sections {
        if body.trim().is_empty() {
            continue;
        }
        out.push(format!("## {title}"));
        if matches!(
            *title,
            "结论" | "Conclusion" | "证据状态" | "Evidence Status" | "待补问题" | "Open Questions"
        ) {
            out.push(body.trim().to_string());
        } else if body.contains('\n') {
            let lines = body
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    if line.trim_start().starts_with('-') {
                        line.to_string()
                    } else {
                        format!("- {}", line.trim())
                    }
                })
                .collect::<Vec<_>>();
            out.push(lines.join("\n"));
        } else {
            out.push(body.trim().to_string());
        }
    }
    out.push(format!("Confidence: {}", package.confidence));
    out.join("\n\n")
}

fn extract_next_queries(plan: &Value, missing: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = plan.get("queryVariants").and_then(Value::as_array) {
        for item in items.iter().filter_map(Value::as_str).take(4) {
            push_unique(&mut out, item);
        }
    }
    if out.is_empty() {
        for miss in missing.iter().take(4) {
            push_unique(&mut out, miss.replace('_', " "));
        }
    }
    out
}

fn extract_first_party_summary(plan: &Value) -> String {
    let Some(first_party) = plan
        .get("reportStrategy")
        .and_then(|value| value.get("firstPartyEvidenceJson"))
    else {
        return String::new();
    };
    let metric_count = first_party
        .get("metrics")
        .and_then(Value::as_array)
        .map(|v| v.len())
        .unwrap_or(0);
    let cohort_count = first_party
        .get("opportunityCohorts")
        .and_then(Value::as_array)
        .map(|v| v.len())
        .unwrap_or(0);
    if metric_count == 0 && cohort_count == 0 {
        return String::new();
    }
    String::new()
}

fn score_first_party_alignment(plan: &Value, question: &str, answer: &str) -> f64 {
    if !has_first_party_evidence(plan) {
        return if answer.trim().is_empty() { 0.0 } else { 0.45 };
    }
    let mut tokens = metric_tokens(question);
    if let Some(items) = plan
        .get("reportStrategy")
        .and_then(|v| v.get("primaryTerms"))
        .and_then(Value::as_array)
    {
        for item in items.iter().filter_map(Value::as_str).take(12) {
            push_unique(&mut tokens, item);
        }
    }
    if tokens.is_empty() {
        return 0.82;
    }
    let answer_lower = answer.to_ascii_lowercase();
    let hits = tokens
        .iter()
        .filter(|token| {
            let lower = token.to_ascii_lowercase();
            answer_lower.contains(&lower) || answer.contains(token.as_str())
        })
        .count();
    (hits as f64 / tokens.len().max(1) as f64)
        .max(if answer.chars().count() > 600 {
            0.58
        } else {
            0.0
        })
        .min(1.0)
}

fn score_actionability(answer: &str) -> f64 {
    let lower = answer.to_ascii_lowercase();
    let checks = [
        contains_any(&lower, &["priority", "优先", "先做"]),
        contains_any(&lower, &["segment", "cohort", "分层", "人群"]),
        contains_any(&lower, &["experiment", "a/b", "灰度", "实验", "对照"]),
        contains_any(
            &lower,
            &["guardrail", "保护指标", "health metric", "留存", "成本"],
        ),
        contains_any(&lower, &["kill", "stop", "停止", "回滚", "止损"]),
        contains_any(&lower, &["tracking", "metric", "埋点", "指标"]),
        contains_any(&lower, &["impact", "收益", "成本", "预期"]),
    ];
    let hits = checks.iter().filter(|hit| **hit).count();
    (hits as f64 / checks.len() as f64).min(1.0)
}

fn has_strategy_package_sections(answer: &str) -> bool {
    let lower = answer.to_ascii_lowercase();
    contains_any(&lower, &["kill criteria", "guardrails", "experiment plan"])
        || (answer.contains("保护指标") && answer.contains("实验") && answer.contains("灰度"))
}

fn has_first_party_evidence(plan: &Value) -> bool {
    plan.get("reportStrategy")
        .and_then(|value| value.get("firstPartyEvidenceJson"))
        .is_some_and(|value| {
            !value.is_null() && value.as_object().map(|obj| !obj.is_empty()).unwrap_or(true)
        })
}

fn metric_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
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
            push_unique(&mut out, token);
        }
    }
    for token in ["收入", "成本", "留存", "次留", "时长", "转化", "价值"] {
        if text.contains(token) {
            push_unique(&mut out, token);
        }
    }
    out
}

fn contains_cjk(text: &str) -> bool {
    text.chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
}

fn contains_any(text: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|token| text.contains(token))
}

fn push_unique(out: &mut Vec<String>, raw: impl AsRef<str>) {
    let value = raw.as_ref().trim();
    if value.is_empty() {
        return;
    }
    if !out.iter().any(|item| item == value) {
        out.push(value.to_string());
    }
}

fn clamp01(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_plan() -> Value {
        json!({
            "mode": "business_report_strategy",
            "queryVariants": ["product operations strategy segmentation benchmark", "experiment design guardrails kill criteria"],
            "reportStrategy": {
                "primaryTerms": ["ROI", "AIPU", "eCPM", "ROAS"],
                "firstPartyEvidenceJson": {
                    "metrics": ["ROI 1.235", "AIPU 17.11", "eCPM 3.16"],
                    "opportunityCohorts": [{"cohort": "eCPM 5+ + AIPU 1~4"}]
                }
            }
        })
    }

    #[test]
    fn enables_business_report_strategy_loop() {
        assert!(PmDeepResearchLoop::should_enable(
            &report_plan(),
            "给我策略"
        ));
        assert!(!PmDeepResearchLoop::should_enable(
            &json!({"mode": "chat"}),
            "hi"
        ));
        assert!(!PmDeepResearchLoop::should_enable(
            &json!({
                "mode": "direct_answer",
                "taskGraph": {"decompositionMode": "none", "complexityScore": 0.1}
            }),
            "翻译成中文"
        ));
        assert!(!PmDeepResearchLoop::should_enable(
            &json!({
                "mode": "auto",
                "taskGraph": {"decompositionMode": "none", "complexityScore": 20}
            }),
            "查一下北京天气"
        ));
        assert!(!PmDeepResearchLoop::should_enable(
            &json!({
                "mode": "auto",
                "taskGraph": {"decompositionMode": "none", "complexityScore": 20}
            }),
            "给我一个产品策略方案"
        ));
        assert!(PmDeepResearchLoop::should_enable(
            &json!({
                "mode": "auto",
                "taskGraph": {"decompositionMode": "full", "complexityScore": 60}
            }),
            "给我一个产品策略方案"
        ));
    }

    #[test]
    fn diagnostic_noise_blocks_finalize() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "durationMs: 1391",
            quality_passed: false,
            deliverable: false,
            citation_count: 0,
            domain_count: 0,
            claim_count: 1,
            triad_coverage: 0.0,
            conflict_confidence: 0.0,
            missing: &["tool_diagnostic_leaked_into_answer".to_string()],
            suggestions: &[],
            attempt: 1,
            max_attempts: 4,
            elapsed_secs: 10,
            max_wall_secs: 420,
            no_new_evidence_repeats: 0,
            no_new_evidence_limit: 2,
            external_search_available: false,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert_eq!(output.decision.action, PmDeepResearchAction::Rewrite);
        assert!(output.strategy_package.is_none());
    }

    #[test]
    fn no_external_search_requests_llm_rewrite_without_strategy_package() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "中文问题：基于报告做策略",
            answer: "分层策略：ROI AIPU eCPM ROAS 实验 保护指标 kill 灰度 埋点 成本 收入 留存",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 5,
            triad_coverage: 0.2,
            conflict_confidence: 0.0,
            missing: &[],
            suggestions: &[],
            attempt: 4,
            max_attempts: 4,
            elapsed_secs: 421,
            max_wall_secs: 420,
            no_new_evidence_repeats: 2,
            no_new_evidence_limit: 2,
            external_search_available: false,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert_eq!(output.decision.action, PmDeepResearchAction::Rewrite);
        assert!(output.strategy_package.is_none());
        assert!(output.degraded);
    }

    #[test]
    fn expert_review_is_non_blocking_and_preserves_evidence() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "结论：ROI 需要提升。建议做分层实验。",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.1,
            conflict_confidence: 0.0,
            missing: &["missing_guardrails".to_string()],
            suggestions: &["补充 kill criteria".to_string()],
            attempt: 1,
            max_attempts: 4,
            elapsed_secs: 20,
            max_wall_secs: 420,
            no_new_evidence_repeats: 0,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert!(output.expert_review_score.non_blocking);
        assert!(output.expert_review_score.should_continue_research);
        assert_eq!(
            output.decision.action,
            PmDeepResearchAction::ContinueResearch
        );
        assert!(output
            .decision
            .weak_claims
            .iter()
            .any(|item| item.contains("kill") || item.contains("guardrail")));
        assert!(output
            .decision
            .next_queries
            .iter()
            .any(|item| item.contains("experiment") || item.contains("missing guardrails")));
    }

    #[test]
    fn rejected_external_evidence_continues_research_then_expert_only_at_limit() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "结论：ROI 需要提升。建议做分层实验。",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.1,
            conflict_confidence: 0.0,
            missing: &["external_evidence_not_admitted".to_string()],
            suggestions: &[],
            attempt: 1,
            max_attempts: 4,
            elapsed_secs: 20,
            max_wall_secs: 420,
            no_new_evidence_repeats: 0,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 3,
        });
        assert_eq!(
            output.decision.action,
            PmDeepResearchAction::ContinueResearch
        );
        assert!(output
            .decision
            .next_queries
            .iter()
            .any(|query| query.contains("retry") || query.contains("external evidence")));

        let limited = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "结论：ROI 需要提升。建议做分层实验。",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.1,
            conflict_confidence: 0.0,
            missing: &["external_evidence_not_admitted".to_string()],
            suggestions: &[],
            attempt: 4,
            max_attempts: 4,
            elapsed_secs: 420,
            max_wall_secs: 420,
            no_new_evidence_repeats: 2,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 3,
        });
        assert_eq!(limited.decision.action, PmDeepResearchAction::Rewrite);
        assert!(limited.strategy_package.is_none());
    }

    #[test]
    fn weak_answer_builds_research_branches_and_hypothesis_graph() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "结论：ROI 需要提升。建议做分层实验。",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.1,
            conflict_confidence: 0.0,
            missing: &["missing_guardrails".to_string()],
            suggestions: &[],
            attempt: 1,
            max_attempts: 4,
            elapsed_secs: 20,
            max_wall_secs: 420,
            no_new_evidence_repeats: 0,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert!(!output.research_branch_queue.branches.is_empty());
        assert!(output
            .research_branch_queue
            .branches
            .iter()
            .any(|branch| branch.lens == "first_party_alignment"
                || branch.lens == "decision_package"));
        assert!(output
            .hypothesis_evidence_graph
            .primary_evidence_node_ids
            .contains(&"first_party_report".to_string()));
        assert!(output
            .hypothesis_evidence_graph
            .nodes
            .iter()
            .any(|node| node.id == "core_strategy_hypothesis"));
        assert!(output
            .golden_eval_hints
            .hints
            .iter()
            .any(|hint| !hint.satisfied && hint.key == "guardrails_kill_criteria"));
        let selected = output
            .research_branch_queue
            .select_next_external_branch()
            .expect("selected external branch");
        assert!(selected.requires_external_search);
        assert_ne!(selected.lens, "first_party_alignment");
        assert_ne!(selected.lens, "decision_package");
        assert!(!selected.queries.is_empty());
        assert!(!selected
            .queries
            .iter()
            .any(|query| query.contains("validate first-party signal")));
    }

    #[test]
    fn expert_review_does_not_force_degraded_strategy_package_at_budget_limit() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &report_plan(),
            question: "基于 ROI/AIPU/eCPM 给策略",
            answer: "结论：ROI 需要提升。建议做分层实验。",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.1,
            conflict_confidence: 0.0,
            missing: &["missing_guardrails".to_string()],
            suggestions: &["补充 kill criteria".to_string()],
            attempt: 4,
            max_attempts: 4,
            elapsed_secs: 420,
            max_wall_secs: 420,
            no_new_evidence_repeats: 2,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert!(output.expert_review_score.non_blocking);
        assert_eq!(output.decision.action, PmDeepResearchAction::Rewrite);
        assert!(output.strategy_package.is_none());
    }

    #[test]
    fn fallback_strategy_package_uses_first_party_context_without_game_template() {
        let plan = json!({
            "mode": "business_report_strategy",
            "reportStrategy": {
                "primaryTerms": ["MRR", "churn", "activation"],
                "firstPartyEvidenceJson": {
                    "contextTerms": ["B2B SaaS self-serve onboarding"],
                    "objectives": ["提升 activation", "降低 churn"],
                    "metrics": [
                        {"name": "MRR", "value": "$120k"},
                        {"name": "activation", "value": "31%"},
                        {"name": "churn", "value": "7.2%"}
                    ],
                    "guardrails": ["support tickets 不上升"],
                    "opportunityCohorts": [
                        {"cohort": "trial users with 3+ teammates invited", "why": "high intent but low activation"}
                    ],
                    "existingMechanics": ["email onboarding", "in-app checklist"],
                    "failedExperiments": [
                        {"name": "mandatory demo wall", "lesson": "activation fell for self-serve users"}
                    ],
                    "antiPatterns": ["不要强制预约 demo"],
                    "rawEvidenceSnippets": ["trial users invite teammates but miss workspace setup"]
                }
            }
        });
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &plan,
            question: "Based on this SaaS onboarding report, produce a strategy package.",
            answer: "runtime execution failed",
            quality_passed: false,
            deliverable: false,
            citation_count: 0,
            domain_count: 0,
            claim_count: 1,
            triad_coverage: 0.0,
            conflict_confidence: 0.0,
            missing: &["timeout".to_string()],
            suggestions: &[],
            attempt: 6,
            max_attempts: 6,
            elapsed_secs: 420,
            max_wall_secs: 420,
            no_new_evidence_repeats: 2,
            no_new_evidence_limit: 2,
            external_search_available: false,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert_eq!(output.decision.action, PmDeepResearchAction::Rewrite);
        assert!(output.strategy_package.is_none());
    }

    #[test]
    fn degraded_strategy_package_drops_dirty_table_and_truncation_fragments() {
        let plan = json!({
            "mode": "business_report_strategy",
            "reportStrategy": {
                "firstPartyEvidenceJson": {
                    "contextTerms": [
                        "成本只算 UA+UG",
                        "印尼网赚单机休闲矩阵产品",
                        "核心成本结构大概是： 成本项占比买量成本 UA约 70%~75%激励提现/UG成本约 25%~30% 当前核心目..."
                    ],
                    "objectives": [
                        "一、业务背景 我们是印尼网赚单机休闲矩阵产品，核心成本结构大概是： 成本项占比买量成本 UA约 70%~75%激励提现/UG成本约 25%~30% 当前核心目标不是单纯少发金币，而是： 目标要求ROI提升AIPU不能下降游戏时长不能下降次留...",
                        "提升 ROI",
                        "AIPU 不能下降"
                    ],
                    "metrics": [
                        {"name": "revenue", "value": "$1,369/dayUA"},
                        {"name": "cost", "value": "25%~30%"},
                        {"name": "ROI", "value": "1.235AIPU17.11eCPM3.16ARPU$0.054"}
                    ],
                    "guardrails": [
                        "而是： 目标要求ROI提升AIPU不能下降游戏时长不能下降次留不能下降ROAS1/3/7希望提升收入希望提升 之前试过 EWMA / hybrid 等 eCPM 算法",
                        "留存不下降"
                    ],
                    "opportunityCohorts": [
                        {"cohort": "三、按 eCPM 用户价值分层 eCPM分层日均UVUV占比日均收入收入占比日均UA+UG成本ROIAIPU", "why": "三、按 eCPM 用户价值分层 eCPM分层日均UVUV占比日均收入收入占比日均UA+UG成本ROIAIPU结论eCPM <18,46133.4%$916.7%$2380.38411.26明显亏损池eCPM 1~3.510,46141.3%$39428.8%$4220.93421..."},
                        {"cohort": "高价值低活跃人群", "why": "单价高但触达不足"}
                    ],
                    "existingMechanics": [
                        "和策略 当前已经存在这些能力： 模块当前情况eCPM分层服务端根据 eCPM 下发不同金币和广告位ID广告位ID当前大致有低/中/高三档悬浮宝箱已有"
                    ],
                    "failedExperiments": [
                        {"name": "hybridROI", "lesson": "结果是： 算法结果hybridROI 不如原加权平均，放弃EWMAROI 小幅上涨"},
                        {"name": "EWMA", "lesson": "伤害保护指标"}
                    ],
                    "antiPatterns": [
                        "UVUV",
                        "+2 more"
                    ],
                    "rawEvidenceSnippets": [
                        "一手片段... +2 more",
                        "Detected first-party evidence: 24 metric signals and 6 opportunity cohorts."
                    ]
                }
            }
        });
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &plan,
            question: "基于报告做产品运营策略",
            answer: "需要重写",
            quality_passed: false,
            deliverable: true,
            citation_count: 0,
            domain_count: 0,
            claim_count: 2,
            triad_coverage: 0.0,
            conflict_confidence: 0.0,
            missing: &["missing_citations".to_string()],
            suggestions: &[],
            attempt: 4,
            max_attempts: 4,
            elapsed_secs: 420,
            max_wall_secs: 420,
            no_new_evidence_repeats: 2,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: false,
            rejected_external_evidence_count: 0,
        });
        assert_eq!(output.decision.action, PmDeepResearchAction::Rewrite);
        assert!(output.strategy_package.is_none());
    }

    #[test]
    fn non_deep_finalize_does_not_emit_strategy_package() {
        let output = PmDeepResearchLoop::evaluate(PmDeepResearchLoopInput {
            plan: &json!({"mode": "chat"}),
            question: "翻译成中文",
            answer: "## 核心结论\nROI AIPU eCPM ROAS 实验 保护指标 kill 灰度 埋点 成本 收入 留存\n\n## 已证实\n- ROI 和 AIPU 需要同时保护。\n\n## 待验证\n- 小流量实验验证。",
            quality_passed: true,
            deliverable: true,
            citation_count: 3,
            domain_count: 2,
            claim_count: 6,
            triad_coverage: 0.8,
            conflict_confidence: 0.8,
            missing: &[],
            suggestions: &[],
            attempt: 2,
            max_attempts: 4,
            elapsed_secs: 80,
            max_wall_secs: 420,
            no_new_evidence_repeats: 0,
            no_new_evidence_limit: 2,
            external_search_available: true,
            admitted_external_evidence: true,
            rejected_external_evidence_count: 0,
        });
        assert_eq!(output.decision.action, PmDeepResearchAction::Finalize);
        assert!(output.strategy_package.is_none());
    }
}
