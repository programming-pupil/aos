//! AgentOps DTOs, status constants, and SQL projection snippets.
//!
//! Keeping these definitions outside the route implementation makes the
//! WatchDog control plane easier to review and extend.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUS_CREATED: &str = "created";
pub const STATUS_RUNNING: &str = "running";
#[allow(dead_code)]
pub const STATUS_WAITING_INPUT: &str = "waiting_input";
pub const STATUS_CANCELLING: &str = "cancelling";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELLED: &str = "cancelled";
pub const STATUS_STALE: &str = "stale";

pub const PHASE_INTAKE: &str = "intake";
#[allow(dead_code)]
pub const PHASE_CAPABILITY_MATCHING: &str = "capability_matching";
#[allow(dead_code)]
pub const PHASE_CONTEXT_LOADING: &str = "context_loading";
pub const PHASE_PLANNING: &str = "planning";
pub const PHASE_RETRIEVING: &str = "retrieving";
pub const PHASE_MODEL_CALLING: &str = "model_calling";
pub const PHASE_DEBATING: &str = "debating";
pub const PHASE_EXECUTING: &str = "executing";
pub const PHASE_VALIDATING: &str = "validating";
#[allow(dead_code)]
pub const PHASE_REPLYING: &str = "replying";
pub const PHASE_FINALIZING: &str = "finalizing";

pub(crate) const AGENT_TASK_SELECT: &str = r"
at.id, at.short_code, at.tenant_id, at.source, at.source_ref, at.source_label, at.capability_key,
at.agent_id, at.agent_name, at.status, at.phase, at.progress_percent, at.title,
at.summary, at.owner_user_id, at.initiator_user_id, at.visibility_scope, at.team_id,
at.correlation_id, at.parent_task_id, at.root_task_id, at.origin_session_id, at.origin_turn_id,
at.external_platform, at.external_channel_id, at.external_conversation_id,
at.external_message_id, at.linked_resource_type, at.linked_resource_id,
CAST(at.input_json AS TEXT) AS input_json, CAST(at.output_json AS TEXT) AS output_json,
at.state_version, at.desired_state, CAST(at.progress_json AS TEXT) AS progress_json,
at.result_summary, at.result_artifact_ref, at.sensitivity_label, at.archived,
at.error_code, at.error_message, at.last_event, at.last_heartbeat_at, at.started_at,
at.completed_at, at.queue_status, at.available_at, at.claimed_by, at.claimed_at,
at.lease_expires_at, at.attempt_count, at.max_attempts, at.idempotency_key, at.priority,
at.last_error, at.finished_at, at.dead_reason, at.created_at, at.updated_at,
ars.id AS runtime_session_id, ars.status AS runtime_status,
ars.workspace_root AS runtime_workspace_root, ars.isolation_mode AS runtime_isolation_mode,
ars.cancel_requested AS runtime_cancel_requested, ars.heartbeat_at AS runtime_heartbeat_at,
arp.command AS runtime_current_command, arp.status AS runtime_current_process_status
";

pub(crate) const AGENT_TASK_RUNTIME_JOIN: &str = r"
LEFT JOIN agent_runtime_sessions ars
  ON ars.tenant_id = at.tenant_id
 AND ars.agent_task_id = at.id
 AND ars.created_at = (
   SELECT MAX(ars2.created_at)
   FROM agent_runtime_sessions ars2
   WHERE ars2.tenant_id = at.tenant_id AND ars2.agent_task_id = at.id
 )
LEFT JOIN agent_runtime_processes arp
  ON arp.tenant_id = ars.tenant_id
 AND arp.runtime_session_id = ars.id
 AND arp.created_at = (
   SELECT MAX(arp2.created_at)
   FROM agent_runtime_processes arp2
   WHERE arp2.tenant_id = ars.tenant_id AND arp2.runtime_session_id = ars.id
 )
";

pub(crate) const AGENT_TASK_EVENT_SELECT: &str = r"
id, tenant_id, task_id, event_type, phase, status, severity, message,
CAST(metadata_json AS TEXT) AS metadata_json, created_at
";

pub(crate) const AGENT_TRACE_EVENT_SELECT: &str = r"
id, tenant_id, task_id, event_type, phase, status, severity, message,
CAST(metadata_json AS TEXT) AS metadata_json, artifact_id, runtime_session_id,
runtime_process_id, token_input, token_output, CAST(cost_usd AS DOUBLE) AS cost_usd,
duration_ms, created_at
";

