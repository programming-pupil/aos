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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenQuestion {
    pub id: String,
    pub question: String,
    pub impact: String,
    pub answerability: String,
    pub user_effort: u8,
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
    pub add_questions: Vec<OpenQuestion>,
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
            add_questions: vec![],
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
    delta: RequirementStateDelta,
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
    extend_unique(&mut next.stakeholders, delta.add_stakeholders);
    extend_unique(&mut next.jobs, delta.add_jobs);
    extend_unique(&mut next.pains, delta.add_pains);
    extend_unique(&mut next.desired_outcomes, delta.add_outcomes);
    extend_unique(&mut next.constraints, delta.add_constraints);
    extend_unique(&mut next.assumptions, delta.add_assumptions);
    extend_unique(&mut next.open_questions, delta.add_questions);
    extend_unique(&mut next.acceptance_criteria, delta.add_acceptance_criteria);
    extend_unique(&mut next.evidence_links, delta.add_evidence_links);
    extend_unique(&mut next.experiments, delta.add_experiments);
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

fn extend_unique<T: PartialEq>(target: &mut Vec<T>, values: Vec<T>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

pub fn is_ready_for_review(state: &RequirementState) -> bool {
    state.problem_frame.as_ref().is_some_and(|f| f.confirmed)
        && !state.stakeholders.is_empty()
        && !state.jobs.is_empty()
        && state.desired_outcomes.iter().any(|o| o.measure.is_some())
        && state.acceptance_criteria.iter().any(|c| c.testable)
        && state.open_questions.iter().all(|q| q.impact != "core")
}
pub fn next_question(state: &RequirementState) -> Option<OpenQuestion> {
    state
        .open_questions
        .iter()
        .max_by(|a, b| score(a).total_cmp(&score(b)))
        .cloned()
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
    impact * answerability / f32::from(q.user_effort.max(1))
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
        });
        delta.add_outcomes.push(Outcome {
            statement: "p95 below 1s".into(),
            measure: Some("p95".into()),
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
            },
            OpenQuestion {
                id: "core".into(),
                question: "who?".into(),
                impact: "core".into(),
                answerability: "high".into(),
                user_effort: 1,
            },
        ];
        assert_eq!(next_question(&s).unwrap().id, "core");
    }
}
