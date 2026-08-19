use crate::{
    AssertionStatus, DecisionRecord, DecisionStatus, EvidenceAuthority, EvidenceLedger,
    ProposedStateDelta, SemanticAssertion, SemanticSnapshot, Sensitivity,
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReductionError {
    #[error("assertion {0} has no evidence")]
    MissingEvidence(String),
    #[error("assertion {assertion_id} references missing evidence {evidence_id}")]
    UnknownEvidence {
        assertion_id: String,
        evidence_id: String,
    },
    #[error("tenant mismatch for assertion {0}")]
    TenantMismatch(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReductionOutcome {
    pub snapshot: SemanticSnapshot,
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
    pub conflicts: Vec<String>,
    pub superseded: Vec<String>,
    pub needs_confirmation: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SemanticReducer;
impl SemanticReducer {
    pub fn apply(
        &self,
        current: &SemanticSnapshot,
        delta: ProposedStateDelta,
        evidence: &EvidenceLedger,
    ) -> Result<ReductionOutcome, ReductionError> {
        let mut next = current.clone();
        let mut outcome = ReductionOutcome {
            snapshot: current.clone(),
            ..ReductionOutcome::default()
        };
        match delta {
            ProposedStateDelta::UpsertAssertion(mut candidate) => {
                if candidate.source_refs.is_empty() {
                    return Err(ReductionError::MissingEvidence(candidate.id));
                }
                for source in &candidate.source_refs {
                    if !evidence.contains(&source.evidence_id) {
                        return Err(ReductionError::UnknownEvidence {
                            assertion_id: candidate.id.clone(),
                            evidence_id: source.evidence_id.clone(),
                        });
                    }
                }
                if candidate.status == AssertionStatus::Confirmed
                    && !assertion_confirmation_authorized(&candidate, evidence)
                {
                    candidate.status = AssertionStatus::Proposed;
                }
                if let Some(existing) = next.assertions.get(&candidate.id) {
                    // Derived conflict/supersession edges are reducer output;
                    // they must not make a retry of the same proposal look
                    // like a new semantic event.
                    if existing.tenant_id == candidate.tenant_id
                        && existing.scope == candidate.scope
                        && existing.subject == candidate.subject
                        && existing.predicate == candidate.predicate
                        && existing.value == candidate.value
                        && existing.qualifiers == candidate.qualifiers
                        && existing
                            .source_refs
                            .iter()
                            .map(|r| (&r.evidence_id, &r.content_hash))
                            .eq(candidate
                                .source_refs
                                .iter()
                                .map(|r| (&r.evidence_id, &r.content_hash)))
                    {
                        return Ok(outcome);
                    }
                }
                let related: Vec<_> = next
                    .assertions
                    .values()
                    .filter(|a| {
                        a.tenant_id == candidate.tenant_id
                            && a.scope == candidate.scope
                            && a.subject == candidate.subject
                            && a.predicate == candidate.predicate
                            && a.status != AssertionStatus::Superseded
                            && a.id != candidate.id
                    })
                    .cloned()
                    .collect();
                for existing in related {
                    if existing.value == candidate.value {
                        candidate.supersedes.push(existing.id.clone());
                        outcome.superseded.push(existing.id.clone());
                    } else {
                        candidate.conflicts_with.push(existing.id.clone());
                        outcome.conflicts.push(existing.id.clone());
                    }
                }
                if candidate.status == AssertionStatus::Proposed {
                    outcome.needs_confirmation.push(candidate.id.clone());
                }
                for id in candidate
                    .supersedes
                    .iter()
                    .chain(candidate.conflicts_with.iter())
                {
                    if let Some(old) = next.assertions.get_mut(id) {
                        if candidate.supersedes.contains(id) {
                            old.status = AssertionStatus::Superseded;
                        } else {
                            old.status = AssertionStatus::Disputed;
                        }
                    }
                }
                let id = candidate.id.clone();
                next.assertions.insert(id.clone(), candidate);
                outcome.accepted.push(id);
            }
            ProposedStateDelta::UpsertDecision(mut decision) => {
                for source in &decision.rationale {
                    if !evidence.contains(&source.evidence_id) {
                        return Err(ReductionError::UnknownEvidence {
                            assertion_id: decision.id.clone(),
                            evidence_id: source.evidence_id.clone(),
                        });
                    }
                }
                if decision.status == DecisionStatus::Accepted
                    && !decision_acceptance_authorized(&decision, evidence)
                {
                    decision.status = DecisionStatus::Proposed;
                    outcome.needs_confirmation.push(decision.id.clone());
                }
                let id = decision.id.clone();
                if next.decisions.get(&id) == Some(&decision) {
                    return Ok(outcome);
                }
                next.decisions.insert(id.clone(), decision);
                outcome.accepted.push(id);
            }
            ProposedStateDelta::ExpireAssertion(id) => {
                if let Some(assertion) = next.assertions.get_mut(&id) {
                    assertion.status = AssertionStatus::Expired;
                    outcome.accepted.push(id);
                } else {
                    outcome.rejected.push(id);
                }
            }
            ProposedStateDelta::Noop { .. } => return Ok(outcome),
        }
        next.version = current.version.saturating_add(1);
        next.updated_at = Some(chrono::Utc::now());
        outcome.snapshot = next;
        Ok(outcome)
    }
}

fn assertion_confirmation_authorized(
    assertion: &SemanticAssertion,
    evidence: &EvidenceLedger,
) -> bool {
    assertion.source_refs.iter().any(|source| {
        evidence.get(&source.evidence_id).is_some_and(|entry| {
            matches!(
                entry.authority,
                EvidenceAuthority::User | EvidenceAuthority::Owner
            ) || (matches!(
                assertion.sensitivity,
                Sensitivity::Public | Sensitivity::Internal
            ) && matches!(
                entry.authority,
                EvidenceAuthority::Tool | EvidenceAuthority::Document
            ))
        })
    })
}

fn decision_acceptance_authorized(decision: &DecisionRecord, evidence: &EvidenceLedger) -> bool {
    decision.rationale.iter().any(|source| {
        evidence.get(&source.evidence_id).is_some_and(|entry| {
            matches!(
                entry.authority,
                EvidenceAuthority::User | EvidenceAuthority::Owner
            )
        })
    })
}
