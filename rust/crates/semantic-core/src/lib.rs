//! Semantic State Kernel.
//!
//! LLMs may propose deltas, but only this deterministic reducer can turn a
//! proposal into a confirmed/current state.  Assertions are versioned rather
//! than overwritten, so temporal changes and conflicts remain auditable.

mod compaction;
mod context;
mod evidence;
mod reducer;
mod types;

pub use compaction::{CompactionCandidate, CompactionError, CompactionValidator};
pub use context::{
    ContextBlock, ContextCompiler, ContextEnvelope, ContextError, ContextOutputContract,
    ContextPacket, ContextReference, ContextSelection, ContextTrust, PromptLayer,
};
pub use evidence::{
    EvidenceAuthority, EvidenceLedger, EvidenceLedgerError, EvidenceRef, EvidenceSourceType,
    SourceRange,
};
pub use reducer::{ReductionError, ReductionOutcome, SemanticReducer};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn assertion(id: &str, value: &str, source: &str) -> SemanticAssertion {
        SemanticAssertion {
            id: id.into(),
            tenant_id: "tenant".into(),
            scope: AssertionScope::Session("s".into()),
            subject: EntityRef::new("user", "u1"),
            predicate: "theme".into(),
            value: TypedValue::String(value.into()),
            qualifiers: Default::default(),
            valid_time: None,
            observed_at: Utc::now(),
            status: AssertionStatus::Proposed,
            confidence: CalibratedScore::new(0.7).unwrap(),
            source_refs: vec![EvidenceRef {
                evidence_id: source.into(),
                source_type: EvidenceSourceType::Message,
                source_locator: "msg".into(),
                content_hash: source.into(),
                event_seq: Some(1),
                byte_or_line_range: None,
                collected_at: Utc::now(),
                authority: EvidenceAuthority::User,
            }],
            supersedes: vec![],
            conflicts_with: vec![],
            sensitivity: Sensitivity::Internal,
            retention: RetentionPolicy::Standard,
        }
    }

    #[test]
    fn reducer_is_idempotent_and_preserves_old_versions() {
        let reducer = SemanticReducer::default();
        let mut snapshot = SemanticSnapshot::default();
        let mut evidence = EvidenceLedger::default();
        for id in ["e1", "e2"] {
            evidence
                .append(EvidenceRef {
                    evidence_id: id.into(),
                    source_type: EvidenceSourceType::Message,
                    source_locator: "msg".into(),
                    content_hash: id.into(),
                    event_seq: Some(1),
                    byte_or_line_range: None,
                    collected_at: Utc::now(),
                    authority: EvidenceAuthority::User,
                })
                .unwrap();
        }
        let first = reducer
            .apply(
                &snapshot,
                ProposedStateDelta::upsert(assertion("a1", "dark", "e1")),
                &evidence,
            )
            .unwrap();
        snapshot = first.snapshot;
        let second = reducer
            .apply(
                &snapshot,
                ProposedStateDelta::upsert(assertion("a2", "light", "e2")),
                &evidence,
            )
            .unwrap();
        assert_eq!(second.snapshot.version, 2);
        assert!(second.conflicts.contains(&"a1".to_string()));
        assert_eq!(second.snapshot.assertions.len(), 2);
        let repeat = reducer
            .apply(
                &second.snapshot,
                ProposedStateDelta::upsert(assertion("a2", "light", "e2")),
                &evidence,
            )
            .unwrap();
        assert_eq!(repeat.snapshot.version, second.snapshot.version);
        assert!(repeat.accepted.is_empty());
    }

    #[test]
    fn reducer_rejects_missing_evidence_and_conflicts_without_overwriting() {
        let reducer = SemanticReducer::default();
        let mut snapshot = SemanticSnapshot::default();
        let mut no_evidence = assertion("a1", "dark", "missing");
        no_evidence.source_refs.clear();
        assert!(matches!(
            reducer.apply(
                &snapshot,
                ProposedStateDelta::upsert(no_evidence),
                &EvidenceLedger::default()
            ),
            Err(ReductionError::MissingEvidence(_))
        ));
        let e1 = EvidenceRef {
            evidence_id: "e1".into(),
            source_type: EvidenceSourceType::Message,
            source_locator: "m".into(),
            content_hash: "h".into(),
            event_seq: Some(1),
            byte_or_line_range: None,
            collected_at: Utc::now(),
            authority: EvidenceAuthority::User,
        };
        let mut evidence = EvidenceLedger::default();
        evidence.append(e1).unwrap();
        let first = reducer
            .apply(
                &snapshot,
                ProposedStateDelta::upsert(assertion("a1", "dark", "e1")),
                &evidence,
            )
            .unwrap();
        snapshot = first.snapshot;
        let mut conflicting = assertion("a2", "light", "e1");
        conflicting.source_refs[0].evidence_id = "e1".into();
        let outcome = reducer
            .apply(
                &snapshot,
                ProposedStateDelta::upsert(conflicting),
                &evidence,
            )
            .unwrap();
        assert!(!outcome.conflicts.is_empty());
        assert_eq!(
            outcome.snapshot.assertions.get("a1").unwrap().value,
            TypedValue::String("dark".into())
        );
    }

    #[test]
    fn reducer_demotes_model_self_confirmation_for_assertions_and_decisions() {
        let reducer = SemanticReducer::default();
        let mut evidence = EvidenceLedger::default();
        let model_evidence = EvidenceRef {
            evidence_id: "model-evidence".into(),
            source_type: EvidenceSourceType::Provider,
            source_locator: "provider://turn".into(),
            content_hash: "model-hash".into(),
            event_seq: Some(1),
            byte_or_line_range: None,
            collected_at: Utc::now(),
            authority: EvidenceAuthority::Model,
        };
        evidence.append(model_evidence.clone()).unwrap();

        let mut self_confirmed = assertion("model-assertion", "dark", "model-evidence");
        self_confirmed.status = AssertionStatus::Confirmed;
        self_confirmed.source_refs = vec![model_evidence.clone()];
        let assertion_outcome = reducer
            .apply(
                &SemanticSnapshot::default(),
                ProposedStateDelta::UpsertAssertion(self_confirmed),
                &evidence,
            )
            .unwrap();
        assert_eq!(
            assertion_outcome
                .snapshot
                .assertions
                .get("model-assertion")
                .unwrap()
                .status,
            AssertionStatus::Proposed
        );
        assert_eq!(assertion_outcome.needs_confirmation, ["model-assertion"]);

        let decision = DecisionRecord {
            id: "model-decision".into(),
            scope: AssertionScope::Session("s".into()),
            question: "Which theme?".into(),
            decision: "dark".into(),
            alternatives: Vec::new(),
            rationale: vec![model_evidence],
            constraints: Vec::new(),
            acceptance_criteria: Vec::new(),
            owner: None,
            status: DecisionStatus::Accepted,
            valid_time: None,
            version: 1,
        };
        let decision_outcome = reducer
            .apply(
                &SemanticSnapshot::default(),
                ProposedStateDelta::UpsertDecision(decision),
                &evidence,
            )
            .unwrap();
        assert_eq!(
            decision_outcome
                .snapshot
                .decisions
                .get("model-decision")
                .unwrap()
                .status,
            DecisionStatus::Proposed
        );
        assert_eq!(decision_outcome.needs_confirmation, ["model-decision"]);
    }

    #[test]
    fn evidence_requires_source_coverage_and_context_has_manifest() {
        let mut evidence = EvidenceLedger::default();
        evidence
            .append(EvidenceRef {
                evidence_id: "e".into(),
                source_type: EvidenceSourceType::ToolResult,
                source_locator: "artifact://1".into(),
                content_hash: "abc".into(),
                event_seq: Some(2),
                byte_or_line_range: Some(SourceRange { start: 1, end: 4 }),
                collected_at: Utc::now(),
                authority: EvidenceAuthority::Tool,
            })
            .unwrap();
        assert!(evidence.validate_source_coverage(&[2]).is_ok());
        let packet = ContextCompiler::default()
            .compile(
                ContextSelection {
                    objective: "answer".into(),
                    envelope: ContextEnvelope::default(),
                    blocks: vec![ContextBlock::new("e", "evidence", "hello", 2, false, "abc")],
                },
                100,
            )
            .unwrap();
        assert_eq!(packet.manifest.used_tokens, 2);
        assert_eq!(packet.blocks[0].content, "hello");
    }

    #[test]
    fn model_context_selection_drops_oldest_recent_blocks_under_budget() {
        let compiler = ContextCompiler::default();
        let packet = compiler
            .compile_for_model(
                ContextSelection {
                    objective: "answer".into(),
                    envelope: ContextEnvelope::default(),
                    blocks: vec![
                        ContextBlock {
                            block_id: "system:0".into(),
                            source: "stable_system".into(),
                            content: "system".into(),
                            tokens: 2,
                            truncated: false,
                            source_hash: "s".into(),
                            policy_version: "v1".into(),
                            layer: PromptLayer::StableSystem,
                            selection_reason: "required policy".into(),
                            trust: ContextTrust::Instruction,
                        },
                        ContextBlock {
                            block_id: "message:0".into(),
                            source: "recent_interaction".into(),
                            content: "old".into(),
                            tokens: 4,
                            truncated: false,
                            source_hash: "o".into(),
                            policy_version: "v1".into(),
                            layer: PromptLayer::RecentInteraction,
                            selection_reason: "recent history".into(),
                            trust: ContextTrust::UntrustedData,
                        },
                        ContextBlock {
                            block_id: "message:1".into(),
                            source: "recent_interaction".into(),
                            content: "new".into(),
                            tokens: 4,
                            truncated: false,
                            source_hash: "n".into(),
                            policy_version: "v1".into(),
                            layer: PromptLayer::RecentInteraction,
                            selection_reason: "recent history".into(),
                            trust: ContextTrust::UntrustedData,
                        },
                        ContextBlock {
                            block_id: "task:current".into(),
                            source: "current_user_request".into(),
                            content: "current".into(),
                            tokens: 3,
                            truncated: false,
                            source_hash: "c".into(),
                            policy_version: "v1".into(),
                            layer: PromptLayer::TaskPacket,
                            selection_reason: "latest user objective".into(),
                            trust: ContextTrust::UntrustedData,
                        },
                    ],
                },
                9,
            )
            .expect("selection should fit the hard budget");
        assert_eq!(packet.manifest.used_tokens, 9);
        assert!(packet
            .blocks
            .iter()
            .any(|block| block.block_id == "system:0"));
        assert!(packet
            .blocks
            .iter()
            .any(|block| block.block_id == "task:current"));
        assert!(packet
            .blocks
            .iter()
            .any(|block| block.block_id == "message:1"));
        assert!(!packet
            .blocks
            .iter()
            .any(|block| block.block_id == "message:0"));
    }
}
