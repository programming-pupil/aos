use crate::EvidenceRef;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

pub type AssertionId = String;
pub type DecisionId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssertionScope {
    Tenant,
    User(String),
    Project(String),
    Session(String),
    Thread(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
}
impl EntityRef {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TypedValue {
    Null,
    String(String),
    Number(f64),
    Boolean(bool),
    Json(serde_json::Value),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalibratedScore(u16);
impl CalibratedScore {
    pub fn new(value: f32) -> Result<Self, ScoreError> {
        if !(0.0..=1.0).contains(&value) || !value.is_finite() {
            return Err(ScoreError::OutOfRange);
        }
        Ok(Self((value * 10_000.0).round() as u16))
    }
    pub fn value(self) -> f32 {
        f32::from(self.0) / 10_000.0
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScoreError {
    #[error("score must be finite and within [0, 1]")]
    OutOfRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AssertionStatus {
    Proposed,
    Confirmed,
    Disputed,
    Superseded,
    Expired,
    Rejected,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Sensitivity {
    Public,
    Internal,
    Confidential,
    Secret,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RetentionPolicy {
    Standard,
    Short,
    UntilDeleted,
    Compliance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeInterval {
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticAssertion {
    pub id: AssertionId,
    pub tenant_id: String,
    pub scope: AssertionScope,
    pub subject: EntityRef,
    pub predicate: String,
    pub value: TypedValue,
    pub qualifiers: BTreeMap<String, TypedValue>,
    pub valid_time: Option<TimeInterval>,
    pub observed_at: DateTime<Utc>,
    pub status: AssertionStatus,
    pub confidence: CalibratedScore,
    pub source_refs: Vec<EvidenceRef>,
    pub supersedes: Vec<AssertionId>,
    pub conflicts_with: Vec<AssertionId>,
    pub sensitivity: Sensitivity,
    pub retention: RetentionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Superseded,
    Rejected,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionAlternative {
    pub label: String,
    pub rationale: Vec<EvidenceRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub statement: String,
    pub testable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionRecord {
    pub id: DecisionId,
    pub scope: AssertionScope,
    pub question: String,
    pub decision: String,
    pub alternatives: Vec<DecisionAlternative>,
    pub rationale: Vec<EvidenceRef>,
    pub constraints: Vec<AssertionId>,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub owner: Option<EntityRef>,
    pub status: DecisionStatus,
    pub valid_time: Option<TimeInterval>,
    pub version: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SemanticSnapshot {
    pub version: u64,
    pub assertions: BTreeMap<AssertionId, SemanticAssertion>,
    pub decisions: BTreeMap<DecisionId, DecisionRecord>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProposedStateDelta {
    UpsertAssertion(SemanticAssertion),
    UpsertDecision(DecisionRecord),
    ExpireAssertion(AssertionId),
    Noop { source_event_ids: Vec<String> },
}
impl ProposedStateDelta {
    pub fn upsert(assertion: SemanticAssertion) -> Self {
        Self::UpsertAssertion(assertion)
    }
}
