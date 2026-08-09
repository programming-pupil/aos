use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmBudgetProfile {
    Normal,
    UnstableRelay,
    ProxyHeavy,
    DeepResearch,
}

impl PmBudgetProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::UnstableRelay => "unstable_relay",
            Self::ProxyHeavy => "proxy_heavy",
            Self::DeepResearch => "deep_research",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "unstable" | "unstable_relay" | "relay" => Self::UnstableRelay,
            "proxy" | "proxy_heavy" => Self::ProxyHeavy,
            "deep" | "deep_research" | "deep-research" => Self::DeepResearch,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmSourceChannel {
    Search,
    Browser,
    ApiFetch,
}

#[derive(Debug, Clone, Copy)]
pub struct PmTimeoutBudget {
    pub pipeline_timeout_secs: u64,
    pub max_attempts: usize,
    pub retrieve_max_tool_calls: usize,
    pub max_calls_per_source: usize,
    pub source_slot_search_secs: u64,
    pub source_slot_browser_secs: u64,
    pub source_slot_api_fetch_secs: u64,
    pub preflight_model_timeout_secs: u64,
    pub preflight_probe_timeout_secs: u64,
    pub preflight_overall_timeout_secs: u64,
    pub retry_step_budget_secs: u64,
    pub retry_total_budget_secs: u64,
}

impl PmTimeoutBudget {
    pub fn source_slot_timeout(self, channel: PmSourceChannel) -> u64 {
        match channel {
            PmSourceChannel::Search => self.source_slot_search_secs,
            PmSourceChannel::Browser => self.source_slot_browser_secs,
            PmSourceChannel::ApiFetch => self.source_slot_api_fetch_secs,
        }
    }

    pub fn baseline_for_profile(profile: PmBudgetProfile) -> Self {
        match profile {
            PmBudgetProfile::Normal => Self {
                // One broad parallel evidence wave plus at most one targeted
                // repair keeps research materially deeper than ordinary chat
                // while leaving a protected synthesis window inside 6.5 min.
                pipeline_timeout_secs: 360,
                max_attempts: 2,
                retrieve_max_tool_calls: 6,
                max_calls_per_source: 3,
                source_slot_search_secs: 75,
                source_slot_browser_secs: 90,
                source_slot_api_fetch_secs: 60,
                preflight_model_timeout_secs: 30,
                preflight_probe_timeout_secs: 10,
                preflight_overall_timeout_secs: 45,
                retry_step_budget_secs: 60,
                retry_total_budget_secs: 120,
            },
            PmBudgetProfile::UnstableRelay => Self {
                pipeline_timeout_secs: 2100,
                max_attempts: 4,
                retrieve_max_tool_calls: 12,
                max_calls_per_source: 3,
                source_slot_search_secs: 300,
                source_slot_browser_secs: 300,
                source_slot_api_fetch_secs: 300,
                preflight_model_timeout_secs: 30,
                preflight_probe_timeout_secs: 12,
                preflight_overall_timeout_secs: 120,
                retry_step_budget_secs: 105,
                retry_total_budget_secs: 520,
            },
            PmBudgetProfile::ProxyHeavy => Self {
                pipeline_timeout_secs: 2400,
                max_attempts: 4,
                retrieve_max_tool_calls: 12,
                max_calls_per_source: 3,
                source_slot_search_secs: 300,
                source_slot_browser_secs: 300,
                source_slot_api_fetch_secs: 300,
                preflight_model_timeout_secs: 30,
                preflight_probe_timeout_secs: 15,
                preflight_overall_timeout_secs: 120,
                retry_step_budget_secs: 120,
                retry_total_budget_secs: 620,
            },
            PmBudgetProfile::DeepResearch => Self {
                // The first attempt fans out across research dimensions. A
                // second attempt is reserved for a fresh, specific evidence gap.
                pipeline_timeout_secs: 360,
                max_attempts: 2,
                retrieve_max_tool_calls: 6,
                max_calls_per_source: 3,
                source_slot_search_secs: 75,
                source_slot_browser_secs: 90,
                source_slot_api_fetch_secs: 60,
                preflight_model_timeout_secs: 30,
                preflight_probe_timeout_secs: 10,
                preflight_overall_timeout_secs: 45,
                retry_step_budget_secs: 60,
                retry_total_budget_secs: 120,
            },
        }
    }

    pub fn from_profile(profile: PmBudgetProfile) -> Self {
        let base = Self::baseline_for_profile(profile);

        Self {
            pipeline_timeout_secs: env_u64("PM_PIPELINE_TIMEOUT_SECS", base.pipeline_timeout_secs),
            max_attempts: env_usize("PM_MAX_ATTEMPTS", base.max_attempts),
            retrieve_max_tool_calls: env_usize(
                "PM_RETRIEVE_MAX_TOOL_CALLS",
                base.retrieve_max_tool_calls,
            ),
            max_calls_per_source: env_usize("PM_MAX_CALLS_PER_SOURCE", base.max_calls_per_source),
            source_slot_search_secs: env_u64(
                "PM_SOURCE_SLOT_TIMEOUT_SEARCH_SECS",
                base.source_slot_search_secs,
            ),
            source_slot_browser_secs: env_u64(
                "PM_SOURCE_SLOT_TIMEOUT_BROWSER_SECS",
                base.source_slot_browser_secs,
            ),
            source_slot_api_fetch_secs: env_u64(
                "PM_SOURCE_SLOT_TIMEOUT_API_FETCH_SECS",
                base.source_slot_api_fetch_secs,
            ),
            preflight_model_timeout_secs: env_u64(
                "PM_PREFLIGHT_MODEL_TIMEOUT_SECS",
                base.preflight_model_timeout_secs,
            ),
            preflight_probe_timeout_secs: env_u64(
                "PM_PREFLIGHT_RETRIEVE_TIMEOUT_SECS",
                base.preflight_probe_timeout_secs,
            ),
            preflight_overall_timeout_secs: env_u64(
                "PM_PREFLIGHT_RETRIEVE_OVERALL_TIMEOUT_SECS",
                base.preflight_overall_timeout_secs,
            ),
            retry_step_budget_secs: env_u64(
                "PM_RETRY_STEP_BUDGET_SECS",
                base.retry_step_budget_secs,
            ),
            retry_total_budget_secs: env_u64(
                "PM_RETRY_TOTAL_BUDGET_SECS",
                base.retry_total_budget_secs,
            ),
        }
    }
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|x| x.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn env_usize(key: &str, fallback: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|x| x.parse::<usize>().ok())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_deep_research_budget_preserves_depth_with_a_bounded_wall_time() {
        let budget = PmTimeoutBudget::baseline_for_profile(PmBudgetProfile::DeepResearch);
        assert_eq!(budget.max_attempts, 2);
        assert_eq!(budget.retrieve_max_tool_calls, 6);
        assert!(budget.max_calls_per_source >= 2);
        assert!(budget.pipeline_timeout_secs <= 360);
        assert!(budget.source_slot_search_secs <= 90);
        assert!(budget.source_slot_browser_secs <= 90);
    }
}
