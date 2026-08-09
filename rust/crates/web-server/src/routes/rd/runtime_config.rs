//! Runtime feature flags and environment-driven configuration for RD tasks.

use super::{
    DEFAULT_RD_RUNTIME_TIMEOUT_SECS, MAX_RD_RUNTIME_TIMEOUT_SECS, MIN_RD_RUNTIME_TIMEOUT_SECS,
};

pub(super) fn rd_runtime_executor_enabled() -> bool {
    match std::env::var("AOS_RD_RUNTIME_EXECUTOR")
        .or_else(|_| std::env::var("RD_CODE_RUNTIME_EXECUTOR"))
        .unwrap_or_else(|_| "runtime".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "direct" | "completion" => false,
        _ => true,
    }
}

pub(super) fn rd_runtime_direct_fallback_enabled() -> bool {
    parse_rd_runtime_direct_fallback_enabled(
        std::env::var("AOS_RD_RUNTIME_DIRECT_FALLBACK")
            .or_else(|_| std::env::var("RD_CODE_RUNTIME_DIRECT_FALLBACK"))
            .ok()
            .as_deref(),
    )
}

pub(super) fn parse_rd_runtime_direct_fallback_enabled(value: Option<&str>) -> bool {
    matches!(
        value
            .unwrap_or("false")
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "on" | "yes" | "direct" | "completion"
    )
}

pub(super) fn rd_runtime_bash_enabled() -> bool {
    parse_rd_bool_env(
        std::env::var("AOS_RD_RUNTIME_BASH_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_RUNTIME_BASH_ENABLED"))
            .ok()
            .as_deref(),
        false,
    )
}

pub(super) fn rd_runtime_write_tools_enabled() -> bool {
    parse_rd_bool_env(
        std::env::var("AOS_RD_RUNTIME_WRITE_TOOLS_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_RUNTIME_WRITE_TOOLS_ENABLED"))
            .ok()
            .as_deref(),
        false,
    )
}

pub(super) fn rd_candidate_runtime_bash_enabled() -> bool {
    parse_rd_bool_env(
        std::env::var("AOS_RD_CANDIDATE_BASH_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_CANDIDATE_BASH_ENABLED"))
            .ok()
            .as_deref(),
        false,
    )
}

pub(super) fn parse_rd_bool_env(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase) {
        Some(value) if matches!(value.as_str(), "1" | "true" | "on" | "yes") => true,
        Some(value) if matches!(value.as_str(), "0" | "false" | "off" | "no") => false,
        Some(_) => default,
        None => default,
    }
}

pub(super) fn rd_candidate_worktree_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_CANDIDATE_WORKTREE_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_CANDIDATE_WORKTREE_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Whether the candidate worktree should run the repository's test command and
/// iterate on failures (CLI-grade edit→test→fix loop). Defaults ON; disable to
/// fall back to single-shot candidate generation.
pub(super) fn rd_candidate_verify_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_CANDIDATE_VERIFY_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_CANDIDATE_VERIFY_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Maximum number of in-worktree fix turns after a failing test run. The initial
/// editing turn is not counted, so a value of 2 means up to 2 additional fix
/// attempts before the current changes are submitted for human review.
pub(super) fn rd_candidate_max_fix_attempts() -> u32 {
    std::env::var("AOS_RD_CANDIDATE_MAX_FIX_ATTEMPTS")
        .or_else(|_| std::env::var("RD_CODE_CANDIDATE_MAX_FIX_ATTEMPTS"))
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| (1..=6).contains(v))
        .unwrap_or(2)
}

pub(super) fn rd_reviewer_pass_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_REVIEWER_PASS_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_REVIEWER_PASS_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

pub(super) fn rd_architecture_pass_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_ARCHITECTURE_PASS_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_ARCHITECTURE_PASS_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

pub(super) fn rd_runtime_timeout_secs() -> u64 {
    parse_rd_runtime_timeout_secs(
        std::env::var("AOS_RD_RUNTIME_TIMEOUT_SECS")
            .or_else(|_| std::env::var("RD_CODE_RUNTIME_TIMEOUT_SECS"))
            .ok()
            .as_deref(),
    )
}

pub(super) fn parse_rd_runtime_timeout_secs(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v.clamp(MIN_RD_RUNTIME_TIMEOUT_SECS, MAX_RD_RUNTIME_TIMEOUT_SECS))
        .unwrap_or(DEFAULT_RD_RUNTIME_TIMEOUT_SECS)
}

pub(super) fn rd_auto_repair_max_attempts() -> i64 {
    std::env::var("AOS_RD_AUTO_REPAIR_MAX_ATTEMPTS")
        .or_else(|_| std::env::var("RD_CODE_AUTO_REPAIR_MAX_ATTEMPTS"))
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| (0..=10).contains(v))
        .unwrap_or(3)
}

pub(super) fn rd_llm_context_summary_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_LLM_CONTEXT_SUMMARY_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_LLM_CONTEXT_SUMMARY_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

pub(super) fn rd_llm_context_planner_enabled() -> bool {
    !matches!(
        std::env::var("AOS_RD_LLM_CONTEXT_PLANNER_ENABLED")
            .or_else(|_| std::env::var("RD_CODE_LLM_CONTEXT_PLANNER_ENABLED"))
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}
