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
    ContextBlock, ContextCompiler, ContextError, ContextPacket, ContextSelection, PromptLayer,
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
                    blocks: vec![ContextBlock::new("e", "evidence", "hello", 2, false, "abc")],
                },
                100,
            )
            .unwrap();
        assert_eq!(packet.manifest.used_tokens, 2);
        assert_eq!(packet.blocks[0].content, "hello");
    }
}
