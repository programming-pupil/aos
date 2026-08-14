//! Deterministic provider replay and fault-injection TCK primitives.
//! Production traces are never implicitly fixtures: callers must explicitly
//! redact/export a script and use `assert_consumed`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRequestFixture {
    pub script_key: String,
    pub canonical_request_hash: String,
    pub frames: Vec<ProviderFrame>,
    pub expected_tool_calls: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderFrame {
    FirstChunkError(String),
    Chunk(String),
    Disconnect,
    Hang,
    Timeout,
    Duplicate(String),
    Done,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeIntent,
    AfterIntentBeforeRemote,
    AfterRemoteBeforeOutcome,
    FirstChunk,
    PartialStream,
    Timeout,
    DuplicateOutcome,
    Cancel,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultScript {
    pub seed: u64,
    pub points: Vec<FaultPoint>,
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReplayError {
    #[error("canonical request hash mismatch: expected {expected}, actual {actual}")]
    RequestHashMismatch { expected: String, actual: String },
    #[error("replay fixture still has {0} unconsumed frame(s)")]
    UnconsumedFrames(usize),
    #[error("unexpected call: {0}")]
    UnexpectedCall(String),
    #[error("fixture is not explicitly marked safe for replay")]
    UnsafeFixture,
}

#[derive(Debug, Clone)]
pub struct ProviderReplay {
    fixture: ProviderRequestFixture,
    cursor: usize,
    consumed_tools: BTreeMap<String, u32>,
}
impl ProviderReplay {
    pub fn new(
        fixture: ProviderRequestFixture,
        canonical_request: &serde_json::Value,
    ) -> Result<Self, ReplayError> {
        let actual = canonical_hash(canonical_request);
        if actual != fixture.canonical_request_hash {
            return Err(ReplayError::RequestHashMismatch {
                expected: fixture.canonical_request_hash,
                actual,
            });
        }
        Ok(Self {
            fixture,
            cursor: 0,
            consumed_tools: BTreeMap::new(),
        })
    }
    pub fn next(&mut self) -> Option<ProviderFrame> {
        let frame = self.fixture.frames.get(self.cursor).cloned();
        if frame.is_some() {
            self.cursor += 1;
        }
        frame
    }
    pub fn record_tool_call(&mut self, name: impl Into<String>) -> Result<(), ReplayError> {
        let name = name.into();
        if !self.fixture.expected_tool_calls.contains(&name) {
            return Err(ReplayError::UnexpectedCall(name));
        }
        *self.consumed_tools.entry(name).or_default() += 1;
        Ok(())
    }
    pub fn assert_consumed(&self) -> Result<(), ReplayError> {
        if self.cursor != self.fixture.frames.len() {
            return Err(ReplayError::UnconsumedFrames(
                self.fixture.frames.len() - self.cursor,
            ));
        }
        for expected in &self.fixture.expected_tool_calls {
            if self.consumed_tools.get(expected).copied().unwrap_or(0) == 0 {
                return Err(ReplayError::UnexpectedCall(format!(
                    "missing expected tool call {expected}"
                )));
            }
        }
        Ok(())
    }
}
pub fn canonical_hash(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).expect("json value is serializable");
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_requires_exact_request_and_consumption() {
        let request = serde_json::json!({"model":"fixture","input":"hello"});
        let fixture = ProviderRequestFixture {
            script_key: "parent/turn-1".into(),
            canonical_request_hash: canonical_hash(&request),
            frames: vec![ProviderFrame::Chunk("a".into()), ProviderFrame::Done],
            expected_tool_calls: vec!["search".into()],
        };
        let mut replay = ProviderReplay::new(fixture, &request).unwrap();
        assert!(replay.assert_consumed().is_err());
        assert!(matches!(replay.next(), Some(ProviderFrame::Chunk(_))));
        replay.next();
        replay.record_tool_call("search").unwrap();
        assert!(replay.assert_consumed().is_ok());
    }
    #[test]
    fn replay_rejects_missing_or_extra_calls() {
        let request = serde_json::json!({"x":1});
        let fixture = ProviderRequestFixture {
            script_key: "child/1".into(),
            canonical_request_hash: canonical_hash(&request),
            frames: vec![],
            expected_tool_calls: vec!["read".into()],
        };
        let mut replay = ProviderReplay::new(fixture, &request).unwrap();
        assert!(replay.record_tool_call("write").is_err());
        assert!(replay.assert_consumed().is_err());
    }
}