#[derive(Debug, Clone)]
pub struct CreateAgentTaskInput {
    pub tenant_id: String,
    pub source: String,
    pub source_ref: Option<String>,
    pub source_label: Option<String>,
    pub capability_key: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub owner_user_id: Option<String>,
    pub correlation_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub external_platform: Option<String>,
    pub external_channel_id: Option<String>,
    pub external_conversation_id: Option<String>,
    pub external_message_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub input_json: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct CreateAgentTaskOutcome {
    pub id: String,
    #[allow(dead_code)]
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskInfo {
    pub id: String,
    pub short_code: Option<String>,
    pub tenant_id: String,
    pub source: String,
    pub source_ref: Option<String>,
    pub source_label: Option<String>,
    pub capability_key: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub status: String,
    pub phase: String,
    pub progress_percent: i32,
    pub title: String,
    pub summary: Option<String>,
    pub owner_user_id: Option<String>,
    pub initiator_user_id: Option<String>,
    pub visibility_scope: String,
    pub team_id: Option<String>,
    pub correlation_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub root_task_id: Option<String>,
    pub origin_session_id: Option<String>,
    pub origin_turn_id: Option<String>,
    pub external_platform: Option<String>,
    pub external_channel_id: Option<String>,
    pub external_conversation_id: Option<String>,
    pub external_message_id: Option<String>,
    pub linked_resource_type: Option<String>,
    pub linked_resource_id: Option<String>,
    pub input_json: Option<Value>,
    pub output_json: Option<Value>,
    pub state_version: u64,
    pub desired_state: Option<String>,
    pub progress_json: Option<Value>,
    pub result_summary: Option<String>,
    pub result_artifact_ref: Option<String>,
    pub sensitivity_label: String,
    pub archived: bool,
    pub runtime_session: Option<AgentTaskRuntimeSummary>,
    pub queue: AgentTaskQueueInfo,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub last_event: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskQueueInfo {
    pub status: String,
    pub available_at: Option<String>,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub attempt_count: i32,
    pub max_attempts: i32,
    pub idempotency_key: Option<String>,
    pub priority: i32,
    pub last_error: Option<String>,
    pub finished_at: Option<String>,
    pub dead_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskRuntimeSummary {
    pub id: String,
    pub status: String,
    pub workspace_root: String,
    pub isolation_mode: String,
    pub cancel_requested: bool,
    pub heartbeat_at: Option<String>,
    pub current_command: Option<String>,
    pub current_process_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskEventInfo {
    pub id: String,
    pub tenant_id: String,
    pub task_id: String,
    pub event_type: String,
    pub phase: Option<String>,
    pub status: Option<String>,
    pub severity: String,
    pub message: String,
    pub metadata_json: Option<Value>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTraceEventInfo {
    pub id: String,
    pub tenant_id: String,
    pub task_id: String,
    pub event_type: String,
    pub phase: Option<String>,
    pub status: Option<String>,
    pub severity: String,
    pub message: String,
    pub metadata_json: Option<Value>,
    pub artifact_id: Option<String>,
    pub runtime_session_id: Option<String>,
    pub runtime_process_id: Option<String>,
    pub token_input: Option<u64>,
    pub token_output: Option<u64>,
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
}

#[derive(Debug, Deserialize, Default)]
pub struct TaskListQuery {
    pub status: Option<String>,
    pub attention_only: Option<bool>,
    pub capability_key: Option<String>,
    pub source: Option<String>,
    pub external_conversation_id: Option<String>,
    pub linked_resource_type: Option<String>,
    pub linked_resource_id: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct WatchDogAskRequest {
    pub question: String,
    pub scope: Option<String>,
    pub external_platform: Option<String>,
    pub external_channel_id: Option<String>,
    pub external_conversation_id: Option<String>,
    #[serde(default, rename = "asyncMode")]
    pub async_mode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueRecoverRequest {
    pub lease_timeout_secs: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueListQuery {
    pub queue_status: Option<String>,
    pub capability_key: Option<String>,
    pub worker_id: Option<String>,
    pub dead_only: Option<bool>,
    pub stale_only: Option<bool>,
    pub lease_timeout_secs: Option<i64>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}
