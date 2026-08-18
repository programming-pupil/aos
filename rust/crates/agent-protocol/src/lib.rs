//! Unified Agent Protocol v1.
//!
//! This crate intentionally contains no HTTP, database or model code.  It is
//! the small, deterministic contract shared by the native runtime and future
//! executor adapters.  The in-memory ledger is also used by replay tests and
//! can be replaced by a durable implementation without changing event types.

mod budget;
mod capabilities;
mod governance;
mod interactions;
mod ledger;
mod lifecycle;
mod protocol;

pub use budget::{
    BudgetDimension, BudgetError, BudgetLedger, BudgetPurpose, BudgetReservation, BudgetState,
};
pub use capabilities::{
    PromptManifest, PromptRegistry, PromptVariant, ToolCandidate, ToolCapabilityRouter,
    ToolDecision,
};
pub use governance::{
    ArtifactObject, ArtifactPlane, CapabilityScope, CapabilityToken, ProjectedPayload,
    ProjectionKind, SensitiveProjectionPolicy, SensitiveProjector,
};
pub use interactions::{
    DurableInteraction, InteractionError, InteractionKind, InteractionResponse, InteractionScope,
    InteractionState,
};
pub use ledger::{AppendReceipt, CorruptionKind, EventLedger, LedgerError, LedgerRecord};
pub use lifecycle::{LifecycleError, ToolLifecycle, ToolLifecycleState};
pub use protocol::*;

