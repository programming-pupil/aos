use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

pub type EventId = String;
pub type ThreadId = String;
pub type TurnId = String;
pub type StepId = String;
pub type ItemId = String;
pub type BatchId = String;
pub type ArtifactId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventActor {
    User { user_id: String },
    Model { model: String },
    Tool { name: String },
    System,
    Worker { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThreadEvent {
    Created,
    Suspended { reason: String },
    Resumed,
    Forked { parent_thread_id: ThreadId },
    Closed { outcome: TerminalOutcome },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TurnEvent {
    Started,
    Completed { outcome: TerminalOutcome },
    Cancelled { reason: String },
    Suspended { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageItem {
    pub role: MessageRole,
    pub text: String,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolInvocationEvent {
    pub invocation_id: String,
    pub tool_name: String,
    pub state: String,
    pub idempotency_key: String,
    pub capability_token_id: Option<String>,
    pub artifact_id: Option<ArtifactId>,
    pub outcome: Option<ToolOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolOutcome {
    Completed,
    Failed,
    Cancelled,
    Expired,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalEvent {
    pub request_id: String,
    pub tool_name: String,
    pub scope_hash: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEvent {
    pub artifact_id: ArtifactId,
    pub content_hash: String,
    pub bytes: u64,
    pub media_type: String,
    pub locator: String,
    pub omitted_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEvent {
    pub assertion_ids: Vec<String>,
    pub source_event_ids: Vec<EventId>,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointEvent {
    pub checkpoint_id: String,
    pub source_event_seqs: Vec<u64>,
    pub state_hash: String,
    pub durable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildThreadEvent {
    pub child_thread_id: ThreadId,
    pub parent_thread_id: ThreadId,
    pub spawn_item_id: ItemId,
    pub settlement: Option<ChildSettlement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChildSettlement {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainEvent {
    pub domain: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentEventV1 {
    Thread(ThreadEvent),
    Turn(TurnEvent),
    Message(MessageItem),
    Tool(ToolInvocationEvent),
    Approval(ApprovalEvent),
    Artifact(ArtifactEvent),
    Memory(MemoryEvent),
    Checkpoint(CheckpointEvent),
    ChildThread(ChildThreadEvent),
    Domain(DomainEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalOutcome {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEventEnvelope {
    pub event_id: EventId,
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub step_id: Option<StepId>,
    pub item_id: ItemId,
    pub parent_item_id: Option<ItemId>,
    pub sequence: u64,
    pub occurred_at: DateTime<Utc>,
    pub actor: EventActor,
    pub event: AgentEventV1,
    pub idempotency_key: Option<String>,
    pub source_event_ids: Vec<EventId>,
    pub semantic_snapshot_version: Option<u64>,
    pub schema_version: u32,
    pub batch_id: BatchId,
    pub payload_hash: String,
}

impl AgentEventEnvelope {
    pub fn new(
        thread_id: impl Into<String>,
        turn_id: Option<&str>,
        step_id: Option<&str>,
        item_id: impl Into<String>,
        event: AgentEventV1,
        sequence: u64,
    ) -> Self {
        let mut envelope = Self {
            event_id: Uuid::new_v4().to_string(),
            thread_id: thread_id.into(),
            turn_id: turn_id.map(str::to_owned),
            step_id: step_id.map(str::to_owned),
            item_id: item_id.into(),
            parent_item_id: None,
            sequence,
            occurred_at: Utc::now(),
            actor: EventActor::System,
            event,
            idempotency_key: None,
            source_event_ids: Vec::new(),
            semantic_snapshot_version: None,
            schema_version: 1,
            batch_id: Uuid::new_v4().to_string(),
            payload_hash: String::new(),
        };
        envelope.payload_hash = envelope
            .compute_payload_hash()
            .expect("event serialization cannot fail");
        envelope
    }

    pub fn compute_payload_hash(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct HashInput<'a> {
            thread_id: &'a str,
            turn_id: &'a Option<String>,
            step_id: &'a Option<String>,
            item_id: &'a str,
            parent_item_id: &'a Option<String>,
            sequence: u64,
            actor: &'a EventActor,
            event: &'a AgentEventV1,
            idempotency_key: &'a Option<String>,
            source_event_ids: &'a [EventId],
            semantic_snapshot_version: Option<u64>,
            schema_version: u32,
            batch_id: &'a str,
        }
        let value = serde_json::to_vec(&HashInput {
            thread_id: &self.thread_id,
            turn_id: &self.turn_id,
            step_id: &self.step_id,
            item_id: &self.item_id,
            parent_item_id: &self.parent_item_id,
            sequence: self.sequence,
            actor: &self.actor,
            event: &self.event,
            idempotency_key: &self.idempotency_key,
            source_event_ids: &self.source_event_ids,
            semantic_snapshot_version: self.semantic_snapshot_version,
            schema_version: self.schema_version,
            batch_id: &self.batch_id,
        })?;
        Ok(hex::encode(Sha256::digest(value)))
    }

    pub fn verify_hash(&self) -> Result<(), ProtocolError> {
        let actual = self
            .compute_payload_hash()
            .map_err(ProtocolError::Serialization)?;
        if actual != self.payload_hash {
            return Err(ProtocolError::PayloadHashMismatch {
                expected: self.payload_hash.clone(),
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("payload hash mismatch: expected {expected}, actual {actual}")]
    PayloadHashMismatch { expected: String, actual: String },
    #[error("serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorCapabilities {
    pub name: String,
    pub version: String,
    pub streaming: bool,
    pub fork: bool,
    pub approval: bool,
    pub suspend_resume: bool,
    pub tool_results: bool,
    pub memory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorStartRequest {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub objective: String,
    pub capability_snapshot: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorHandle {
    pub id: String,
    pub executor: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorInput {
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorCheckpoint {
    pub handle: ExecutorHandle,
    pub sequence: u64,
    pub state_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InterruptOutcome {
    Interrupted,
    AlreadyCompleted,
    NotFound,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutorEvent {
    pub envelope: AgentEventEnvelope,
    pub native_event_ref: Option<String>,
}
pub type ExecutorEventStream = Vec<ExecutorEvent>;

#[async_trait]
pub trait AgentExecutorAdapter: Send + Sync {
    fn capabilities(&self) -> ExecutorCapabilities;
    async fn start(&self, request: ExecutorStartRequest) -> Result<ExecutorHandle, ProtocolError>;
    async fn append_input(
        &self,
        handle: &ExecutorHandle,
        input: ExecutorInput,
    ) -> Result<(), ProtocolError>;
    async fn interrupt(&self, handle: &ExecutorHandle) -> Result<InterruptOutcome, ProtocolError>;
    async fn resume(&self, checkpoint: ExecutorCheckpoint)
        -> Result<ExecutorHandle, ProtocolError>;
    async fn stream_events(
        &self,
        handle: &ExecutorHandle,
    ) -> Result<ExecutorEventStream, ProtocolError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBudgetManifest {
    pub max_tokens: u64,
    pub used_tokens: u64,
    pub reserved_tokens: u64,
    pub blocks: Vec<ContextBlockManifest>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBlockManifest {
    pub block_id: String,
    pub source: String,
    pub tokens: u64,
    pub truncated: bool,
    pub source_hash: String,
    pub policy_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactExcerpt {
    pub artifact_id: ArtifactId,
    pub preview: String,
    pub content_hash: String,
    pub omitted_bytes: u64,
    pub next_page: Option<String>,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionCheckpoint {
    pub checkpoint_id: String,
    pub source_event_seqs: Vec<u64>,
    pub narrative_summary: String,
    pub continuity_state: serde_json::Value,
    pub unresolved_questions: Vec<String>,
    pub artifact_refs: Vec<String>,
    pub exact_archive_refs: Vec<String>,
    pub retained_recent_event_seqs: Vec<u64>,
    pub input_tokens_estimated: u64,
    pub output_tokens_estimated: u64,
    pub extractor_version: String,
    pub prompt_version: String,
}
