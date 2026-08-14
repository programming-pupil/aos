use crate::{AgentEventEnvelope, EventId, ThreadId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerRecord {
    pub event: AgentEventEnvelope,
    pub committed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppendReceipt {
    pub event_id: EventId,
    pub sequence: u64,
    pub deduplicated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CorruptionKind {
    PayloadHash,
    SequenceGap,
    UnknownRequiredEvent,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("writer lease is stale or missing for thread {0}")]
    StaleWriter(ThreadId),
    #[error("expected sequence {expected}, got {actual}")]
    Sequence { expected: u64, actual: u64 },
    #[error("event payload is invalid: {0}")]
    InvalidPayload(String),
    #[error("idempotency key belongs to another event")]
    IdempotencyCollision,
    #[error("ledger corruption in thread {thread_id} at sequence {sequence}: {kind:?}")]
    Corruption {
        thread_id: ThreadId,
        sequence: u64,
        kind: CorruptionKind,
    },
    #[error("thread not found: {0}")]
    ThreadNotFound(ThreadId),
}

#[derive(Debug, Clone)]
struct WriterLease {
    worker: String,
    fencing: u64,
}
#[derive(Debug, Clone, Default)]
struct ThreadLog {
    records: Vec<LedgerRecord>,
    lease: Option<WriterLease>,
    next_fencing: u64,
}

#[derive(Debug, Clone, Default)]
pub struct EventLedger {
    threads: BTreeMap<ThreadId, ThreadLog>,
    idempotency: HashMap<(ThreadId, String), EventId>,
}

impl EventLedger {
    pub fn acquire_writer(
        &mut self,
        thread_id: impl Into<ThreadId>,
        worker: impl Into<String>,
    ) -> Result<WriterHandle, LedgerError> {
        let thread_id = thread_id.into();
        let log = self.threads.entry(thread_id.clone()).or_default();
        log.next_fencing += 1;
        let lease = WriterLease {
            worker: worker.into(),
            fencing: log.next_fencing,
        };
        log.lease = Some(lease.clone());
        Ok(WriterHandle {
            thread_id,
            worker: lease.worker,
            fencing: lease.fencing,
        })
    }
    pub fn append(
        &mut self,
        handle: WriterHandle,
        event: AgentEventEnvelope,
    ) -> Result<AppendReceipt, LedgerError> {
        let log = self
            .threads
            .get_mut(&handle.thread_id)
            .ok_or_else(|| LedgerError::ThreadNotFound(handle.thread_id.clone()))?;
        let lease = log
            .lease
            .as_ref()
            .filter(|l| l.worker == handle.worker && l.fencing == handle.fencing)
            .ok_or_else(|| LedgerError::StaleWriter(handle.thread_id.clone()))?;
        let _ = lease;
        if let Some(key) = &event.idempotency_key {
            if let Some(existing) = self
                .idempotency
                .get(&(handle.thread_id.clone(), key.clone()))
            {
                if existing == &event.event_id {
                    return Ok(AppendReceipt {
                        event_id: existing.clone(),
                        sequence: event.sequence,
                        deduplicated: true,
                    });
                }
                return Err(LedgerError::IdempotencyCollision);
            }
        }
        let expected = log.records.last().map_or(1, |r| r.event.sequence + 1);
        if event.sequence != expected {
            return Err(LedgerError::Sequence {
                expected,
                actual: event.sequence,
            });
        }
        event
            .verify_hash()
            .map_err(|e| LedgerError::InvalidPayload(e.to_string()))?;
        let receipt = AppendReceipt {
            event_id: event.event_id.clone(),
            sequence: event.sequence,
            deduplicated: false,
        };
        if let Some(key) = &event.idempotency_key {
            self.idempotency.insert(
                (handle.thread_id.clone(), key.clone()),
                event.event_id.clone(),
            );
        }
        log.records.push(LedgerRecord {
            event,
            committed: true,
        });
        Ok(receipt)
    }
    #[cfg(test)]
    pub fn append_uncommitted_for_test(
        &mut self,
        handle: WriterHandle,
        event: AgentEventEnvelope,
    ) -> Result<(), LedgerError> {
        let log = self
            .threads
            .get_mut(&handle.thread_id)
            .ok_or_else(|| LedgerError::ThreadNotFound(handle.thread_id.clone()))?;
        if log
            .lease
            .as_ref()
            .filter(|l| l.worker == handle.worker && l.fencing == handle.fencing)
            .is_none()
        {
            return Err(LedgerError::StaleWriter(handle.thread_id));
        }
        log.records.push(LedgerRecord {
            event,
            committed: false,
        });
        Ok(())
    }
    pub fn records(&self, thread_id: &str) -> Option<Vec<LedgerRecord>> {
        self.threads.get(thread_id).map(|l| l.records.clone())
    }
    pub fn repair(&mut self, thread_id: &str) -> Result<usize, LedgerError> {
        let log = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| LedgerError::ThreadNotFound(thread_id.into()))?;
        let mut valid = 0usize;
        for (idx, record) in log.records.iter().enumerate() {
            let expected = idx as u64 + 1;
            if record.event.sequence != expected {
                return Err(LedgerError::Corruption {
                    thread_id: thread_id.into(),
                    sequence: record.event.sequence,
                    kind: CorruptionKind::SequenceGap,
                });
            }
            if record.event.schema_version != 1 {
                if idx + 1 == log.records.len() && !record.committed {
                    break;
                }
                return Err(LedgerError::Corruption {
                    thread_id: thread_id.into(),
                    sequence: record.event.sequence,
                    kind: CorruptionKind::UnknownRequiredEvent,
                });
            }
            if record.event.verify_hash().is_err() {
                if idx + 1 == log.records.len() && !record.committed {
                    break;
                }
                return Err(LedgerError::Corruption {
                    thread_id: thread_id.into(),
                    sequence: record.event.sequence,
                    kind: CorruptionKind::PayloadHash,
                });
            }
            valid += 1;
        }
        if valid < log.records.len() {
            log.records.truncate(valid);
        }
        Ok(valid)
    }
    pub fn corrupt_for_test(
        &mut self,
        thread_id: &str,
        index: usize,
        kind: CorruptionKind,
    ) -> Result<(), LedgerError> {
        let log = self
            .threads
            .get_mut(thread_id)
            .ok_or_else(|| LedgerError::ThreadNotFound(thread_id.into()))?;
        let record = log
            .records
            .get_mut(index)
            .ok_or_else(|| LedgerError::ThreadNotFound(thread_id.into()))?;
        match kind {
            CorruptionKind::PayloadHash => record.event.payload_hash = "corrupt".into(),
            CorruptionKind::SequenceGap => record.event.sequence += 2,
            CorruptionKind::UnknownRequiredEvent => record.event.schema_version = u32::MAX,
        };
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterHandle {
    thread_id: ThreadId,
    worker: String,
    fencing: u64,
}
