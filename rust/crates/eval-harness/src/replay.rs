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
    #[serde(default)]
    pub safe_for_replay: bool,
    pub canonical_request_hash: String,
    pub frames: Vec<ProviderFrame>,
    pub expected_tool_calls: Vec<String>,
    #[serde(default)]
    pub fault_script: Option<FaultScript>,
    #[serde(default)]
    pub expected_terminal_hash: Option<String>,
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
    #[error("replay fixture still has {0} unconsumed fault point(s)")]
    UnconsumedFaults(usize),
    #[error("terminal projection mismatch: expected {expected}, actual {actual:?}")]
    TerminalMismatch {
        expected: String,
        actual: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderReplay {
    fixture: ProviderRequestFixture,
    cursor: usize,
    consumed_tools: BTreeMap<String, u32>,
    remaining_faults: Vec<FaultPoint>,
    terminal_hash: Option<String>,
}
impl ProviderReplay {
    pub fn new(
        fixture: ProviderRequestFixture,
        canonical_request: &serde_json::Value,
    ) -> Result<Self, ReplayError> {
        if !fixture.safe_for_replay {
            return Err(ReplayError::UnsafeFixture);
        }
        let actual = canonical_hash(canonical_request);
        if actual != fixture.canonical_request_hash {
            return Err(ReplayError::RequestHashMismatch {
                expected: fixture.canonical_request_hash,
                actual,
            });
        }
        let remaining_faults = fixture
            .fault_script
            .as_ref()
            .map(|script| script.points.clone())
            .unwrap_or_default();
        Ok(Self {
            fixture,
            cursor: 0,
            consumed_tools: BTreeMap::new(),
            remaining_faults,
            terminal_hash: None,
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
        let expected = self
            .fixture
            .expected_tool_calls
            .iter()
            .filter(|candidate| *candidate == &name)
            .count() as u32;
        let consumed = self.consumed_tools.get(&name).copied().unwrap_or_default();
        if expected == 0 || consumed >= expected {
            return Err(ReplayError::UnexpectedCall(name));
        }
        *self.consumed_tools.entry(name).or_default() += 1;
        Ok(())
    }

    pub fn record_terminal_projection(&mut self, projection: &serde_json::Value) {
        self.terminal_hash = Some(canonical_hash(projection));
    }

    /// Consume one declared fault at the point where the runtime injected it.
    /// A fault script is metadata until the runtime explicitly acknowledges it;
    /// this prevents a test from passing merely because a fixture listed a
    /// timeout that never actually occurred.
    pub fn consume_fault(&mut self, point: FaultPoint) -> bool {
        let Some(index) = self
            .remaining_faults
            .iter()
            .position(|candidate| *candidate == point)
        else {
            return false;
        };
        self.remaining_faults.remove(index);
        true
    }
    pub fn assert_consumed(&self) -> Result<(), ReplayError> {
        if self.cursor != self.fixture.frames.len() {
            return Err(ReplayError::UnconsumedFrames(
                self.fixture.frames.len() - self.cursor,
            ));
        }
        let expected_counts = self.fixture.expected_tool_calls.iter().fold(
            BTreeMap::<String, u32>::new(),
            |mut counts, expected| {
                *counts.entry(expected.clone()).or_default() += 1;
                counts
            },
        );
        for (expected, count) in expected_counts {
            if self.consumed_tools.get(&expected).copied().unwrap_or(0) != count {
                return Err(ReplayError::UnexpectedCall(format!(
                    "missing expected tool call {expected}"
                )));
            }
        }
        if !self.remaining_faults.is_empty() {
            return Err(ReplayError::UnconsumedFaults(self.remaining_faults.len()));
        }
        if let Some(expected) = self.fixture.expected_terminal_hash.as_ref() {
            if self.terminal_hash.as_ref() != Some(expected) {
                return Err(ReplayError::TerminalMismatch {
                    expected: expected.clone(),
                    actual: self.terminal_hash.clone(),
                });
            }
        }
        Ok(())
    }
}
pub fn canonical_hash(value: &serde_json::Value) -> String {
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                // `serde_json::Map` preserves insertion order when the
                // workspace enables `preserve_order`. Provider request hashes
                // must be independent of construction order, so sort keys
                // explicitly instead of relying on the map implementation.
                let sorted = map.iter().collect::<BTreeMap<_, _>>();
                let mut ordered = serde_json::Map::new();
                for (key, value) in sorted {
                    ordered.insert(key.clone(), canonical(value));
                }
                serde_json::Value::Object(ordered)
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(canonical).collect())
            }
            scalar => scalar.clone(),
        }
    }
    let bytes = serde_json::to_vec(&canonical(value)).expect("json value is serializable");
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
            safe_for_replay: true,
            canonical_request_hash: canonical_hash(&request),
            frames: vec![ProviderFrame::Chunk("a".into()), ProviderFrame::Done],
            expected_tool_calls: vec!["search".into()],
            fault_script: None,
            expected_terminal_hash: None,
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
            safe_for_replay: true,
            canonical_request_hash: canonical_hash(&request),
            frames: vec![],
            expected_tool_calls: vec!["read".into()],
            fault_script: None,
            expected_terminal_hash: None,
        };
        let mut replay = ProviderReplay::new(fixture, &request).unwrap();
        assert!(replay.record_tool_call("write").is_err());
        assert!(replay.assert_consumed().is_err());
    }

    #[test]
    fn canonical_request_hash_ignores_object_insertion_order() {
        let first = serde_json::json!({"z": 1, "a": {"y": true, "b": [3, 2]}});
        let second = serde_json::json!({"a": {"b": [3, 2], "y": true}, "z": 1});
        assert_eq!(canonical_hash(&first), canonical_hash(&second));
    }

    #[test]
    fn declared_faults_must_be_consumed() {
        let request = serde_json::json!({"model": "fixture"});
        let fixture = ProviderRequestFixture {
            script_key: "faults/1".into(),
            safe_for_replay: true,
            canonical_request_hash: canonical_hash(&request),
            frames: vec![ProviderFrame::Timeout],
            expected_tool_calls: vec![],
            fault_script: Some(FaultScript {
                seed: 7,
                points: vec![FaultPoint::Timeout, FaultPoint::Cancel],
            }),
            expected_terminal_hash: None,
        };
        let mut replay = ProviderReplay::new(fixture, &request).unwrap();
        assert!(replay.next().is_some());
        assert!(replay.consume_fault(FaultPoint::Timeout));
        assert!(matches!(
            replay.assert_consumed(),
            Err(ReplayError::UnconsumedFaults(1))
        ));
        assert!(replay.consume_fault(FaultPoint::Cancel));
        assert!(replay.assert_consumed().is_ok());
    }

    #[test]
    fn replay_rejects_unsafe_fixtures_duplicate_effects_and_wrong_terminal() {
        let request = serde_json::json!({"model": "fixture"});
        let terminal = serde_json::json!({"status": "completed", "effects": 1});
        let fixture = ProviderRequestFixture {
            script_key: "strict/1".into(),
            safe_for_replay: false,
            canonical_request_hash: canonical_hash(&request),
            frames: vec![],
            expected_tool_calls: vec!["write:invocation-1".into()],
            fault_script: None,
            expected_terminal_hash: Some(canonical_hash(&terminal)),
        };
        assert!(matches!(
            ProviderReplay::new(fixture.clone(), &request),
            Err(ReplayError::UnsafeFixture)
        ));
        let mut fixture = fixture;
        fixture.safe_for_replay = true;
        let mut replay = ProviderReplay::new(fixture, &request).unwrap();
        replay.record_tool_call("write:invocation-1").unwrap();
        assert!(replay.record_tool_call("write:invocation-1").is_err());
        replay.record_terminal_projection(&serde_json::json!({"status": "failed"}));
        assert!(matches!(
            replay.assert_consumed(),
            Err(ReplayError::TerminalMismatch { .. })
        ));
        replay.record_terminal_projection(&terminal);
        assert!(replay.assert_consumed().is_ok());
    }
}
