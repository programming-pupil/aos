//! Runtime tool allowlist and safety policy for RD agent sessions.

use super::{
    rd_candidate_runtime_bash_enabled, rd_runtime_bash_enabled, rd_runtime_write_tools_enabled,
};

pub(super) use rd_core::runtime_tools::RdRuntimeToolPolicy;

#[cfg(test)]
pub(super) use rd_core::runtime_tools::{
    ensure_rd_candidate_runtime_tools_with_bash, rd_tool_is_bash,
    resolve_rd_runtime_tool_policy_with_switches,
};

pub(super) fn ensure_rd_candidate_runtime_tools(allowed_tools: Option<Vec<String>>) -> Vec<String> {
    rd_core::runtime_tools::ensure_rd_candidate_runtime_tools_with_bash(
        allowed_tools,
        rd_candidate_runtime_bash_enabled(),
    )
}

pub(super) fn resolve_rd_runtime_tool_policy(
    mode: &str,
    allowed_tools: Option<Vec<String>>,
) -> RdRuntimeToolPolicy {
    rd_core::runtime_tools::resolve_rd_runtime_tool_policy_with_switches(
        mode,
        allowed_tools,
        rd_runtime_bash_enabled(),
        rd_runtime_write_tools_enabled(),
    )
}
