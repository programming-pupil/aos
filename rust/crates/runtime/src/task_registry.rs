#![allow(clippy::must_use_candidate, clippy::unnecessary_map_or)]
//! In-memory task registry for sub-agent task lifecycle management.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{validate_packet, RuntimeCancellationToken, TaskPacket, TaskPacketValidationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Created,
    Planned,
    Ready,
    Running,
    WaitingApproval,
    WaitingInput,
    Completed,
    Failed,
    Stopped,
    Cancelled,
    TimedOut,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Planned => write!(f, "planned"),
            Self::Ready => write!(f, "ready"),
            Self::Running => write!(f, "running"),
            Self::WaitingApproval => write!(f, "waiting_approval"),
            Self::WaitingInput => write!(f, "waiting_input"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Stopped => write!(f, "stopped"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::TimedOut => write!(f, "timed_out"),
        }
    }
}

impl TaskStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Stopped | Self::Cancelled | Self::TimedOut
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub step_id: String,
    pub content: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskApproval {
    pub approval_id: String,
    pub tool_name: String,
    pub reason: String,
    pub status: ApprovalStatus,
    pub requested_at: u64,
    pub expires_at: Option<u64>,
    pub resolved_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEvent {
    pub revision: u64,
    pub kind: String,
    pub status: TaskStatus,
    pub detail: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub task_id: String,
    pub prompt: String,
    pub description: Option<String>,
    pub task_packet: Option<TaskPacket>,
    pub status: TaskStatus,
    /// Monotonically increasing coordinator revision. Mutations must advance
    /// this value exactly once so stale writers cannot overwrite newer state.
    pub revision: u64,
    pub attempt: u32,
    pub max_attempts: u32,
    pub parent_task_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<TaskMessage>,
    pub output: String,
    pub team_id: Option<String>,
    pub plan: Vec<PlanStep>,
    pub pending_approval: Option<TaskApproval>,
    pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage {
    pub role: String,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    tasks: HashMap<String, Task>,
    cancellation: HashMap<String, RuntimeCancellationToken>,
    counter: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl TaskRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&self, prompt: &str, description: Option<&str>) -> Task {
        self.create_task(prompt.to_owned(), description.map(str::to_owned), None)
    }

    pub fn create_from_packet(
        &self,
        packet: TaskPacket,
    ) -> Result<Task, TaskPacketValidationError> {
        let packet = validate_packet(packet)?.into_inner();
        Ok(self.create_task(
            packet.objective.clone(),
            Some(packet.scope.clone()),
            Some(packet),
        ))
    }

    fn create_task(
        &self,
        prompt: String,
        description: Option<String>,
        task_packet: Option<TaskPacket>,
    ) -> Task {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        inner.counter += 1;
        let ts = now_secs();
        let task_id = format!("task_{:08x}_{}", ts, inner.counter);
        let task = Task {
            task_id: task_id.clone(),
            prompt,
            description,
            task_packet,
            status: TaskStatus::Created,
            revision: 1,
            attempt: 0,
            max_attempts: 1,
            parent_task_id: None,
            created_at: ts,
            updated_at: ts,
            messages: Vec::new(),
            output: String::new(),
            team_id: None,
            plan: Vec::new(),
            pending_approval: None,
            events: vec![TaskEvent {
                revision: 1,
                kind: "created".to_string(),
                status: TaskStatus::Created,
                detail: None,
                timestamp: ts,
            }],
        };
        inner
            .cancellation
            .insert(task_id.clone(), RuntimeCancellationToken::new());
        inner.tasks.insert(task_id, task.clone());
        task
    }

    /// Create a child task under a coordinator-owned parent. The parent must
    /// exist and may not be terminal; this prevents orphaned work from being
    /// introduced after a parent has already completed or been cancelled.
    pub fn create_child(
        &self,
        parent_task_id: &str,
        prompt: &str,
        description: Option<&str>,
    ) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let parent = inner
            .tasks
            .get(parent_task_id)
            .ok_or_else(|| format!("parent task not found: {parent_task_id}"))?;
        if parent.status.is_terminal() {
            return Err(format!(
                "parent task {parent_task_id} is already terminal: {}",
                parent.status
            ));
        }
        inner.counter += 1;
        let ts = now_secs();
        let task_id = format!("task_{:08x}_{}", ts, inner.counter);
        let task = Task {
            task_id: task_id.clone(),
            prompt: prompt.to_owned(),
            description: description.map(str::to_owned),
            task_packet: None,
            status: TaskStatus::Created,
            revision: 1,
            attempt: 0,
            max_attempts: 1,
            parent_task_id: Some(parent_task_id.to_owned()),
            created_at: ts,
            updated_at: ts,
            messages: Vec::new(),
            output: String::new(),
            team_id: None,
            plan: Vec::new(),
            pending_approval: None,
            events: vec![TaskEvent {
                revision: 1,
                kind: "created".to_string(),
                status: TaskStatus::Created,
                detail: Some(format!("child of {parent_task_id}")),
                timestamp: ts,
            }],
        };
        inner
            .cancellation
            .insert(task_id.clone(), RuntimeCancellationToken::new());
        inner.tasks.insert(task_id, task.clone());
        Ok(task)
    }

    pub fn get(&self, task_id: &str) -> Option<Task> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.tasks.get(task_id).cloned()
    }

    pub fn list(&self, status_filter: Option<TaskStatus>) -> Vec<Task> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner
            .tasks
            .values()
            .filter(|t| status_filter.map_or(true, |s| t.status == s))
            .cloned()
            .collect()
    }

    pub fn stop(&self, task_id: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = transition_locked(
            &mut inner,
            task_id,
            None,
            TaskStatus::Stopped,
            "stopped",
            Some("stopped by coordinator"),
        )?;
        cancel_descendants_locked(&mut inner, task_id);
        if let Some(token) = inner.cancellation.get(task_id) {
            token.cancel();
        }
        Ok(task)
    }

    /// Cooperative cancellation for a parent task and all of its descendants.
    /// `stop` is retained as the legacy terminal state; this method exposes the
    /// canonical cancellation state used by the coordinator and resume logic.
    pub fn cancel(&self, task_id: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = transition_locked(
            &mut inner,
            task_id,
            None,
            TaskStatus::Cancelled,
            "cancelled",
            Some("cancelled by coordinator"),
        )?;
        if let Some(token) = inner.cancellation.get(task_id) {
            token.cancel();
        }
        cancel_descendants_locked(&mut inner, task_id);
        Ok(task)
    }

    pub fn cancellation_token(&self, task_id: &str) -> Result<RuntimeCancellationToken, String> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        if !inner.tasks.contains_key(task_id) {
            return Err(format!("task not found: {task_id}"));
        }
        inner
            .cancellation
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("task cancellation token missing: {task_id}"))
    }

    pub fn start(&self, task_id: &str, expected_revision: Option<u64>) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status == TaskStatus::WaitingApproval
            || task
                .pending_approval
                .as_ref()
                .is_some_and(|approval| approval.status == ApprovalStatus::Pending)
        {
            return Err(format!(
                "task {task_id} cannot start while approval is pending"
            ));
        }
        if task
            .plan
            .iter()
            .any(|step| step.status != PlanStepStatus::Completed)
        {
            return Err(format!(
                "task {task_id} cannot start while its plan has unfinished steps"
            ));
        }
        transition_locked(
            &mut inner,
            task_id,
            expected_revision,
            TaskStatus::Running,
            "started",
            None,
        )
    }

    pub fn complete(&self, task_id: &str, output: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        if has_active_descendants_locked(&inner, task_id) {
            return Err(format!(
                "task {task_id} cannot complete while a child task is still active"
            ));
        }
        transition_locked(
            &mut inner,
            task_id,
            None,
            TaskStatus::Completed,
            "completed",
            None,
        )?;
        let task_mut = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        task_mut.output = output.to_owned();
        task_mut.updated_at = now_secs();
        append_event(task_mut, "output_recorded", None);
        Ok(task_mut.clone())
    }

    /// Settle a terminal task idempotently. Agent persistence can observe the
    /// same completion from both the execution loop and its manifest writer;
    /// repeated writes must not turn a successful task into a false failure.
    pub fn settle_terminal(
        &self,
        task_id: &str,
        status: TaskStatus,
        detail: Option<&str>,
    ) -> Result<Task, String> {
        if !status.is_terminal() {
            return Err(format!("{status} is not a terminal task status"));
        }
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let current = inner
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if current.status == status {
            return Ok(current);
        }
        if current.status.is_terminal() {
            return Err(format!(
                "task {task_id} is already in terminal state: {}",
                current.status
            ));
        }
        if status == TaskStatus::Completed && has_active_descendants_locked(&inner, task_id) {
            return Err(format!(
                "task {task_id} cannot complete while a child task is still active"
            ));
        }
        let cancellation = inner.cancellation.get(task_id).cloned();
        let mut settled = transition_locked(
            &mut inner,
            task_id,
            None,
            status,
            &status.to_string(),
            detail,
        )?;
        if status == TaskStatus::Completed {
            let task = inner
                .tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("task not found: {task_id}"))?;
            task.output = detail.unwrap_or_default().to_owned();
            task.updated_at = now_secs();
            append_event(task, "output_recorded", None);
            settled = task.clone();
        } else if matches!(
            status,
            TaskStatus::Stopped | TaskStatus::Cancelled | TaskStatus::TimedOut
        ) {
            if let Some(token) = cancellation {
                token.cancel();
            }
        }
        if matches!(
            status,
            TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Cancelled | TaskStatus::TimedOut
        ) {
            cancel_descendants_locked(&mut inner, task_id);
        }
        Ok(settled)
    }

    pub fn fail(&self, task_id: &str, error: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = transition_locked(
            &mut inner,
            task_id,
            None,
            TaskStatus::Failed,
            "failed",
            Some(error),
        )?;
        if let Some(token) = inner.cancellation.get(task_id) {
            token.cancel();
        }
        cancel_descendants_locked(&mut inner, task_id);
        Ok(task)
    }

    pub fn timeout(&self, task_id: &str, detail: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = transition_locked(
            &mut inner,
            task_id,
            None,
            TaskStatus::TimedOut,
            "timed_out",
            Some(detail),
        )?;
        if let Some(token) = inner.cancellation.get(task_id) {
            token.cancel();
        }
        cancel_descendants_locked(&mut inner, task_id);
        Ok(task)
    }

    pub fn retry(&self, task_id: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let current = inner
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if !matches!(
            current.status,
            TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Cancelled | TaskStatus::TimedOut
        ) {
            return Err(format!(
                "task {task_id} cannot retry from state {}",
                current.status
            ));
        }
        if current.attempt.saturating_add(1) >= current.max_attempts {
            return Err(format!(
                "task {task_id} retry budget exhausted ({}/{})",
                current.attempt, current.max_attempts
            ));
        }
        if let Some(parent_task_id) = current.parent_task_id.as_deref() {
            if inner
                .tasks
                .get(parent_task_id)
                .is_some_and(|parent| parent.status.is_terminal())
            {
                return Err(format!(
                    "task {task_id} cannot retry because parent task {parent_task_id} is terminal"
                ));
            }
        }
        inner
            .cancellation
            .insert(task_id.to_owned(), RuntimeCancellationToken::new());
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        task.attempt = task.attempt.saturating_add(1);
        task.status = TaskStatus::Ready;
        task.pending_approval = None;
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "retried", Some("retry scheduled"));
        Ok(task.clone())
    }

    pub fn set_max_attempts(&self, task_id: &str, max_attempts: u32) -> Result<Task, String> {
        if max_attempts == 0 {
            return Err("max_attempts must be at least 1".to_string());
        }
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        task.max_attempts = max_attempts;
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(
            task,
            "retry_budget_updated",
            Some(&max_attempts.to_string()),
        );
        Ok(task.clone())
    }

    pub fn set_plan(&self, task_id: &str, steps: Vec<String>) -> Result<Task, String> {
        if steps.iter().any(|step| step.trim().is_empty()) {
            return Err("plan steps must not be empty".to_string());
        }
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        task.plan = steps
            .into_iter()
            .enumerate()
            .map(|(index, content)| PlanStep {
                step_id: format!("step-{}", index.saturating_add(1)),
                content,
                status: PlanStepStatus::Pending,
            })
            .collect();
        if !task.status.is_terminal() {
            task.status = TaskStatus::Planned;
        }
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(
            task,
            "plan_set",
            Some(&format!("{} steps", task.plan.len())),
        );
        Ok(task.clone())
    }

    pub fn advance_plan_step(
        &self,
        task_id: &str,
        step_id: &str,
        status: PlanStepStatus,
    ) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        let step = task
            .plan
            .iter_mut()
            .find(|step| step.step_id == step_id)
            .ok_or_else(|| format!("plan step not found: {step_id}"))?;
        step.status = status;
        if !task.status.is_terminal() {
            task.status = if task
                .plan
                .iter()
                .all(|step| step.status == PlanStepStatus::Completed)
            {
                TaskStatus::Ready
            } else if matches!(status, PlanStepStatus::InProgress) {
                TaskStatus::Running
            } else {
                TaskStatus::Planned
            };
        }
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "plan_step_updated", Some(step_id));
        Ok(task.clone())
    }

    pub fn request_approval(
        &self,
        task_id: &str,
        approval_id: &str,
        tool_name: &str,
        reason: &str,
        expires_at: Option<u64>,
    ) -> Result<Task, String> {
        if approval_id.trim().is_empty() || tool_name.trim().is_empty() {
            return Err("approval_id and tool_name must not be empty".to_string());
        }
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        if let Some(existing) = &task.pending_approval {
            if existing.status == ApprovalStatus::Pending {
                if existing.approval_id == approval_id
                    && existing.tool_name == tool_name
                    && existing.reason == reason
                    && existing.expires_at == expires_at
                {
                    return Ok(task.clone());
                }
                if existing.approval_id == approval_id {
                    return Err(format!(
                        "approval {approval_id} already exists with different request details"
                    ));
                }
                return Err(format!("task {task_id} already has a pending approval"));
            }
        }
        task.pending_approval = Some(TaskApproval {
            approval_id: approval_id.to_owned(),
            tool_name: tool_name.to_owned(),
            reason: reason.to_owned(),
            status: ApprovalStatus::Pending,
            requested_at: now_secs(),
            expires_at,
            resolved_at: None,
        });
        task.status = TaskStatus::WaitingApproval;
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "approval_requested", Some(approval_id));
        Ok(task.clone())
    }

    pub fn resolve_approval(
        &self,
        task_id: &str,
        approval_id: &str,
        approved: bool,
    ) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let cancellation = inner.cancellation.get(task_id).cloned();
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!(
                "task {task_id} is already in terminal state: {}",
                task.status
            ));
        }
        let approval = task
            .pending_approval
            .as_mut()
            .ok_or_else(|| format!("task {task_id} has no pending approval"))?;
        if approval.approval_id != approval_id || approval.status != ApprovalStatus::Pending {
            return Err(
                "approval is stale, already resolved, or belongs to another task".to_string(),
            );
        }
        let now = now_secs();
        if approval
            .expires_at
            .is_some_and(|expires_at| now >= expires_at)
        {
            approval.status = ApprovalStatus::Expired;
            approval.resolved_at = Some(now);
            task.status = TaskStatus::TimedOut;
            task.pending_approval = None;
            task.revision = task.revision.saturating_add(1);
            task.updated_at = now;
            append_event(task, "approval_expired", Some(approval_id));
            let settled = task.clone();
            if let Some(token) = cancellation.as_ref() {
                token.cancel();
            }
            cancel_descendants_locked(&mut inner, task_id);
            return Ok(settled);
        }
        approval.status = if approved {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        approval.resolved_at = Some(now);
        task.status = if approved {
            if task
                .plan
                .iter()
                .any(|step| step.status != PlanStepStatus::Completed)
            {
                TaskStatus::Planned
            } else {
                TaskStatus::Running
            }
        } else {
            TaskStatus::Cancelled
        };
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now;
        append_event(
            task,
            if approved {
                "approval_approved"
            } else {
                "approval_denied"
            },
            Some(approval_id),
        );
        let settled = task.clone();
        if !approved {
            if let Some(token) = cancellation.as_ref() {
                token.cancel();
            }
            cancel_descendants_locked(&mut inner, task_id);
        }
        Ok(settled)
    }

    #[must_use]
    pub fn events(&self, task_id: &str) -> Option<Vec<TaskEvent>> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.tasks.get(task_id).map(|task| task.events.clone())
    }

    pub fn update(&self, task_id: &str, message: &str) -> Result<Task, String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }

        task.messages.push(TaskMessage {
            role: String::from("user"),
            content: message.to_owned(),
            timestamp: now_secs(),
        });
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "message_appended", None);
        Ok(task.clone())
    }

    pub fn output(&self, task_id: &str) -> Result<String, String> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        Ok(task.output.clone())
    }

    pub fn append_output(&self, task_id: &str, output: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        task.output.push_str(output);
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "output_appended", None);
        Ok(())
    }

    pub fn set_status(&self, task_id: &str, status: TaskStatus) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        if status == TaskStatus::Completed && has_active_descendants_locked(&inner, task_id) {
            return Err(format!(
                "task {task_id} cannot complete while a child task is still active"
            ));
        }
        let cancellation = inner.cancellation.get(task_id).cloned();
        transition_locked(&mut inner, task_id, None, status, "status_changed", None)?;
        if status.is_terminal() && status != TaskStatus::Completed {
            if let Some(token) = cancellation {
                token.cancel();
            }
        }
        if matches!(
            status,
            TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Cancelled | TaskStatus::TimedOut
        ) {
            cancel_descendants_locked(&mut inner, task_id);
        }
        Ok(())
    }

    pub fn assign_team(&self, task_id: &str, team_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        let task = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("task not found: {task_id}"))?;
        if task.status.is_terminal() {
            return Err(format!("task {task_id} is already terminal"));
        }
        task.team_id = Some(team_id.to_owned());
        task.revision = task.revision.saturating_add(1);
        task.updated_at = now_secs();
        append_event(task, "team_assigned", Some(team_id));
        Ok(())
    }

    pub fn remove(&self, task_id: &str) -> Option<Task> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        if let Some(token) = inner.cancellation.get(task_id) {
            token.cancel();
        }
        inner.cancellation.remove(task_id);
        inner.tasks.remove(task_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.tasks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn append_event(task: &mut Task, kind: &str, detail: Option<&str>) {
    task.events.push(TaskEvent {
        revision: task.revision,
        kind: kind.to_owned(),
        status: task.status,
        detail: detail.map(str::to_owned),
        timestamp: task.updated_at,
    });
}

fn transition_locked(
    inner: &mut RegistryInner,
    task_id: &str,
    expected_revision: Option<u64>,
    status: TaskStatus,
    kind: &str,
    detail: Option<&str>,
) -> Result<Task, String> {
    let task = inner
        .tasks
        .get_mut(task_id)
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if let Some(expected) = expected_revision {
        if task.revision != expected {
            return Err(format!(
                "stale task revision for {task_id}: expected {expected}, current {}",
                task.revision
            ));
        }
    }
    if task.status.is_terminal() {
        return Err(format!(
            "task {task_id} is already in terminal state: {}",
            task.status
        ));
    }
    if status == TaskStatus::Running
        && task
            .pending_approval
            .as_ref()
            .is_some_and(|approval| approval.status == ApprovalStatus::Pending)
    {
        return Err(format!(
            "task {task_id} cannot enter running state while approval is pending"
        ));
    }
    if status == TaskStatus::Running
        && task
            .plan
            .iter()
            .any(|step| step.status != PlanStepStatus::Completed)
    {
        return Err(format!(
            "task {task_id} cannot enter running state while its plan has unfinished steps"
        ));
    }
    task.status = status;
    if status.is_terminal() {
        task.pending_approval = None;
    }
    task.revision = task.revision.saturating_add(1);
    task.updated_at = now_secs();
    append_event(task, kind, detail);
    Ok(task.clone())
}

fn has_active_descendants_locked(inner: &RegistryInner, parent_task_id: &str) -> bool {
    let mut pending = vec![parent_task_id.to_owned()];
    while let Some(parent_id) = pending.pop() {
        let child_ids: Vec<String> = inner
            .tasks
            .values()
            .filter(|task| task.parent_task_id.as_deref() == Some(parent_id.as_str()))
            .map(|task| task.task_id.clone())
            .collect();
        for child_id in child_ids {
            if inner
                .tasks
                .get(&child_id)
                .is_some_and(|child| !child.status.is_terminal())
            {
                return true;
            }
            pending.push(child_id);
        }
    }
    false
}

fn cancel_descendants_locked(inner: &mut RegistryInner, parent_task_id: &str) {
    let mut pending = vec![parent_task_id.to_owned()];
    while let Some(parent_id) = pending.pop() {
        let child_ids: Vec<String> = inner
            .tasks
            .values()
            .filter(|task| task.parent_task_id.as_deref() == Some(parent_id.as_str()))
            .map(|task| task.task_id.clone())
            .collect();
        for child_id in child_ids {
            if let Some(token) = inner.cancellation.get(&child_id) {
                token.cancel();
            }
            if let Some(child) = inner.tasks.get_mut(&child_id) {
                if !child.status.is_terminal() {
                    child.status = TaskStatus::Cancelled;
                    child.revision = child.revision.saturating_add(1);
                    child.updated_at = now_secs();
                    child.pending_approval = None;
                    append_event(child, "cancelled_by_parent", Some(parent_task_id));
                }
            }
            pending.push(child_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_retrieves_tasks() {
        let registry = TaskRegistry::new();
        let task = registry.create("Do something", Some("A test task"));
        assert_eq!(task.status, TaskStatus::Created);
        assert_eq!(task.prompt, "Do something");
        assert_eq!(task.description.as_deref(), Some("A test task"));
        assert_eq!(task.task_packet, None);

        let fetched = registry.get(&task.task_id).expect("task should exist");
        assert_eq!(fetched.task_id, task.task_id);
    }

    #[test]
    fn creates_task_from_packet() {
        let registry = TaskRegistry::new();
        let packet = TaskPacket {
            objective: "Ship task packet support".to_string(),
            scope: "runtime/task system".to_string(),
            repo: "claw-code-parity".to_string(),
            branch_policy: "origin/main only".to_string(),
            acceptance_tests: vec!["cargo test --workspace".to_string()],
            commit_policy: "single commit".to_string(),
            reporting_contract: "print commit sha".to_string(),
            escalation_policy: "manual escalation".to_string(),
        };

        let task = registry
            .create_from_packet(packet.clone())
            .expect("packet-backed task should be created");

        assert_eq!(task.prompt, packet.objective);
        assert_eq!(task.description.as_deref(), Some("runtime/task system"));
        assert_eq!(task.task_packet, Some(packet.clone()));

        let fetched = registry.get(&task.task_id).expect("task should exist");
        assert_eq!(fetched.task_packet, Some(packet));
    }

    #[test]
    fn lists_tasks_with_optional_filter() {
        let registry = TaskRegistry::new();
        registry.create("Task A", None);
        let task_b = registry.create("Task B", None);
        registry
            .set_status(&task_b.task_id, TaskStatus::Running)
            .expect("set status should succeed");

        let all = registry.list(None);
        assert_eq!(all.len(), 2);

        let running = registry.list(Some(TaskStatus::Running));
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].task_id, task_b.task_id);

        let created = registry.list(Some(TaskStatus::Created));
        assert_eq!(created.len(), 1);
    }

    #[test]
    fn stops_running_task() {
        let registry = TaskRegistry::new();
        let task = registry.create("Stoppable", None);
        registry
            .set_status(&task.task_id, TaskStatus::Running)
            .unwrap();

        let stopped = registry.stop(&task.task_id).expect("stop should succeed");
        assert_eq!(stopped.status, TaskStatus::Stopped);

        // Stopping again should fail
        let result = registry.stop(&task.task_id);
        assert!(result.is_err());
    }

    #[test]
    fn updates_task_with_messages() {
        let registry = TaskRegistry::new();
        let task = registry.create("Messageable", None);
        let updated = registry
            .update(&task.task_id, "Here's more context")
            .expect("update should succeed");
        assert_eq!(updated.messages.len(), 1);
        assert_eq!(updated.messages[0].content, "Here's more context");
        assert_eq!(updated.messages[0].role, "user");
    }

    #[test]
    fn appends_and_retrieves_output() {
        let registry = TaskRegistry::new();
        let task = registry.create("Output task", None);
        registry
            .append_output(&task.task_id, "line 1\n")
            .expect("append should succeed");
        registry
            .append_output(&task.task_id, "line 2\n")
            .expect("append should succeed");

        let output = registry.output(&task.task_id).expect("output should exist");
        assert_eq!(output, "line 1\nline 2\n");
    }

    #[test]
    fn assigns_team_and_removes_task() {
        let registry = TaskRegistry::new();
        let task = registry.create("Team task", None);
        registry
            .assign_team(&task.task_id, "team_abc")
            .expect("assign should succeed");

        let fetched = registry.get(&task.task_id).unwrap();
        assert_eq!(fetched.team_id.as_deref(), Some("team_abc"));

        let removed = registry.remove(&task.task_id);
        assert!(removed.is_some());
        assert!(registry.get(&task.task_id).is_none());
        assert!(registry.is_empty());
    }

    #[test]
    fn rejects_operations_on_missing_task() {
        let registry = TaskRegistry::new();
        assert!(registry.stop("nonexistent").is_err());
        assert!(registry.update("nonexistent", "msg").is_err());
        assert!(registry.output("nonexistent").is_err());
        assert!(registry.append_output("nonexistent", "data").is_err());
        assert!(registry
            .set_status("nonexistent", TaskStatus::Running)
            .is_err());
    }

    #[test]
    fn task_status_display_all_variants() {
        // given
        let cases = [
            (TaskStatus::Created, "created"),
            (TaskStatus::Running, "running"),
            (TaskStatus::Completed, "completed"),
            (TaskStatus::Failed, "failed"),
            (TaskStatus::Stopped, "stopped"),
        ];

        // when
        let rendered: Vec<_> = cases
            .into_iter()
            .map(|(status, expected)| (status.to_string(), expected))
            .collect();

        // then
        assert_eq!(
            rendered,
            vec![
                ("created".to_string(), "created"),
                ("running".to_string(), "running"),
                ("completed".to_string(), "completed"),
                ("failed".to_string(), "failed"),
                ("stopped".to_string(), "stopped"),
            ]
        );
    }

    #[test]
    fn stop_rejects_completed_task() {
        // given
        let registry = TaskRegistry::new();
        let task = registry.create("done", None);
        registry
            .set_status(&task.task_id, TaskStatus::Completed)
            .expect("set status should succeed");

        // when
        let result = registry.stop(&task.task_id);

        // then
        let error = result.expect_err("completed task should be rejected");
        assert!(error.contains("already in terminal state"));
        assert!(error.contains("completed"));
    }

    #[test]
    fn stop_rejects_failed_task() {
        // given
        let registry = TaskRegistry::new();
        let task = registry.create("failed", None);
        registry
            .set_status(&task.task_id, TaskStatus::Failed)
            .expect("set status should succeed");

        // when
        let result = registry.stop(&task.task_id);

        // then
        let error = result.expect_err("failed task should be rejected");
        assert!(error.contains("already in terminal state"));
        assert!(error.contains("failed"));
    }

    #[test]
    fn stop_succeeds_from_created_state() {
        // given
        let registry = TaskRegistry::new();
        let task = registry.create("created task", None);

        // when
        let stopped = registry.stop(&task.task_id).expect("stop should succeed");

        // then
        assert_eq!(stopped.status, TaskStatus::Stopped);
        assert!(stopped.updated_at >= task.updated_at);
    }

    #[test]
    fn new_registry_is_empty() {
        // given
        let registry = TaskRegistry::new();

        // when
        let all_tasks = registry.list(None);

        // then
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(all_tasks.is_empty());
    }

    #[test]
    fn create_without_description() {
        // given
        let registry = TaskRegistry::new();

        // when
        let task = registry.create("Do the thing", None);

        // then
        assert!(task.task_id.starts_with("task_"));
        assert_eq!(task.description, None);
        assert_eq!(task.task_packet, None);
        assert!(task.messages.is_empty());
        assert!(task.output.is_empty());
        assert_eq!(task.team_id, None);
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        // given
        let registry = TaskRegistry::new();

        // when
        let removed = registry.remove("missing");

        // then
        assert!(removed.is_none());
    }

    #[test]
    fn assign_team_rejects_missing_task() {
        // given
        let registry = TaskRegistry::new();

        // when
        let result = registry.assign_team("missing", "team_123");

        // then
        let error = result.expect_err("missing task should be rejected");
        assert_eq!(error, "task not found: missing");
    }

    #[test]
    fn coordinator_revision_rejects_stale_start() {
        let registry = TaskRegistry::new();
        let task = registry.create("revisioned", None);
        registry
            .update(&task.task_id, "new context")
            .expect("update should advance revision");
        let error = registry
            .start(&task.task_id, Some(task.revision))
            .expect_err("stale revision must not start task");
        assert!(error.contains("stale task revision"));
    }

    #[test]
    fn plan_approval_retry_and_expiry_are_terminally_safe() {
        let registry = TaskRegistry::new();
        let task = registry.create("governed", None);
        registry
            .set_max_attempts(&task.task_id, 2)
            .expect("retry budget");
        let planned = registry
            .set_plan(&task.task_id, vec!["inspect".into(), "verify".into()])
            .expect("plan should persist");
        assert_eq!(planned.status, TaskStatus::Planned);
        registry
            .advance_plan_step(&task.task_id, "step-1", PlanStepStatus::Completed)
            .expect("step should advance");

        let waiting = registry
            .request_approval(
                &task.task_id,
                "approval-1",
                "bash",
                "run verification",
                Some(0),
            )
            .expect("approval should suspend task");
        assert_eq!(waiting.status, TaskStatus::WaitingApproval);
        let expired = registry
            .resolve_approval(&task.task_id, "approval-1", true)
            .expect("expired approval should settle fail-closed");
        assert_eq!(expired.status, TaskStatus::TimedOut);
        assert!(registry
            .cancellation_token(&task.task_id)
            .expect("cancellation token")
            .is_cancelled());
        assert!(registry
            .resolve_approval(&task.task_id, "approval-1", true)
            .is_err());
        let retried = registry
            .retry(&task.task_id)
            .expect("retry should reset token");
        assert_eq!(retried.status, TaskStatus::Ready);
        assert_eq!(retried.attempt, 1);
        assert!(!registry
            .cancellation_token(&task.task_id)
            .expect("new cancellation token")
            .is_cancelled());
    }

    #[test]
    fn terminal_settlement_is_idempotent_and_parent_failure_cancels_children() {
        let registry = TaskRegistry::new();
        let parent = registry.create("parent", None);
        let child = registry
            .create_child(&parent.task_id, "child", None)
            .expect("child should be created");
        registry
            .start(&parent.task_id, Some(parent.revision))
            .expect("parent should start");
        registry
            .start(&child.task_id, Some(child.revision))
            .expect("child should start");

        let error = registry
            .complete(&parent.task_id, "premature")
            .expect_err("parent cannot complete with active child");
        assert!(error.contains("child task is still active"));

        let failed = registry
            .settle_terminal(&parent.task_id, TaskStatus::Failed, Some("child failed"))
            .expect("parent failure should settle");
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(
            registry.get(&child.task_id).expect("child exists").status,
            TaskStatus::Cancelled
        );
        let repeated = registry
            .settle_terminal(&parent.task_id, TaskStatus::Failed, Some("duplicate"))
            .expect("same terminal settlement should be idempotent");
        assert_eq!(repeated.status, TaskStatus::Failed);
        assert!(registry
            .settle_terminal(&parent.task_id, TaskStatus::Completed, Some("wrong"))
            .is_err());
    }
}
