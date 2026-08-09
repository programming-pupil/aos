use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    egress::{evaluate_egress, EgressDecision, NetTarget},
    isolation::{evaluate_path_access, IsolationDecision, PathAccess},
    tenant_sandbox::{ExecOutcome, ExecutionAudit, ResourceUsage, TenantSandbox},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TenantCommand {
    pub command: String,
    pub resource_usage: ResourceUsage,
    pub egress_targets: Vec<NetTarget>,
    pub path_accesses: Vec<PathAccess>,
    pub exit_code: Option<i32>,
}

impl TenantCommand {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            resource_usage: ResourceUsage::default(),
            egress_targets: Vec::new(),
            path_accesses: Vec::new(),
            exit_code: Some(0),
        }
    }
}

#[must_use]
pub async fn execute_in_tenant_sandbox(
    sandbox: &TenantSandbox,
    command: &TenantCommand,
) -> ExecutionAudit {
    evaluate_tenant_command(sandbox, command, current_unix_timestamp())
}

#[must_use]
pub fn evaluate_tenant_command(
    sandbox: &TenantSandbox,
    command: &TenantCommand,
    started_at: impl Into<String>,
) -> ExecutionAudit {
    let started_at = started_at.into();
    let (outcome, reason) = classify_command(sandbox, command);
    ExecutionAudit::new(
        sandbox.tenant_id.clone(),
        command.command.clone(),
        started_at,
        command.resource_usage,
        outcome,
        reason,
    )
}

fn classify_command(
    sandbox: &TenantSandbox,
    command: &TenantCommand,
) -> (ExecOutcome, Option<String>) {
    if !sandbox.enabled {
        return (
            ExecOutcome::Failed,
            Some("tenant sandbox is disabled".to_string()),
        );
    }

    for access in &command.path_accesses {
        if let IsolationDecision::Deny { reason } = evaluate_path_access(sandbox, access) {
            return (ExecOutcome::Denied, Some(reason));
        }
    }

    for target in &command.egress_targets {
        if evaluate_egress(&sandbox.egress_policy, target) == EgressDecision::Deny {
            return (
                ExecOutcome::Denied,
                Some(format!("egress denied for host '{}'", target.host)),
            );
        }
    }

    if sandbox.quota.exceeded_by(command.resource_usage) {
        return (
            ExecOutcome::QuotaExceeded,
            Some("resource quota exceeded".to_string()),
        );
    }

    match command.exit_code {
        Some(0) => (ExecOutcome::Completed, None),
        Some(code) => (
            ExecOutcome::Failed,
            Some(format!("command exited with status {code}")),
        ),
        None => (
            ExecOutcome::Failed,
            Some("command did not report an exit code".to_string()),
        ),
    }
}

fn current_unix_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("unix:{}", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use crate::{
        egress::{EgressPolicy, EgressRule},
        isolation::PathAccess,
        tenant_executor::{evaluate_tenant_command, TenantCommand},
        tenant_sandbox::{ExecOutcome, ResourceQuota, ResourceUsage, TenantSandbox},
    };

    fn sandbox(quota: ResourceQuota) -> TenantSandbox {
        TenantSandbox::new(
            "tenant-a",
            PathBuf::from("/srv/aos/tenants/tenant-a"),
            quota,
            EgressPolicy {
                default: crate::egress::EgressDecision::Deny,
                rules: vec![EgressRule::allow_exact_host("api.internal")],
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_18_over_quota_command_is_terminated_and_audited(
            cpu_limit in 0_u64..1_000,
            mem_limit in 0_u64..1_000,
            wall_limit in 0_u64..1_000,
            over_by in 1_u64..1_000,
        ) {
            // Feature: codex-parity-gaps, Property 18: （OPTIONAL）超配额执行被终止并记入审计
            let quota = ResourceQuota::new(cpu_limit, mem_limit, wall_limit);
            let mut command = TenantCommand::new("python job.py");
            command.resource_usage = ResourceUsage {
                cpu_millis: cpu_limit.saturating_add(over_by),
                memory_bytes: mem_limit,
                wall_time_secs: wall_limit,
            };
            command.path_accesses = vec![PathAccess::read("/srv/aos/tenants/tenant-a/input.txt")];

            let audit = evaluate_tenant_command(&sandbox(quota), &command, "test-start");

            prop_assert_eq!(audit.outcome, ExecOutcome::QuotaExceeded);
            prop_assert_eq!(audit.tenant_id, "tenant-a");
            prop_assert_eq!(audit.command, "python job.py");
            prop_assert_eq!(audit.resource_usage, command.resource_usage);
            prop_assert!(audit.reason.as_deref().unwrap_or("").contains("quota"));
        }

        #[test]
        fn property_19_execution_audit_contains_required_fields(
            command_text in ".{1,80}",
            cpu in 0_u64..500,
            mem in 0_u64..500,
            wall in 0_u64..500,
        ) {
            // Feature: codex-parity-gaps, Property 19: （OPTIONAL）执行审计记录字段完整
            let mut command = TenantCommand::new(command_text.clone());
            command.resource_usage = ResourceUsage {
                cpu_millis: cpu,
                memory_bytes: mem,
                wall_time_secs: wall,
            };
            command.path_accesses = vec![PathAccess::read("/srv/aos/tenants/tenant-a/input.txt")];

            let audit = evaluate_tenant_command(
                &sandbox(ResourceQuota::new(1_000, 1_000, 1_000)),
                &command,
                "test-start",
            );

            prop_assert_eq!(audit.tenant_id, "tenant-a");
            prop_assert_eq!(audit.command, command_text);
            prop_assert_eq!(audit.started_at, "test-start");
            prop_assert_eq!(audit.resource_usage, command.resource_usage);
            prop_assert!(matches!(
                audit.outcome,
                ExecOutcome::Completed | ExecOutcome::QuotaExceeded | ExecOutcome::Denied | ExecOutcome::Failed
            ));
        }
    }

    #[test]
    fn denied_egress_is_audited_before_execution_success() {
        let mut command = TenantCommand::new("curl https://blocked.example");
        command
            .egress_targets
            .push(crate::egress::NetTarget::new("blocked.example"));

        let audit = evaluate_tenant_command(
            &sandbox(ResourceQuota::new(1_000, 1_000, 1_000)),
            &command,
            "test-start",
        );

        assert_eq!(audit.outcome, ExecOutcome::Denied);
        assert!(audit
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("egress denied"));
    }
}
