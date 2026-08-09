use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::egress::EgressPolicy;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TenantSandbox {
    pub tenant_id: String,
    pub root: PathBuf,
    pub quota: ResourceQuota,
    pub egress_policy: EgressPolicy,
    pub enabled: bool,
}

impl TenantSandbox {
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        root: impl Into<PathBuf>,
        quota: ResourceQuota,
        egress_policy: EgressPolicy,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            root: root.into(),
            quota,
            egress_policy,
            enabled: true,
        }
    }

    #[must_use]
    pub fn from_base_dir(
        base_dir: impl AsRef<Path>,
        tenant_id: impl Into<String>,
        quota: ResourceQuota,
        egress_policy: EgressPolicy,
    ) -> Self {
        let tenant_id = tenant_id.into();
        let root = tenant_root_from_base(base_dir, &tenant_id);
        Self::new(tenant_id, root, quota, egress_policy)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceQuota {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub wall_time_secs: u64,
}

impl ResourceQuota {
    #[must_use]
    pub const fn new(cpu_millis: u64, memory_bytes: u64, wall_time_secs: u64) -> Self {
        Self {
            cpu_millis,
            memory_bytes,
            wall_time_secs,
        }
    }

    #[must_use]
    pub const fn local_lightweight_default() -> Self {
        Self {
            cpu_millis: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            wall_time_secs: 120,
        }
    }

    #[must_use]
    pub fn exceeded_by(self, usage: ResourceUsage) -> bool {
        usage.cpu_millis > self.cpu_millis
            || usage.memory_bytes > self.memory_bytes
            || usage.wall_time_secs > self.wall_time_secs
    }
}

impl Default for ResourceQuota {
    fn default() -> Self {
        Self::local_lightweight_default()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUsage {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub wall_time_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionAudit {
    pub tenant_id: String,
    pub command: String,
    pub started_at: String,
    pub resource_usage: ResourceUsage,
    pub outcome: ExecOutcome,
    pub reason: Option<String>,
}

impl ExecutionAudit {
    #[must_use]
    pub fn new(
        tenant_id: impl Into<String>,
        command: impl Into<String>,
        started_at: impl Into<String>,
        resource_usage: ResourceUsage,
        outcome: ExecOutcome,
        reason: Option<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            command: command.into(),
            started_at: started_at.into(),
            resource_usage,
            outcome,
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutcome {
    Completed,
    QuotaExceeded,
    Denied,
    Failed,
}

#[must_use]
pub fn tenant_root_from_base(base_dir: impl AsRef<Path>, tenant_id: &str) -> PathBuf {
    base_dir.as_ref().join(stable_tenant_dir_name(tenant_id))
}

#[must_use]
pub fn stable_tenant_dir_name(tenant_id: &str) -> String {
    let readable = tenant_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(48)
        .collect::<String>();
    let readable = if readable.is_empty() {
        "tenant".to_string()
    } else {
        readable
    };
    let digest = Sha256::digest(tenant_id.as_bytes());
    format!(
        "{readable}-{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

#[cfg(test)]
mod tests {
    use super::{stable_tenant_dir_name, ResourceQuota, ResourceUsage};
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn quota_exceeded_iff_any_usage_dimension_exceeds(
            cpu_limit in 0_u64..1_000_000,
            mem_limit in 0_u64..1_000_000,
            wall_limit in 0_u64..1_000_000,
            cpu_usage in 0_u64..1_000_001,
            mem_usage in 0_u64..1_000_001,
            wall_usage in 0_u64..1_000_001,
        ) {
            let quota = ResourceQuota::new(cpu_limit, mem_limit, wall_limit);
            let usage = ResourceUsage {
                cpu_millis: cpu_usage,
                memory_bytes: mem_usage,
                wall_time_secs: wall_usage,
            };

            prop_assert_eq!(
                quota.exceeded_by(usage),
                cpu_usage > cpu_limit || mem_usage > mem_limit || wall_usage > wall_limit
            );
        }

        #[test]
        fn stable_tenant_dir_names_are_nonempty_and_deterministic(tenant_id in "\\PC*") {
            let first = stable_tenant_dir_name(&tenant_id);
            let second = stable_tenant_dir_name(&tenant_id);

            prop_assert!(!first.is_empty());
            prop_assert_eq!(&first, &second);
            prop_assert!(!first.contains('/'));
            prop_assert!(!first.contains('\\'));
        }
    }
}
