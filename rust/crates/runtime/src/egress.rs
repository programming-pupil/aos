use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressDecision {
    Allow,
    #[default]
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetTarget {
    pub scheme: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl NetTarget {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            scheme: None,
            host: host.into(),
            port: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EgressPolicy {
    pub default: EgressDecision,
    pub rules: Vec<EgressRule>,
}

impl EgressPolicy {
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            default: EgressDecision::Deny,
            rules: Vec::new(),
        }
    }

    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            default: EgressDecision::Allow,
            rules: Vec::new(),
        }
    }
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self::deny_all()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EgressRule {
    pub decision: EgressDecision,
    pub host: HostMatcher,
    pub scheme: Option<String>,
    pub port: Option<u16>,
}

impl EgressRule {
    #[must_use]
    pub fn allow_exact_host(host: impl Into<String>) -> Self {
        Self {
            decision: EgressDecision::Allow,
            host: HostMatcher::Exact(host.into()),
            scheme: None,
            port: None,
        }
    }

    #[must_use]
    pub fn deny_exact_host(host: impl Into<String>) -> Self {
        Self {
            decision: EgressDecision::Deny,
            host: HostMatcher::Exact(host.into()),
            scheme: None,
            port: None,
        }
    }

    #[must_use]
    pub fn matches(&self, target: &NetTarget) -> bool {
        if self
            .scheme
            .as_deref()
            .is_some_and(|scheme| !eq_ascii_case_trimmed(Some(scheme), target.scheme.as_deref()))
        {
            return false;
        }
        if self.port.is_some_and(|port| target.port != Some(port)) {
            return false;
        }
        self.host.matches(&target.host)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum HostMatcher {
    Any,
    Exact(String),
    Suffix(String),
}

impl HostMatcher {
    #[must_use]
    pub fn matches(&self, host: &str) -> bool {
        let host = normalize_host(host);
        match self {
            Self::Any => true,
            Self::Exact(expected) => host == normalize_host(expected),
            Self::Suffix(suffix) => {
                let suffix = normalize_host(suffix).trim_start_matches('.').to_string();
                host == suffix || host.ends_with(&format!(".{suffix}"))
            }
        }
    }
}

#[must_use]
pub fn evaluate_egress(policy: &EgressPolicy, dst: &NetTarget) -> EgressDecision {
    policy
        .rules
        .iter()
        .find(|rule| rule.matches(dst))
        .map_or(policy.default, |rule| rule.decision)
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn eq_ascii_case_trimmed(left: Option<&str>, right: Option<&str>) -> bool {
    left.map(str::trim).map(str::to_ascii_lowercase)
        == right.map(str::trim).map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_egress, EgressDecision, EgressPolicy, EgressRule, HostMatcher, NetTarget,
    };
    use proptest::prelude::*;

    fn host_segment() -> impl Strategy<Value = String> {
        "[a-z0-9]{1,12}".prop_map(|s| s)
    }

    fn host() -> impl Strategy<Value = String> {
        prop::collection::vec(host_segment(), 1..5).prop_map(|parts| parts.join("."))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn property_17_egress_decision_matches_first_rule(
            allowed_host in host(),
            denied_host in host(),
            fallback_allow in any::<bool>(),
        ) {
            // Feature: codex-parity-gaps, Property 17: （OPTIONAL）Egress 决策符合策略
            let policy = EgressPolicy {
                default: if fallback_allow { EgressDecision::Allow } else { EgressDecision::Deny },
                rules: vec![
                    EgressRule::deny_exact_host(denied_host.clone()),
                    EgressRule::allow_exact_host(allowed_host.clone()),
                ],
            };

            prop_assert_eq!(
                evaluate_egress(&policy, &NetTarget::new(denied_host)),
                EgressDecision::Deny
            );
            prop_assert_eq!(
                evaluate_egress(&policy, &NetTarget::new(allowed_host)),
                EgressDecision::Allow
            );
            prop_assert_eq!(
                evaluate_egress(&policy, &NetTarget::new("not-listed.example")),
                policy.default
            );
        }

        #[test]
        fn suffix_rule_matches_subdomains(base in host(), child in host_segment()) {
            // Feature: codex-parity-gaps, Property 17: （OPTIONAL）Egress 决策符合策略
            let policy = EgressPolicy {
                default: EgressDecision::Deny,
                rules: vec![EgressRule {
                    decision: EgressDecision::Allow,
                    host: HostMatcher::Suffix(base.clone()),
                    scheme: None,
                    port: None,
                }],
            };

            prop_assert_eq!(
                evaluate_egress(&policy, &NetTarget::new(format!("{child}.{base}"))),
                EgressDecision::Allow
            );
            prop_assert_eq!(
                evaluate_egress(&policy, &NetTarget::new(base)),
                EgressDecision::Allow
            );
        }
    }
}
