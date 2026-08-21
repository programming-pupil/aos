use crate::AgentEventEnvelope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Provider-neutral role in the canonical model-visible message surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Provider-neutral content retained by the canonical surface. Keeping tool
/// calls and results typed prevents adapters from silently flattening their
/// pairing into prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SurfaceBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    ToolCall {
        invocation_id: String,
        tool_name: String,
        input: String,
    },
    ToolResult {
        invocation_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    Image {
        media_type: String,
        source_type: String,
        data: String,
    },
    Document {
        media_type: String,
        source_type: String,
        data: String,
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceMessage {
    pub message_id: String,
    pub role: SurfaceRole,
    pub blocks: Vec<SurfaceBlock>,
}

impl SurfaceMessage {
    #[must_use]
    pub fn model_view(&self) -> ModelSurfaceMessage {
        ModelSurfaceMessage {
            role: self.role,
            blocks: self.blocks.clone(),
        }
    }
}

/// The exact provider-neutral message shape used for request equality. Message
/// IDs and provenance are intentionally excluded: providers see content, while
/// the full surface hash below proves where that content came from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSurfaceMessage {
    pub role: SurfaceRole,
    pub blocks: Vec<SurfaceBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SurfaceOperation {
    Append {
        message: SurfaceMessage,
    },
    Replace {
        /// Exact provider-neutral messages that replace the selected range.
        /// A vector is required because context compilation and native
        /// compaction may replace one history range with multiple retained
        /// messages. Every emitted node shares this operation's event sequence
        /// and flattened provenance.
        messages: Vec<SurfaceMessage>,
        /// Event sequences of the current surface nodes being replaced. They
        /// must form one non-empty contiguous range in the surface at this
        /// event's sequence.
        source_event_sequences: Vec<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceNode {
    pub event_sequence: u64,
    pub message: SurfaceMessage,
    /// Flattened source provenance. Append nodes contain their own sequence;
    /// replacement nodes contain every original source sequence they shadow.
    pub source_event_sequences: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalSurface {
    pub nodes: Vec<SurfaceNode>,
    pub ledger_tail_sequence: u64,
    pub surface_hash: String,
    pub model_messages_hash: String,
}

impl CanonicalSurface {
    #[must_use]
    pub fn model_messages(&self) -> Vec<ModelSurfaceMessage> {
        self.nodes
            .iter()
            .map(|node| node.message.model_view())
            .collect()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SurfaceError {
    #[error("surface ledger sequence gap: expected {expected}, got {actual}")]
    Sequence { expected: u64, actual: u64 },
    #[error("surface message id is empty")]
    EmptyMessageId,
    #[error("surface message {0} has no content blocks")]
    EmptyMessage(String),
    #[error("surface message id is already active: {0}")]
    DuplicateMessageId(String),
    #[error("surface replacement source is empty")]
    EmptyReplacement,
    #[error("surface replacement output is empty")]
    EmptyReplacementOutput,
    #[error("surface replacement sources must be strictly increasing")]
    UnorderedReplacement,
    #[error("surface replacement references a node that is not current: {0}")]
    MissingReplacementSource(u64),
    #[error("surface replacement sources are not contiguous")]
    NonContiguousReplacement,
    #[error("surface serialization failed: {0}")]
    Serialization(String),
    #[error("duplicate tool call invocation id: {0}")]
    DuplicateToolCall(String),
    #[error("tool result references an unknown invocation id: {0}")]
    UnknownToolResult(String),
    #[error("duplicate tool result invocation id: {0}")]
    DuplicateToolResult(String),
    #[error("tool result name does not match invocation {invocation_id}: expected {expected}, got {actual}")]
    ToolNameMismatch {
        invocation_id: String,
        expected: String,
        actual: String,
    },
    #[error("tool call has no result before provider dispatch: {0}")]
    UnresolvedToolCall(String),
}

fn validate_message(message: &SurfaceMessage) -> Result<(), SurfaceError> {
    if message.message_id.trim().is_empty() {
        return Err(SurfaceError::EmptyMessageId);
    }
    if message.blocks.is_empty() {
        return Err(SurfaceError::EmptyMessage(message.message_id.clone()));
    }
    Ok(())
}

fn sha256_json<T: Serialize + ?Sized>(value: &T) -> Result<String, SurfaceError> {
    serde_json::to_vec(value)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|error| SurfaceError::Serialization(error.to_string()))
}

/// Hash a provider request after converting it to [`ModelSurfaceMessage`].
/// Dispatch adapters compare this value with `CanonicalSurface::model_messages_hash`
/// and fail closed on any shadow-history mismatch.
pub fn hash_model_messages(messages: &[ModelSurfaceMessage]) -> Result<String, SurfaceError> {
    sha256_json(messages)
}

/// Validate the tool lifecycle in a provider-bound surface. Intermediate
/// ledger states may contain a just-emitted call, but a new model dispatch may
/// never contain an unknown, duplicate, mismatched, or unresolved invocation.
pub fn validate_model_messages(messages: &[ModelSurfaceMessage]) -> Result<(), SurfaceError> {
    let mut calls = BTreeMap::<String, String>::new();
    let mut results = BTreeSet::<String>::new();
    for message in messages {
        for block in &message.blocks {
            match block {
                SurfaceBlock::ToolCall {
                    invocation_id,
                    tool_name,
                    ..
                } => {
                    if calls
                        .insert(invocation_id.clone(), tool_name.clone())
                        .is_some()
                    {
                        return Err(SurfaceError::DuplicateToolCall(invocation_id.clone()));
                    }
                }
                SurfaceBlock::ToolResult {
                    invocation_id,
                    tool_name,
                    ..
                } => {
                    let expected = calls
                        .get(invocation_id)
                        .ok_or_else(|| SurfaceError::UnknownToolResult(invocation_id.clone()))?;
                    if !tool_name.is_empty() && expected != tool_name {
                        return Err(SurfaceError::ToolNameMismatch {
                            invocation_id: invocation_id.clone(),
                            expected: expected.clone(),
                            actual: tool_name.clone(),
                        });
                    }
                    if !results.insert(invocation_id.clone()) {
                        return Err(SurfaceError::DuplicateToolResult(invocation_id.clone()));
                    }
                }
                SurfaceBlock::Text { .. }
                | SurfaceBlock::Thinking { .. }
                | SurfaceBlock::Image { .. }
                | SurfaceBlock::Document { .. } => {}
            }
        }
    }
    if let Some(unresolved) = calls.keys().find(|id| !results.contains(*id)) {
        return Err(SurfaceError::UnresolvedToolCall(unresolved.clone()));
    }
    Ok(())
}

/// Deterministically fold append/replace operations from an append-only event
/// ledger. Events without a surface operation remain part of the ledger but do
/// not affect model-visible history.
pub fn fold_surface(events: &[AgentEventEnvelope]) -> Result<CanonicalSurface, SurfaceError> {
    let mut nodes = Vec::<SurfaceNode>::new();
    let mut expected_sequence = 1_u64;
    for event in events {
        if event.sequence != expected_sequence {
            return Err(SurfaceError::Sequence {
                expected: expected_sequence,
                actual: event.sequence,
            });
        }
        expected_sequence = expected_sequence.saturating_add(1);
        let Some(operation) = event.surface_op.as_ref() else {
            continue;
        };
        match operation {
            SurfaceOperation::Append { message } => {
                validate_message(message)?;
                if nodes
                    .iter()
                    .any(|node| node.message.message_id == message.message_id)
                {
                    return Err(SurfaceError::DuplicateMessageId(message.message_id.clone()));
                }
                nodes.push(SurfaceNode {
                    event_sequence: event.sequence,
                    message: message.clone(),
                    source_event_sequences: vec![event.sequence],
                });
            }
            SurfaceOperation::Replace {
                messages,
                source_event_sequences,
            } => {
                if messages.is_empty() {
                    return Err(SurfaceError::EmptyReplacementOutput);
                }
                for message in messages {
                    validate_message(message)?;
                }
                let output_ids = messages
                    .iter()
                    .map(|message| message.message_id.as_str())
                    .collect::<BTreeSet<_>>();
                if output_ids.len() != messages.len() {
                    let duplicate = messages
                        .iter()
                        .find(|message| {
                            messages
                                .iter()
                                .filter(|candidate| candidate.message_id == message.message_id)
                                .count()
                                > 1
                        })
                        .expect("replacement contains a duplicate message id");
                    return Err(SurfaceError::DuplicateMessageId(
                        duplicate.message_id.clone(),
                    ));
                }
                if source_event_sequences.is_empty() {
                    return Err(SurfaceError::EmptyReplacement);
                }
                if source_event_sequences
                    .windows(2)
                    .any(|window| window[0] >= window[1])
                {
                    return Err(SurfaceError::UnorderedReplacement);
                }
                let requested = source_event_sequences
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let indexes = nodes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, node)| {
                        requested.contains(&node.event_sequence).then_some(index)
                    })
                    .collect::<Vec<_>>();
                let current = nodes
                    .iter()
                    .map(|node| node.event_sequence)
                    .collect::<BTreeSet<_>>();
                if let Some(missing) = requested.difference(&current).next().copied() {
                    return Err(SurfaceError::MissingReplacementSource(missing));
                }
                if indexes
                    .windows(2)
                    .any(|window| window[1] != window[0].saturating_add(1))
                {
                    return Err(SurfaceError::NonContiguousReplacement);
                }
                let first = indexes[0];
                let last = *indexes.last().expect("non-empty replacement indexes");
                if let Some(duplicate) = messages.iter().find(|message| {
                    nodes.iter().enumerate().any(|(index, node)| {
                        (index < first || index > last)
                            && node.message.message_id == message.message_id
                    })
                }) {
                    return Err(SurfaceError::DuplicateMessageId(
                        duplicate.message_id.clone(),
                    ));
                }
                let mut flattened = nodes[first..=last]
                    .iter()
                    .flat_map(|node| node.source_event_sequences.iter().copied())
                    .collect::<Vec<_>>();
                flattened.sort_unstable();
                flattened.dedup();
                let replacement_nodes = messages.iter().cloned().map(|message| SurfaceNode {
                    event_sequence: event.sequence,
                    message,
                    source_event_sequences: flattened.clone(),
                });
                nodes.splice(first..=last, replacement_nodes);
            }
        }
    }

    let ledger_tail_sequence = events.last().map_or(0, |event| event.sequence);
    let surface_hash = sha256_json(&nodes)?;
    let model_messages = nodes
        .iter()
        .map(|node| node.message.model_view())
        .collect::<Vec<_>>();
    let model_messages_hash = hash_model_messages(&model_messages)?;
    Ok(CanonicalSurface {
        nodes,
        ledger_tail_sequence,
        surface_hash,
        model_messages_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentEventV1, DomainEvent};

    fn event(sequence: u64, operation: SurfaceOperation) -> AgentEventEnvelope {
        let mut event = AgentEventEnvelope::new(
            "thread",
            Some("turn"),
            None,
            format!("item-{sequence}"),
            AgentEventV1::Domain(DomainEvent {
                domain: "test".into(),
                kind: "surface".into(),
                payload: serde_json::json!({}),
            }),
            sequence,
        );
        event.surface_op = Some(operation);
        event.payload_hash = event.compute_payload_hash().unwrap();
        event
    }

    fn message(id: &str, text: &str) -> SurfaceMessage {
        SurfaceMessage {
            message_id: id.into(),
            role: SurfaceRole::User,
            blocks: vec![SurfaceBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn exact_replacement_is_deterministic_and_preserves_flattened_sources() {
        let events = vec![
            event(
                1,
                SurfaceOperation::Append {
                    message: message("m1", "one"),
                },
            ),
            event(
                2,
                SurfaceOperation::Append {
                    message: message("m2", "two"),
                },
            ),
            event(
                3,
                SurfaceOperation::Append {
                    message: message("m3", "three"),
                },
            ),
            event(
                4,
                SurfaceOperation::Replace {
                    messages: vec![message("summary", "one and two")],
                    source_event_sequences: vec![1, 2],
                },
            ),
            event(
                5,
                SurfaceOperation::Replace {
                    messages: vec![message("summary-2", "all three")],
                    source_event_sequences: vec![3, 4],
                },
            ),
        ];
        let folded = fold_surface(&events).unwrap();
        assert_eq!(folded.nodes.len(), 1);
        assert_eq!(folded.nodes[0].source_event_sequences, vec![1, 2, 3]);
        assert_eq!(folded.nodes[0].message.message_id, "summary-2");
        assert_eq!(fold_surface(&events).unwrap(), folded);
    }

    #[test]
    fn replacement_rejects_shadowed_or_non_contiguous_sources() {
        let base = vec![
            event(
                1,
                SurfaceOperation::Append {
                    message: message("m1", "one"),
                },
            ),
            event(
                2,
                SurfaceOperation::Append {
                    message: message("m2", "two"),
                },
            ),
            event(
                3,
                SurfaceOperation::Append {
                    message: message("m3", "three"),
                },
            ),
        ];
        let mut non_contiguous = base.clone();
        non_contiguous.push(event(
            4,
            SurfaceOperation::Replace {
                messages: vec![message("summary", "bad")],
                source_event_sequences: vec![1, 3],
            },
        ));
        assert_eq!(
            fold_surface(&non_contiguous).unwrap_err(),
            SurfaceError::NonContiguousReplacement
        );

        let mut shadowed = base;
        shadowed.push(event(
            4,
            SurfaceOperation::Replace {
                messages: vec![message("summary", "one and two")],
                source_event_sequences: vec![1, 2],
            },
        ));
        shadowed.push(event(
            5,
            SurfaceOperation::Replace {
                messages: vec![message("bad", "bad")],
                source_event_sequences: vec![1],
            },
        ));
        assert_eq!(
            fold_surface(&shadowed).unwrap_err(),
            SurfaceError::MissingReplacementSource(1)
        );
    }

    #[test]
    fn provider_surface_requires_exact_tool_call_result_pairing() {
        let call = ModelSurfaceMessage {
            role: SurfaceRole::Assistant,
            blocks: vec![SurfaceBlock::ToolCall {
                invocation_id: "call-1".into(),
                tool_name: "read_file".into(),
                input: "{}".into(),
            }],
        };
        assert_eq!(
            validate_model_messages(std::slice::from_ref(&call)).unwrap_err(),
            SurfaceError::UnresolvedToolCall("call-1".into())
        );
        let result = ModelSurfaceMessage {
            role: SurfaceRole::Tool,
            blocks: vec![SurfaceBlock::ToolResult {
                invocation_id: "call-1".into(),
                tool_name: "read_file".into(),
                output: "ok".into(),
                is_error: false,
            }],
        };
        validate_model_messages(&[call.clone(), result.clone()]).unwrap();

        let mut mismatched = result;
        if let SurfaceBlock::ToolResult { tool_name, .. } = &mut mismatched.blocks[0] {
            *tool_name = "write_file".into();
        }
        assert!(matches!(
            validate_model_messages(&[call, mismatched]),
            Err(SurfaceError::ToolNameMismatch { .. })
        ));
    }
}
