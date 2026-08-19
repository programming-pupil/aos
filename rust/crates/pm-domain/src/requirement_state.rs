//! Requirement Discovery Engine state.  A report is a view of this state;
//! it is never the source of truth for the next turn.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProblemFrame {
    pub statement: String,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stakeholder {
    pub name: String,
    pub role: Option<String>,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobToBeDone {
    pub statement: String,
    pub evidence_ids: Vec<String>,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pain {
    pub statement: String,
    pub severity: u8,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outcome {
    pub statement: String,
    pub measure: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementConstraint {
    pub statement: String,
    pub priority: String,
    pub source_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeDefinition {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AssumptionType {
    User,
    Product,
    Technical,
    Market,
    Data,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AssumptionStatus {
    Open,
    Supported,
    Falsified,
    AcceptedRisk,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assumption {
    pub statement: String,
    pub type_: AssumptionType,
    pub importance: f32,
    pub uncertainty: f32,
    pub status: AssumptionStatus,
    pub supporting_evidence: Vec<String>,
    pub counter_evidence: Vec<String>,
    pub falsification_test: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDecisionTarget {
    ProblemFrame,
    Stakeholder,
    OutcomeMetric,
    Population,
    #[default]
    Scope,
    Constraint,
    Solution,
    Deliverable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionAnswerBranch {
    pub id: String,
    pub answer: String,
    pub probability_basis_points: u16,
    pub posterior_uncertainty_basis_points: u16,
    pub decision_effect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenQuestion {
    pub id: String,
    pub question: String,
    pub impact: String,
    pub answerability: String,
    pub user_effort: u8,
    #[serde(default)]
    pub decision_target: QuestionDecisionTarget,
    #[serde(default)]
    pub prior_uncertainty_basis_points: u16,
    #[serde(default)]
    pub answer_branches: Vec<QuestionAnswerBranch>,
    #[serde(default)]
    pub expected_posterior_uncertainty_basis_points: u16,
    #[serde(default)]
    pub expected_information_gain_basis_points: u16,
}

impl OpenQuestion {
    #[must_use]
    pub fn with_recomputed_information_value(mut self) -> Self {
        let (posterior, gain) =
            expected_information_value(self.prior_uncertainty_basis_points, &self.answer_branches);
        self.expected_posterior_uncertainty_basis_points = posterior;
        self.expected_information_gain_basis_points = gain;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionResolution {
    pub question_id: String,
    pub selected_branch_id: Option<String>,
    pub observed_posterior_uncertainty_basis_points: u16,
    pub observed_convergence_basis_points: u16,
    pub decision_changed: bool,
    pub source_event_ids: Vec<String>,
    #[serde(default)]
    pub decision_target: QuestionDecisionTarget,
    #[serde(default)]
    pub predicted_information_gain_basis_points: u16,
    #[serde(default)]
    pub user_effort: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionCalibrationReport {
    pub sample_count: u32,
    pub mean_absolute_error_basis_points: u16,
    pub decision_change_rate_basis_points: u16,
    pub effort_adjusted_utility_basis_points: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub statement: String,
    pub testable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRef {
    pub id: String,
    pub statement: String,
    pub version: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimEvidenceLink {
    pub claim: String,
    pub evidence_ids: Vec<String>,
    pub support: EvidenceSupport,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvidenceSupport {
    Supported,
    Contradicted,
    Inconclusive,
    NotChecked,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationExperiment {
    pub id: String,
    pub hypothesis: String,
    pub success_signal: String,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequirementReadiness {
    Brief,
    NeedsClarification,
    ReadyForReview,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementPlanningGate {
    ContinueResearch,
    Ask(OpenQuestion),
    ReadyForDelivery,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequirementState {
    pub id: String,
    pub version: u64,
    pub problem_frame: Option<ProblemFrame>,
    pub stakeholders: Vec<Stakeholder>,
    pub jobs: Vec<JobToBeDone>,
    pub pains: Vec<Pain>,
    pub desired_outcomes: Vec<Outcome>,
    pub constraints: Vec<RequirementConstraint>,
    pub assumptions: Vec<Assumption>,
    pub scope: ScopeDefinition,
    pub decisions: Vec<DecisionRef>,
    pub open_questions: Vec<OpenQuestion>,
    #[serde(default)]
    pub question_resolutions: Vec<QuestionResolution>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub evidence_links: Vec<ClaimEvidenceLink>,
    pub experiments: Vec<ValidationExperiment>,
    pub readiness: RequirementReadiness,
}
impl Default for RequirementState {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            version: 0,
            problem_frame: None,
            stakeholders: vec![],
            jobs: vec![],
            pains: vec![],
            desired_outcomes: vec![],
            constraints: vec![],
            assumptions: vec![],
            scope: ScopeDefinition {
                included: vec![],
                excluded: vec![],
            },
            decisions: vec![],
            open_questions: vec![],
            question_resolutions: vec![],
            acceptance_criteria: vec![],
            evidence_links: vec![],
            experiments: vec![],
            readiness: RequirementReadiness::Brief,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequirementStateDelta {
    pub source_event_ids: Vec<String>,
    pub problem_frame: Option<Option<ProblemFrame>>,
    pub add_stakeholders: Vec<Stakeholder>,
    pub add_jobs: Vec<JobToBeDone>,
    pub add_pains: Vec<Pain>,
    pub add_outcomes: Vec<Outcome>,
    pub add_constraints: Vec<RequirementConstraint>,
    pub add_assumptions: Vec<Assumption>,
    pub scope: Option<ScopeDefinition>,
    pub add_decisions: Vec<DecisionRef>,
    pub add_questions: Vec<OpenQuestion>,
    pub resolve_question_ids: Vec<String>,
    #[serde(default)]
    pub add_question_resolutions: Vec<QuestionResolution>,
    pub add_acceptance_criteria: Vec<AcceptanceCriterion>,
    pub add_evidence_links: Vec<ClaimEvidenceLink>,
    pub add_experiments: Vec<ValidationExperiment>,
    pub readiness: Option<RequirementReadiness>,
}
impl Default for RequirementStateDelta {
    fn default() -> Self {
        Self {
            source_event_ids: vec![],
            problem_frame: None,
            add_stakeholders: vec![],
            add_jobs: vec![],
            add_pains: vec![],
            add_outcomes: vec![],
            add_constraints: vec![],
            add_assumptions: vec![],
            scope: None,
            add_decisions: vec![],
            add_questions: vec![],
            resolve_question_ids: vec![],
            add_question_resolutions: vec![],
            add_acceptance_criteria: vec![],
            add_evidence_links: vec![],
            add_experiments: vec![],
            readiness: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementDeltaError {
    DuplicateEvent(String),
    InvalidReadiness(String),
}
impl std::fmt::Display for RequirementDeltaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateEvent(id) => write!(f, "delta already applied: {id}"),
            Self::InvalidReadiness(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for RequirementDeltaError {}

pub fn apply_delta(
    state: &RequirementState,
    mut delta: RequirementStateDelta,
    applied_events: &[String],
) -> Result<RequirementState, RequirementDeltaError> {
    if let Some(id) = delta
        .source_event_ids
        .iter()
        .find(|id| applied_events.contains(id))
    {
        return Err(RequirementDeltaError::DuplicateEvent(id.clone()));
    }
    let mut next = state.clone();
    next.version = state.version.saturating_add(1);
    if let Some(frame) = delta.problem_frame {
        next.problem_frame = frame;
    }
    upsert_by(
        &mut next.stakeholders,
        delta.add_stakeholders,
        |left, right| left.name.eq_ignore_ascii_case(&right.name),
    );
    upsert_by(&mut next.jobs, delta.add_jobs, |left, right| {
        left.statement.eq_ignore_ascii_case(&right.statement)
    });
    upsert_by(&mut next.pains, delta.add_pains, |left, right| {
        left.statement.eq_ignore_ascii_case(&right.statement)
    });
    upsert_by(
        &mut next.desired_outcomes,
        delta.add_outcomes,
        |left, right| left.statement.eq_ignore_ascii_case(&right.statement),
    );
    upsert_by(
        &mut next.constraints,
        delta.add_constraints,
        |left, right| left.statement.eq_ignore_ascii_case(&right.statement),
    );
    upsert_by(
        &mut next.assumptions,
        delta.add_assumptions,
        |left, right| left.statement.eq_ignore_ascii_case(&right.statement),
    );
    if let Some(scope) = delta.scope {
        next.scope = scope;
    }
    for decision in delta.add_decisions {
        if let Some(existing) = next
            .decisions
            .iter_mut()
            .find(|existing| existing.id == decision.id)
        {
            if decision.version >= existing.version {
                *existing = decision;
            }
        } else {
            next.decisions.push(decision);
        }
    }
    for resolution in &mut delta.add_question_resolutions {
        if let Some(question) = state
            .open_questions
            .iter()
            .find(|question| question.id == resolution.question_id)
        {
            resolution.decision_target = question.decision_target.clone();
            resolution.predicted_information_gain_basis_points =
                question.expected_information_gain_basis_points;
            resolution.user_effort = question.user_effort.max(1);
        }
    }
    if !delta.resolve_question_ids.is_empty() {
        next.open_questions.retain(|question| {
            !delta
                .resolve_question_ids
                .iter()
                .any(|id| id == &question.id)
        });
    }
    upsert_by(
        &mut next.open_questions,
        delta
            .add_questions
            .into_iter()
            .map(OpenQuestion::with_recomputed_information_value)
            .collect(),
        |left, right| left.id == right.id,
    );
    upsert_by(
        &mut next.question_resolutions,
        delta.add_question_resolutions,
        |left, right| left.question_id == right.question_id,
    );
    upsert_by(
        &mut next.acceptance_criteria,
        delta.add_acceptance_criteria,
        |left, right| left.id == right.id,
    );
    upsert_by(
        &mut next.evidence_links,
        delta.add_evidence_links,
        |left, right| left.claim.eq_ignore_ascii_case(&right.claim),
    );
    upsert_by(
        &mut next.experiments,
        delta.add_experiments,
        |left, right| left.id == right.id,
    );
    if let Some(readiness) = delta.readiness {
        if matches!(
            readiness,
            RequirementReadiness::ReadyForReview | RequirementReadiness::Approved
        ) && !is_ready_for_review(&next)
        {
            return Err(RequirementDeltaError::InvalidReadiness("cannot mark requirement ready: problem, outcome, scope and testable acceptance criteria are incomplete".into()));
        }
        next.readiness = readiness;
    }
    Ok(next)
}

fn upsert_by<T, F>(target: &mut Vec<T>, values: Vec<T>, same_key: F)
where
    F: Fn(&T, &T) -> bool,
{
    for value in values {
        if let Some(existing) = target
            .iter_mut()
            .find(|existing| same_key(existing, &value))
        {
            *existing = value;
        } else {
            target.push(value);
        }
    }
}

pub fn is_ready_for_review(state: &RequirementState) -> bool {
    let unresolved_critical_assumption = state.assumptions.iter().any(|assumption| {
        assumption.importance >= 0.7
            && assumption.uncertainty >= 0.5
            && matches!(assumption.status, AssumptionStatus::Open)
            && assumption.falsification_test.is_none()
    });
    state.problem_frame.as_ref().is_some_and(|f| f.confirmed)
        && state.stakeholders.iter().any(|item| item.confirmed)
        && state.jobs.iter().any(|item| item.confirmed)
        && state.desired_outcomes.iter().any(|o| o.measure.is_some())
        && !state.scope.included.is_empty()
        && state.acceptance_criteria.iter().any(|c| c.testable)
        && state.open_questions.iter().all(|q| q.impact != "core")
        && !unresolved_critical_assumption
}
pub fn next_question(state: &RequirementState) -> Option<OpenQuestion> {
    state
        .open_questions
        .iter()
        .max_by(|a, b| {
            calibrated_question_score(state, a).total_cmp(&calibrated_question_score(state, b))
        })
        .cloned()
}

/// Decide what the orchestrator is allowed to do next from the durable state.
/// Research may gather evidence while a brief is incomplete, but delivery is
/// blocked until the readiness contract is satisfied.
pub fn planning_gate(state: &RequirementState) -> RequirementPlanningGate {
    if is_ready_for_review(state)
        && matches!(
            state.readiness,
            RequirementReadiness::ReadyForReview | RequirementReadiness::Approved
        )
    {
        return RequirementPlanningGate::ReadyForDelivery;
    }
    if let Some(question) = next_question(state) {
        if matches!(question.impact.as_str(), "core" | "high")
            && question.expected_information_gain_basis_points >= 500
        {
            return RequirementPlanningGate::Ask(question);
        }
    }
    RequirementPlanningGate::ContinueResearch
}
fn score(q: &OpenQuestion) -> f32 {
    let impact = match q.impact.as_str() {
        "core" => 3.0,
        "high" => 2.0,
        _ => 1.0,
    };
    let answerability = match q.answerability.as_str() {
        "high" => 1.0,
        "medium" => 0.7,
        _ => 0.4,
    };
    let information_gain = f32::from(q.expected_information_gain_basis_points) / 10_000.0;
    impact * information_gain * answerability / f32::from(q.user_effort.max(1))
}

fn calibrated_question_score(state: &RequirementState, question: &OpenQuestion) -> f32 {
    let relevant = state
        .question_resolutions
        .iter()
        .filter(|resolution| resolution.decision_target == question.decision_target)
        .collect::<Vec<_>>();
    if relevant.is_empty() {
        return score(question);
    }
    let absolute_error = relevant
        .iter()
        .map(|resolution| {
            let observed = if resolution.decision_changed {
                resolution.observed_convergence_basis_points
            } else {
                resolution.observed_convergence_basis_points / 2
            };
            resolution
                .predicted_information_gain_basis_points
                .abs_diff(observed)
        })
        .map(u32::from)
        .sum::<u32>()
        / u32::try_from(relevant.len()).unwrap_or(1);
    let reliability = (1.0 - absolute_error as f32 / 10_000.0).clamp(0.25, 1.0);
    score(question) * reliability
}

#[must_use]
pub fn question_calibration_report(
    resolutions: &[QuestionResolution],
) -> QuestionCalibrationReport {
    if resolutions.is_empty() {
        return QuestionCalibrationReport {
            sample_count: 0,
            mean_absolute_error_basis_points: 0,
            decision_change_rate_basis_points: 0,
            effort_adjusted_utility_basis_points: 0,
        };
    }
    let count = u32::try_from(resolutions.len()).unwrap_or(u32::MAX).max(1);
    let error = resolutions
        .iter()
        .map(|resolution| {
            resolution
                .predicted_information_gain_basis_points
                .abs_diff(resolution.observed_convergence_basis_points)
        })
        .map(u64::from)
        .sum::<u64>()
        / u64::from(count);
    let changes = resolutions
        .iter()
        .filter(|resolution| resolution.decision_changed)
        .count() as u64
        * 10_000
        / u64::from(count);
    let utility = resolutions
        .iter()
        .map(|resolution| {
            u64::from(resolution.observed_convergence_basis_points)
                / u64::from(resolution.user_effort.max(1))
        })
        .sum::<u64>()
        / u64::from(count);
    QuestionCalibrationReport {
        sample_count: count,
        mean_absolute_error_basis_points: u16::try_from(error.min(10_000)).unwrap_or(10_000),
        decision_change_rate_basis_points: u16::try_from(changes.min(10_000)).unwrap_or(10_000),
        effort_adjusted_utility_basis_points: u16::try_from(utility.min(10_000)).unwrap_or(10_000),
    }
}

fn expected_information_value(
    prior_uncertainty_basis_points: u16,
    branches: &[QuestionAnswerBranch],
) -> (u16, u16) {
    let prior = prior_uncertainty_basis_points.min(10_000);
    let distinct_effects = branches
        .iter()
        .filter_map(|branch| {
            let effect = branch.decision_effect.trim();
            (!effect.is_empty()).then_some(effect.to_ascii_lowercase())
        })
        .collect::<std::collections::BTreeSet<_>>();
    let total_probability = branches
        .iter()
        .map(|branch| u64::from(branch.probability_basis_points.min(10_000)))
        .sum::<u64>();
    if branches.len() < 2 || distinct_effects.len() < 2 || total_probability == 0 {
        return (prior, 0);
    }
    let weighted_posterior = branches
        .iter()
        .map(|branch| {
            u64::from(branch.probability_basis_points.min(10_000))
                * u64::from(branch.posterior_uncertainty_basis_points.min(10_000))
        })
        .sum::<u64>();
    let posterior = u16::try_from(weighted_posterior / total_probability)
        .unwrap_or(10_000)
        .min(10_000);
    (posterior, prior.saturating_sub(posterior))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_delta_is_incremental_and_gated() {
        let state = RequirementState::default();
        let mut delta = RequirementStateDelta::default();
        delta.source_event_ids = vec!["event-1".into()];
        delta.problem_frame = Some(Some(ProblemFrame {
            statement: "reduce latency".into(),
            confirmed: true,
        }));
        delta.add_stakeholders.push(Stakeholder {
            name: "operator".into(),
            role: None,
            confirmed: true,
        });
        delta.add_jobs.push(JobToBeDone {
            statement: "monitor".into(),
            evidence_ids: vec![],
            confirmed: true,
        });
        delta.add_outcomes.push(Outcome {
            statement: "p95 below 1s".into(),
            measure: Some("p95".into()),
        });
        delta.scope = Some(ScopeDefinition {
            included: vec!["latency monitoring".into()],
            excluded: vec!["capacity planning".into()],
        });
        delta.add_acceptance_criteria.push(AcceptanceCriterion {
            id: "ac1".into(),
            statement: "p95 < 1s".into(),
            testable: true,
        });
        delta.readiness = Some(RequirementReadiness::ReadyForReview);
        let next = apply_delta(&state, delta.clone(), &[]).unwrap();
        assert_eq!(next.version, 1);
        assert!(is_ready_for_review(&next));
        assert!(matches!(
            apply_delta(&next, delta, &["event-1".to_string()]),
            Err(RequirementDeltaError::DuplicateEvent(id)) if id == "event-1"
        ));
    }
    #[test]
    fn next_question_uses_information_value_not_fixed_order() {
        let mut s = RequirementState::default();
        s.open_questions = vec![
            OpenQuestion {
                id: "low".into(),
                question: "wording?".into(),
                impact: "low".into(),
                answerability: "high".into(),
                user_effort: 1,
                decision_target: QuestionDecisionTarget::Deliverable,
                prior_uncertainty_basis_points: 2_000,
                answer_branches: vec![
                    QuestionAnswerBranch {
                        id: "short".into(),
                        answer: "short".into(),
                        probability_basis_points: 5_000,
                        posterior_uncertainty_basis_points: 500,
                        decision_effect: "short copy".into(),
                    },
                    QuestionAnswerBranch {
                        id: "long".into(),
                        answer: "long".into(),
                        probability_basis_points: 5_000,
                        posterior_uncertainty_basis_points: 500,
                        decision_effect: "long copy".into(),
                    },
                ],
                expected_posterior_uncertainty_basis_points: 0,
                expected_information_gain_basis_points: 0,
            },
            OpenQuestion {
                id: "core".into(),
                question: "who?".into(),
                impact: "core".into(),
                answerability: "high".into(),
                user_effort: 1,
                decision_target: QuestionDecisionTarget::Stakeholder,
                prior_uncertainty_basis_points: 9_000,
                answer_branches: vec![
                    QuestionAnswerBranch {
                        id: "operator".into(),
                        answer: "operator".into(),
                        probability_basis_points: 5_000,
                        posterior_uncertainty_basis_points: 1_000,
                        decision_effect: "operator workflow".into(),
                    },
                    QuestionAnswerBranch {
                        id: "admin".into(),
                        answer: "admin".into(),
                        probability_basis_points: 5_000,
                        posterior_uncertainty_basis_points: 1_000,
                        decision_effect: "admin workflow".into(),
                    },
                ],
                expected_posterior_uncertainty_basis_points: 0,
                expected_information_gain_basis_points: 0,
            },
        ]
        .into_iter()
        .map(OpenQuestion::with_recomputed_information_value)
        .collect();
        assert_eq!(next_question(&s).unwrap().id, "core");
        assert!(matches!(planning_gate(&s), RequirementPlanningGate::Ask(_)));
    }

    #[test]
    fn discriminating_question_beats_high_impact_non_discriminating_question() {
        let non_discriminating = OpenQuestion {
            id: "prestige".into(),
            question: "Should the wording sound strategic?".into(),
            impact: "core".into(),
            answerability: "high".into(),
            user_effort: 1,
            decision_target: QuestionDecisionTarget::Deliverable,
            prior_uncertainty_basis_points: 9_000,
            answer_branches: vec![
                QuestionAnswerBranch {
                    id: "yes".into(),
                    answer: "yes".into(),
                    probability_basis_points: 5_000,
                    posterior_uncertainty_basis_points: 1_000,
                    decision_effect: "same implementation".into(),
                },
                QuestionAnswerBranch {
                    id: "no".into(),
                    answer: "no".into(),
                    probability_basis_points: 5_000,
                    posterior_uncertainty_basis_points: 1_000,
                    decision_effect: "same implementation".into(),
                },
            ],
            expected_posterior_uncertainty_basis_points: 1,
            expected_information_gain_basis_points: 9_999,
        }
        .with_recomputed_information_value();
        assert_eq!(non_discriminating.expected_information_gain_basis_points, 0);

        let discriminating = OpenQuestion {
            id: "population".into(),
            question: "Is this for new users or all active users?".into(),
            impact: "high".into(),
            answerability: "high".into(),
            user_effort: 1,
            decision_target: QuestionDecisionTarget::Population,
            prior_uncertainty_basis_points: 8_000,
            answer_branches: vec![
                QuestionAnswerBranch {
                    id: "new".into(),
                    answer: "new users".into(),
                    probability_basis_points: 4_000,
                    posterior_uncertainty_basis_points: 1_500,
                    decision_effect: "onboarding cohort".into(),
                },
                QuestionAnswerBranch {
                    id: "all".into(),
                    answer: "all active users".into(),
                    probability_basis_points: 6_000,
                    posterior_uncertainty_basis_points: 1_000,
                    decision_effect: "whole active population".into(),
                },
            ],
            expected_posterior_uncertainty_basis_points: 0,
            expected_information_gain_basis_points: 0,
        }
        .with_recomputed_information_value();
        let mut state = RequirementState::default();
        state.open_questions = vec![non_discriminating, discriminating];
        let selected = next_question(&state).expect("select a question");
        assert_eq!(selected.id, "population");
        assert_eq!(selected.expected_posterior_uncertainty_basis_points, 1_200);
        assert_eq!(selected.expected_information_gain_basis_points, 6_800);
    }

    #[test]
    fn observed_question_outcomes_are_persisted_and_calibrated() {
        let question = OpenQuestion {
            id: "scope".into(),
            question: "Which population?".into(),
            impact: "high".into(),
            answerability: "high".into(),
            user_effort: 2,
            decision_target: QuestionDecisionTarget::Population,
            prior_uncertainty_basis_points: 8_000,
            answer_branches: vec![
                QuestionAnswerBranch {
                    id: "new".into(),
                    answer: "new".into(),
                    probability_basis_points: 5_000,
                    posterior_uncertainty_basis_points: 2_000,
                    decision_effect: "new cohort".into(),
                },
                QuestionAnswerBranch {
                    id: "all".into(),
                    answer: "all".into(),
                    probability_basis_points: 5_000,
                    posterior_uncertainty_basis_points: 2_000,
                    decision_effect: "all users".into(),
                },
            ],
            expected_posterior_uncertainty_basis_points: 0,
            expected_information_gain_basis_points: 0,
        }
        .with_recomputed_information_value();
        let mut state = RequirementState::default();
        state.open_questions.push(question);
        let mut delta = RequirementStateDelta::default();
        delta.resolve_question_ids.push("scope".into());
        delta.add_question_resolutions.push(QuestionResolution {
            question_id: "scope".into(),
            selected_branch_id: Some("new".into()),
            observed_posterior_uncertainty_basis_points: 3_000,
            observed_convergence_basis_points: 5_000,
            decision_changed: true,
            source_event_ids: vec!["answer-event".into()],
            decision_target: Default::default(),
            predicted_information_gain_basis_points: 0,
            user_effort: 0,
        });
        let next = apply_delta(&state, delta, &[]).unwrap();
        let resolution = &next.question_resolutions[0];
        assert_eq!(
            resolution.decision_target,
            QuestionDecisionTarget::Population
        );
        assert_eq!(resolution.predicted_information_gain_basis_points, 6_000);
        assert_eq!(resolution.user_effort, 2);
        let report = question_calibration_report(&next.question_resolutions);
        assert_eq!(report.sample_count, 1);
        assert_eq!(report.mean_absolute_error_basis_points, 1_000);
        assert_eq!(report.effort_adjusted_utility_basis_points, 2_500);
    }

    #[test]
    fn planning_gate_only_allows_delivery_for_a_ready_state() {
        let mut s = RequirementState::default();
        assert!(matches!(
            planning_gate(&s),
            RequirementPlanningGate::ContinueResearch
        ));
        s.problem_frame = Some(ProblemFrame {
            statement: "ship".into(),
            confirmed: true,
        });
        s.stakeholders.push(Stakeholder {
            name: "owner".into(),
            role: None,
            confirmed: true,
        });
        s.jobs.push(JobToBeDone {
            statement: "deliver".into(),
            evidence_ids: vec![],
            confirmed: true,
        });
        s.desired_outcomes.push(Outcome {
            statement: "p95 < 1s".into(),
            measure: Some("p95".into()),
        });
        s.scope.included.push("delivery workflow".into());
        s.acceptance_criteria.push(AcceptanceCriterion {
            id: "ac".into(),
            statement: "test p95".into(),
            testable: true,
        });
        s.readiness = RequirementReadiness::ReadyForReview;
        assert!(matches!(
            planning_gate(&s),
            RequirementPlanningGate::ReadyForDelivery
        ));

        s.assumptions.push(Assumption {
            statement: "traffic remains stable".into(),
            type_: AssumptionType::Technical,
            importance: 0.9,
            uncertainty: 0.8,
            status: AssumptionStatus::Open,
            supporting_evidence: vec![],
            counter_evidence: vec![],
            falsification_test: None,
        });
        assert!(matches!(
            planning_gate(&s),
            RequirementPlanningGate::ContinueResearch
        ));
    }

    #[test]
    fn evolving_requirement_records_upsert_state_and_ignore_stale_decisions() {
        let mut initial = RequirementState::default();
        initial.assumptions.push(Assumption {
            statement: "capacity is sufficient".into(),
            type_: AssumptionType::Technical,
            importance: 0.9,
            uncertainty: 0.8,
            status: AssumptionStatus::Open,
            supporting_evidence: vec![],
            counter_evidence: vec![],
            falsification_test: None,
        });
        initial.acceptance_criteria.push(AcceptanceCriterion {
            id: "ac".into(),
            statement: "works".into(),
            testable: false,
        });
        initial.experiments.push(ValidationExperiment {
            id: "exp".into(),
            hypothesis: "capacity holds".into(),
            success_signal: "p95 remains stable".into(),
            status: "planned".into(),
        });
        initial.decisions.push(DecisionRef {
            id: "decision".into(),
            statement: "pilot".into(),
            version: 2,
        });

        let mut delta = RequirementStateDelta::default();
        delta.add_assumptions.push(Assumption {
            statement: "capacity is sufficient".into(),
            type_: AssumptionType::Technical,
            importance: 0.9,
            uncertainty: 0.1,
            status: AssumptionStatus::Supported,
            supporting_evidence: vec!["load-test".into()],
            counter_evidence: vec![],
            falsification_test: Some("repeat load test monthly".into()),
        });
        delta.add_acceptance_criteria.push(AcceptanceCriterion {
            id: "ac".into(),
            statement: "p95 remains below one second".into(),
            testable: true,
        });
        delta.add_experiments.push(ValidationExperiment {
            id: "exp".into(),
            hypothesis: "capacity holds".into(),
            success_signal: "p95 remains stable".into(),
            status: "completed".into(),
        });
        delta.add_decisions.push(DecisionRef {
            id: "decision".into(),
            statement: "roll out".into(),
            version: 3,
        });
        let next = apply_delta(&initial, delta, &[]).unwrap();
        assert_eq!(next.assumptions.len(), 1);
        assert_eq!(next.assumptions[0].status, AssumptionStatus::Supported);
        assert_eq!(next.acceptance_criteria.len(), 1);
        assert!(next.acceptance_criteria[0].testable);
        assert_eq!(next.experiments.len(), 1);
        assert_eq!(next.experiments[0].status, "completed");
        assert_eq!(next.decisions.len(), 1);
        assert_eq!(next.decisions[0].version, 3);

        let mut stale = RequirementStateDelta::default();
        stale.add_decisions.push(DecisionRef {
            id: "decision".into(),
            statement: "old pilot".into(),
            version: 1,
        });
        let after_stale = apply_delta(&next, stale, &[]).unwrap();
        assert_eq!(after_stale.decisions[0].version, 3);
        assert_eq!(after_stale.decisions[0].statement, "roll out");
    }
}