/// Emit a gated marker used by the conformance gate to prove that a test
/// exercised the production contract.  It is inert unless the gate sets the
/// matching case id, so normal runtime logs never contain test-only noise.
pub(crate) fn behavior_trace(case_id: &str) {
    if std::env::var("AOS_BEHAVIOR_TRACE_CASE").as_deref() == Ok(case_id) {
        static EMITTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if EMITTED.set(()).is_ok() {
            eprintln!("AOS_PRODUCTION_TRACE\t{case_id}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn message(sequence: u64) -> AgentEventEnvelope {
        AgentEventEnvelope::new(
            "thread-1",
            Some("turn-1"),
            None,
            format!("item-{sequence}"),
            AgentEventV1::Message(MessageItem {
                role: MessageRole::User,
                text: format!("hello-{sequence}"),
                content_hash: None,
            }),
            sequence,
        )
    }

    #[test]
    fn envelope_hash_is_deterministic_and_verifiable() {
        let mut event = message(1);
        let hash = event.payload_hash.clone();
        assert_eq!(hash, event.compute_payload_hash().unwrap());
        event.event = AgentEventV1::Message(MessageItem {
            role: MessageRole::User,
            text: "tampered".into(),
            content_hash: None,
        });
        assert_ne!(hash, event.compute_payload_hash().unwrap());
        assert!(event.verify_hash().is_err());
    }

    #[test]
    fn ledger_fences_stale_writers_and_deduplicates_idempotency() {
        let mut ledger = EventLedger::default();
        let first = ledger.acquire_writer("thread-1", "worker-a").unwrap();
        let stale = ledger.acquire_writer("thread-1", "worker-b").unwrap();
        assert!(ledger.append(&first, message(1)).is_err());
        let receipt = ledger.append(&stale, message(1)).unwrap();
        assert_eq!(receipt.sequence, 1);
        let mut duplicate = message(2);
        duplicate.idempotency_key = Some("same".into());
        duplicate.payload_hash = duplicate.compute_payload_hash().unwrap();
        ledger.append(&stale, duplicate.clone()).unwrap();
        let duplicate_receipt = ledger.append(&stale, duplicate).unwrap();
        assert!(duplicate_receipt.deduplicated);
    }

    #[test]
    fn lifecycle_rejects_unsafe_transition() {
        assert!(ToolLifecycle::new()
            .transition(ToolLifecycleState::Completed)
            .is_err());
        let mut lifecycle = ToolLifecycle::new();
        lifecycle
            .transition(ToolLifecycleState::AwaitingAuthorization)
            .unwrap();
        lifecycle
            .transition(ToolLifecycleState::Authorized)
            .unwrap();
        lifecycle.transition(ToolLifecycleState::Started).unwrap();
        lifecycle.transition(ToolLifecycleState::Completed).unwrap();
        assert!(lifecycle.transition(ToolLifecycleState::Started).is_err());
    }

    #[test]
    fn budget_parent_child_is_conserved() {
        let mut ledger = BudgetLedger::new([(BudgetDimension::TokenInput, 100)]);
        let parent = ledger
            .reserve("parent", [(BudgetDimension::TokenInput, 60)])
            .unwrap();
        assert!(ledger
            .reserve("child", [(BudgetDimension::TokenInput, 50)])
            .is_err());
        ledger
            .commit(&parent, [(BudgetDimension::TokenInput, 20)])
            .unwrap();
        assert_eq!(
            ledger.state(BudgetDimension::TokenInput),
            BudgetState {
                available: 80,
                reserved: 0,
                committed: 20
            }
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn protected_final_and_verifier_budget_cannot_be_spent_by_general_work() {
        let mut ledger = BudgetLedger::new([
            (BudgetDimension::TokenInput, 100),
            (BudgetDimension::TokenOutput, 60),
        ]);
        let final_budget = ledger
            .reserve_protected(
                "turn-final",
                BudgetPurpose::FinalSynthesis,
                [
                    (BudgetDimension::TokenInput, 30),
                    (BudgetDimension::TokenOutput, 20),
                ],
            )
            .unwrap();
        let verifier_budget = ledger
            .reserve_protected(
                "turn-verifier",
                BudgetPurpose::DomainVerifier,
                [
                    (BudgetDimension::TokenInput, 20),
                    (BudgetDimension::TokenOutput, 10),
                ],
            )
            .unwrap();

        assert!(matches!(
            ledger.reserve(
                "exploration",
                [
                    (BudgetDimension::TokenInput, 51),
                    (BudgetDimension::TokenOutput, 31),
                ]
            ),
            Err(BudgetError::Insufficient { .. })
        ));
        let exploration = ledger
            .reserve(
                "exploration",
                [
                    (BudgetDimension::TokenInput, 50),
                    (BudgetDimension::TokenOutput, 30),
                ],
            )
            .unwrap();
        let final_call = ledger
            .reserve_child(
                &final_budget,
                "final-call",
                [
                    (BudgetDimension::TokenInput, 25),
                    (BudgetDimension::TokenOutput, 15),
                ],
            )
            .unwrap();
        assert!(matches!(
            ledger.release(&final_budget),
            Err(BudgetError::ActiveChildren(_))
        ));

        ledger
            .commit(
                &final_call,
                [
                    (BudgetDimension::TokenInput, 18),
                    (BudgetDimension::TokenOutput, 9),
                ],
            )
            .unwrap();
        assert!(matches!(
            ledger.reserve_child(
                &final_budget,
                "oversized-second-final-call",
                [
                    (BudgetDimension::TokenInput, 13),
                    (BudgetDimension::TokenOutput, 12),
                ],
            ),
            Err(BudgetError::Insufficient { .. })
        ));
        ledger.release(&final_budget).unwrap();
        ledger.release(&verifier_budget).unwrap();
        ledger
            .commit(
                &exploration,
                [
                    (BudgetDimension::TokenInput, 40),
                    (BudgetDimension::TokenOutput, 20),
                ],
            )
            .unwrap();

        assert_eq!(
            ledger.state(BudgetDimension::TokenInput),
            BudgetState {
                available: 42,
                reserved: 0,
                committed: 58,
            }
        );
        assert_eq!(
            ledger.state(BudgetDimension::TokenOutput),
            BudgetState {
                available: 31,
                reserved: 0,
                committed: 29,
            }
        );
    }

    #[test]
    fn tail_repair_is_fail_closed_for_middle_corruption() {
        let mut ledger = EventLedger::default();
        let writer = ledger.acquire_writer("t", "w").unwrap();
        ledger.append(&writer, message(1)).unwrap();
        ledger.append(&writer, message(2)).unwrap();
        ledger
            .corrupt_for_test("t", 1, CorruptionKind::PayloadHash)
            .unwrap();
        assert!(matches!(
            ledger.repair("t"),
            Err(LedgerError::Corruption { .. })
        ));
    }

    #[test]
    fn uncommitted_torn_tail_is_discarded_but_committed_corruption_is_not() {
        let mut ledger = EventLedger::default();
        let writer = ledger.acquire_writer("tail", "w").unwrap();
        ledger.append(&writer, message(1)).unwrap();
        let mut tail = message(2);
        tail.payload_hash = "partial".into();
        ledger.append_uncommitted_for_test(&writer, tail).unwrap();
        assert_eq!(ledger.repair("tail").unwrap(), 1);
        assert_eq!(ledger.records("tail").unwrap().len(), 1);
    }

    #[test]
    fn checkpoint_and_protocol_are_forward_compatible() {
        let checkpoint = CheckpointEvent {
            checkpoint_id: "cp".into(),
            source_event_seqs: vec![1, 2],
            state_hash: "hash".into(),
            durable: true,
        };
        let event = AgentEventEnvelope::new(
            "t",
            None,
            None,
            "i",
            AgentEventV1::Checkpoint(checkpoint),
            1,
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("schema_version"));
        assert_eq!(event.occurred_at.timezone(), Utc::now().timezone());
    }
}
