//! Environment-backed NL2SQL runtime knobs shared outside the web route layer.
//!
//! These helpers intentionally stay free of HTTP/database state so expensive
//! route crates can depend on stable domain policy instead of re-declaring it.

pub const DEFAULT_MAX_AGENT_STEPS: usize = 10;
pub const DEFAULT_MAX_CROSS_DS_TABLES: usize = 4;
pub const DEFAULT_MAX_CROSS_DS_ROWS: usize = 10_000;
pub const DEFAULT_MAX_ROWS_PER_STEP: usize = 10_000;
pub const DEFAULT_MAX_AGENT_RESPONSE_ROWS: usize = 300;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool_default_true(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(true)
}

pub fn max_agent_steps() -> usize {
    env_usize("NL2SQL_MAX_AGENT_STEPS", DEFAULT_MAX_AGENT_STEPS)
}

pub fn max_cross_ds_tables() -> usize {
    env_usize("NL2SQL_MAX_CROSS_DS_TABLES", DEFAULT_MAX_CROSS_DS_TABLES)
}

pub fn max_cross_ds_rows() -> usize {
    env_usize("NL2SQL_MAX_CROSS_DS_ROWS", DEFAULT_MAX_CROSS_DS_ROWS)
}

pub fn max_rows_per_step() -> usize {
    env_usize("NL2SQL_MAX_ROWS_PER_STEP", DEFAULT_MAX_ROWS_PER_STEP)
}

pub fn max_self_correct_attempts() -> usize {
    env_usize("NL2SQL_MAX_SELF_CORRECT_ATTEMPTS", 2)
}

pub fn conversation_summary_threshold() -> u32 {
    env_u32("NL2SQL_CONVERSATION_SUMMARY_THRESHOLD", 5)
}

/// Whether Query Understanding is enabled for NL2SQL query handlers.
pub fn should_enable_qu() -> bool {
    env_bool_default_true("NL2SQL_ENABLE_QUERY_UNDERSTANDING")
}

/// Whether result set validation is enabled after SQL execution.
pub fn should_enable_result_validation() -> bool {
    env_bool_default_true("NL2SQL_ENABLE_RESULT_VALIDATION")
}

/// Whether business domain context is injected into routing prompts.
pub fn should_enable_domain_routing() -> bool {
    !std::env::var("NL2SQL_ENABLE_DOMAIN_ROUTING")
        .ok()
        .map(|v| v == "false")
        .unwrap_or(false)
}

/// Returns the expected embedding dimensions for a model name.
pub fn dimensions_for_model(model: &str) -> usize {
    match model {
        "local/paraphrase-multilingual-minilm-l12-v2-q" => 384,
        "text-embedding-3-small" | "text-embedding-ada-002" => 1536,
        "text-embedding-3-large" => 3072,
        "text-embedding-5-large" => 4096,
        _ => 1536,
    }
}

#[allow(clippy::cast_possible_truncation)]
pub fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(0))
}
