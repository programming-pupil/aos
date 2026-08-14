use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvidenceSourceType {
    Message,
    File,
    ToolResult,
    DatabaseQuery,
    Human,
    Provider,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvidenceAuthority {
    User,
    Owner,
    Tool,
    Document,
    Model,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceRange {
    pub start: u64,
    pub end: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceRef {
    pub evidence_id: String,
    pub source_type: EvidenceSourceType,
    pub source_locator: String,
    pub content_hash: String,
    pub event_seq: Option<u64>,
    pub byte_or_line_range: Option<SourceRange>,
    pub collected_at: DateTime<Utc>,
    pub authority: EvidenceAuthority,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvidenceLedgerError {
    #[error("duplicate evidence id with a different content hash: {0}")]
    ConflictingDuplicate(String),
    #[error("replacement references event sequence {0}, which is not present")]
    MissingSourceEvent(u64),
    #[error("evidence {0} is not available in the ledger")]
    MissingEvidence(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EvidenceLedger {
    entries: BTreeMap<String, EvidenceRef>,
}
impl EvidenceLedger {
    pub fn append(&mut self, evidence: EvidenceRef) -> Result<(), EvidenceLedgerError> {
        if let Some(existing) = self.entries.get(&evidence.evidence_id) {
            if existing.content_hash != evidence.content_hash {
                return Err(EvidenceLedgerError::ConflictingDuplicate(
                    evidence.evidence_id,
                ));
            }
            return Ok(());
        }
        self.entries.insert(evidence.evidence_id.clone(), evidence);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<&EvidenceRef> {
        self.entries.get(id)
    }
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
    pub fn validate_source_coverage(&self, source_seqs: &[u64]) -> Result<(), EvidenceLedgerError> {
        for seq in source_seqs {
            if !self.entries.values().any(|e| e.event_seq == Some(*seq)) {
                return Err(EvidenceLedgerError::MissingSourceEvent(*seq));
            }
        }
        Ok(())
    }
    pub fn iter(&self) -> impl Iterator<Item = &EvidenceRef> {
        self.entries.values()
    }
}
