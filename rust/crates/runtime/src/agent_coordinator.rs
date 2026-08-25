//! Unified parent coordinator for plans, tasks, approvals and child agents.
//!
//! The coordinator deliberately owns only lifecycle state. Provider sessions,
//! tool execution and durable SQL projections remain in their existing layers;
//! all of them can use the revisioned [`TaskRegistry`] as the single authority
//! for parent/child state transitions.

use std::sync::Arc;

use crate::task_registry::{PlanStepStatus, Task, TaskRegistry, TaskStatus};
use crate::RuntimeCancellationToken;

#[derive(Debug, Clone, Default)]
pub struct AgentCoordinator {
    registry: TaskRegistry,
}

impl AgentCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn registry(&self) -> &TaskRegistry {
        &self.registry
    }

    #[must_use]
    pub fn shared_registry(&self) -> Arc<TaskRegistry> {
        Arc::new(self.registry.clone())
    }

    pub fn create_root(&self, prompt: &str, description: Option<&str>) -> Task {
        self.registry.create(prompt, description)
    }

    pub fn create_child(
        &self,
        parent_task_id: &str,
        prompt: &str,
        description: Option<&str>,
    ) -> Result<Task, String> {
        self.registry
            .create_child(parent_task_id, prompt, description)
    }

    pub fn start(&self, task_id: &str, expected_revision: Option<u64>) -> Result<Task, String> {
        self.registry.start(task_id, expected_revision)
    }

    pub fn complete(&self, task_id: &str, output: &str) -> Result<Task, String> {
        self.registry.complete(task_id, output)
    }

    pub fn settle_terminal(
        &self,
        task_id: &str,
        status: TaskStatus,
        detail: Option<&str>,
    ) -> Result<Task, String> {
        self.registry.settle_terminal(task_id, status, detail)
    }

    pub fn fail(&self, task_id: &str, error: &str) -> Result<Task, String> {
        self.registry.fail(task_id, error)
    }

    pub fn timeout(&self, task_id: &str, detail: &str) -> Result<Task, String> {
        self.registry.timeout(task_id, detail)
    }

    pub fn cancel(&self, task_id: &str) -> Result<Task, String> {
        self.registry.cancel(task_id)
    }

    pub fn retry(&self, task_id: &str) -> Result<Task, String> {
        self.registry.retry(task_id)
    }

    pub fn set_plan(&self, task_id: &str, steps: Vec<String>) -> Result<Task, String> {
        self.registry.set_plan(task_id, steps)
    }

    pub fn advance_plan_step(
        &self,
        task_id: &str,
        step_id: &str,
        status: PlanStepStatus,
    ) -> Result<Task, String> {
        self.registry.advance_plan_step(task_id, step_id, status)
    }

    pub fn request_approval(
        &self,
        task_id: &str,
        approval_id: &str,
        tool_name: &str,
        reason: &str,
        expires_at: Option<u64>,
    ) -> Result<Task, String> {
        self.registry
            .request_approval(task_id, approval_id, tool_name, reason, expires_at)
    }

    pub fn resolve_approval(
        &self,
        task_id: &str,
        approval_id: &str,
        approved: bool,
    ) -> Result<Task, String> {
        self.registry
            .resolve_approval(task_id, approval_id, approved)
    }

    pub fn cancellation_token(&self, task_id: &str) -> Result<RuntimeCancellationToken, String> {
        self.registry.cancellation_token(task_id)
    }

    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<Task> {
        self.registry.get(task_id)
    }

    #[must_use]
    pub fn list(&self, status: Option<TaskStatus>) -> Vec<Task> {
        self.registry.list(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_owns_parent_child_lifecycle() {
        let coordinator = AgentCoordinator::new();
        let parent = coordinator.create_root("parent", None);
        let child = coordinator
            .create_child(&parent.task_id, "child", None)
            .expect("child should be attached");
        coordinator
            .start(&parent.task_id, Some(parent.revision))
            .expect("parent should start");
        coordinator
            .start(&child.task_id, Some(child.revision))
            .expect("child should start");

        let cancelled = coordinator
            .cancel(&parent.task_id)
            .expect("parent cancellation should succeed");
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            coordinator
                .get(&child.task_id)
                .expect("child exists")
                .status,
            TaskStatus::Cancelled
        );
        assert!(coordinator
            .cancellation_token(&child.task_id)
            .expect("child token")
            .is_cancelled());
    }
}
