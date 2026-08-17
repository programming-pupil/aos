//! In-memory semantic-kernel fixture used by isolated runtime tests. Production
//! execution and Memory state are owned by the durable ledger/repository
//! adapters; this type deliberately does not claim persistence authority.

use agent_protocol::{AgentEventEnvelope, AgentEventV1, EventLedger, MessageItem, MessageRole};
use memory_engine::{DualChannelExtraction, InMemoryMemoryRepository};
use semantic_core::{
    EvidenceLedger, ProposedStateDelta, ReductionOutcome, SemanticAssertion, SemanticReducer,
    SemanticSnapshot,
};

#[derive(Debug, Clone)]
pub struct SemanticKernelBridge {
    pub enabled: bool,
    pub execution: EventLedger,
    pub evidence: EvidenceLedger,
    pub snapshot: SemanticSnapshot,
    pub memory: InMemoryMemoryRepository,
    reducer: SemanticReducer,
}

impl SemanticKernelBridge {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            execution: EventLedger::default(),
            evidence: EvidenceLedger::default(),
            snapshot: SemanticSnapshot::default(),
            memory: InMemoryMemoryRepository::default(),
            reducer: SemanticReducer::default(),
        }
    }
    pub fn append_user_message(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        text: &str,
    ) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let writer = self
            .execution
            .acquire_writer(thread_id, "semantic-kernel")
            .map_err(|e| e.to_string())?;
        let event = AgentEventEnvelope::new(
            thread_id,
            Some(turn_id),
            None,
            item_id,
            AgentEventV1::Message(MessageItem {
                role: MessageRole::User,
                text: text.into(),
                content_hash: None,
            }),
            self.execution
                .records(thread_id)
                .map_or(1, |r| r.len() as u64 + 1),
        );
        self.execution
            .append(writer, event)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn ingest_memory(&mut self, extraction: DualChannelExtraction) -> Result<usize, String> {
        if !self.enabled {
            return Ok(0);
        }
        self.memory
            .ingest_channels(extraction)
            .map_err(|e| e.to_string())
    }
    pub fn propose_assertion(
        &mut self,
        assertion: SemanticAssertion,
    ) -> Result<ReductionOutcome, String> {
        if !self.enabled {
            return Ok(ReductionOutcome {
                snapshot: self.snapshot.clone(),
                ..ReductionOutcome::default()
            });
        }
        let result = self
            .reducer
            .apply(
                &self.snapshot,
                ProposedStateDelta::upsert(assertion),
                &self.evidence,
            )
            .map_err(|e| e.to_string())?;
        self.snapshot = result.snapshot.clone();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_bridge_is_a_noop() {
        let mut bridge = SemanticKernelBridge::new(false);
        bridge
            .append_user_message("t", "turn", "item", "hello")
            .unwrap();
        assert!(bridge.execution.records("t").is_none());
    }
    #[test]
    fn enabled_bridge_shadow_writes_execution_event() {
        let mut bridge = SemanticKernelBridge::new(true);
        bridge
            .append_user_message("t", "turn", "item", "hello")
            .unwrap();
        assert_eq!(bridge.execution.records("t").unwrap().len(), 1);
    }
}
