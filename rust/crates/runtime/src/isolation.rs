use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tenant_sandbox::TenantSandbox;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PathAccess {
    pub path: PathBuf,
    pub write: bool,
}

impl PathAccess {
    #[must_use]
    pub fn read(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write: false,
        }
    }

    #[must_use]
    pub fn write(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationDecision {
    Allow,
    Deny { reason: String },
}

impl IsolationDecision {
    #[must_use]
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[must_use]
pub fn evaluate_path_access(sandbox: &TenantSandbox, access: &PathAccess) -> IsolationDecision {
    if !sandbox.enabled {
        return IsolationDecision::Deny {
            reason: "tenant sandbox is disabled".to_string(),
        };
    }

    let root = normalize_lexical(&sandbox.root);
    let path = if access.path.is_absolute() {
        access.path.clone()
    } else {
        root.join(&access.path)
    };
    let path = normalize_lexical(path);

    if path.starts_with(&root) {
        IsolationDecision::Allow
    } else {
        IsolationDecision::Deny {
            reason: format!(
                "path '{}' escapes tenant root '{}'",
                path.display(),
                root.display()
            ),
        }
    }
}

#[must_use]
pub fn tenant_roots_disjoint(left: &TenantSandbox, right: &TenantSandbox) -> bool {
    if left.tenant_id == right.tenant_id {
        return false;
    }
    let left_root = normalize_lexical(&left.root);
    let right_root = normalize_lexical(&right.root);
    left_root != right_root
        && !left_root.starts_with(&right_root)
        && !right_root.starts_with(&left_root)
}

#[must_use]
pub fn normalize_lexical(path: impl AsRef<Path>) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.as_ref().components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use proptest::prelude::*;

    use crate::{
        egress::EgressPolicy,
        isolation::{evaluate_path_access, tenant_roots_disjoint, IsolationDecision, PathAccess},
        tenant_sandbox::{ResourceQuota, TenantSandbox},
    };

    fn tenant_name() -> impl Strategy<Value = String> {
        "[a-z0-9_-]{1,16}".prop_map(|s| s)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_20_denies_paths_outside_the_tenant_root(
            tenant in tenant_name(),
            file in "[a-z0-9_-]{1,16}",
        ) {
            // Feature: codex-parity-gaps, Property 20: （OPTIONAL）租户隔离与防逃逸
            let sandbox = TenantSandbox::new(
                tenant,
                PathBuf::from("/srv/aos/tenants/current"),
                ResourceQuota::default(),
                EgressPolicy::deny_all(),
            );

            let allowed = evaluate_path_access(
                &sandbox,
                &PathAccess::read(PathBuf::from(format!("/srv/aos/tenants/current/{file}"))),
            );
            let denied = evaluate_path_access(
                &sandbox,
                &PathAccess::read(PathBuf::from(format!("/srv/aos/tenants/other/{file}"))),
            );
            let parent_escape = evaluate_path_access(
                &sandbox,
                &PathAccess::read(PathBuf::from(format!("../other/{file}"))),
            );

            prop_assert!(allowed.is_allow());
            prop_assert!(
                matches!(denied, IsolationDecision::Deny { .. }),
                "outside tenant root must be denied"
            );
            prop_assert!(
                matches!(parent_escape, IsolationDecision::Deny { .. }),
                "parent path escape must be denied"
            );
        }

        #[test]
        fn property_20_different_tenant_roots_are_disjoint(
            left in tenant_name(),
            right in tenant_name(),
        ) {
            // Feature: codex-parity-gaps, Property 20: （OPTIONAL）租户隔离与防逃逸
            prop_assume!(left != right);
            let left_sandbox = TenantSandbox::new(
                left.clone(),
                PathBuf::from(format!("/srv/aos/tenants/{left}")),
                ResourceQuota::default(),
                EgressPolicy::deny_all(),
            );
            let right_sandbox = TenantSandbox::new(
                right.clone(),
                PathBuf::from(format!("/srv/aos/tenants/{right}")),
                ResourceQuota::default(),
                EgressPolicy::deny_all(),
            );

            prop_assert!(tenant_roots_disjoint(&left_sandbox, &right_sandbox));
        }
    }
}
