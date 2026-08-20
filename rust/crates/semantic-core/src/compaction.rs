use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionCandidate {
    pub source_event_seqs: Vec<u64>,
    pub narrative_summary: String,
    pub continuity_state: serde_json::Value,
    pub retained_event_seqs: Vec<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub complete: bool,
    pub cuts_tool_transaction: bool,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompactionError {
    #[error("compaction output is incomplete; checkpoint is fail-closed")]
    Incomplete,
    #[error("compaction does not reduce the replaced context: input {input}, output {output}")]
    NotSmaller { input: u64, output: u64 },
    #[error("compaction source coverage is empty")]
    MissingSourceCoverage,
    #[error("compaction cuts a tool transaction boundary")]
    ToolTransactionBoundary,
}
#[derive(Debug, Default)]
pub struct CompactionValidator;
impl CompactionValidator {
    pub fn validate(&self, candidate: &CompactionCandidate) -> Result<(), CompactionError> {
        if !candidate.complete {
            return Err(CompactionError::Incomplete);
        }
        if candidate.source_event_seqs.is_empty() {
            return Err(CompactionError::MissingSourceCoverage);
        }
        if candidate.output_tokens >= candidate.input_tokens {
            return Err(CompactionError::NotSmaller {
                input: candidate.input_tokens,
                output: candidate.output_tokens,
            });
        }
        if candidate.cuts_tool_transaction {
            return Err(CompactionError::ToolTransactionBoundary);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compaction_is_fail_closed_and_requires_real_reduction() {
        let validator = CompactionValidator;
        let candidate = CompactionCandidate {
            source_event_seqs: vec![1, 2],
            narrative_summary: "summary".into(),
            continuity_state: serde_json::json!({"next":"verify"}),
            retained_event_seqs: vec![3],
            input_tokens: 100,
            output_tokens: 40,
            complete: true,
            cuts_tool_transaction: false,
        };
        assert!(validator.validate(&candidate).is_ok());
        let mut bad = candidate.clone();
        bad.complete = false;
        assert_eq!(validator.validate(&bad), Err(CompactionError::Incomplete));
        bad.complete = true;
        bad.output_tokens = 100;
        assert!(matches!(
            validator.validate(&bad),
            Err(CompactionError::NotSmaller { .. })
        ));
    }
}
