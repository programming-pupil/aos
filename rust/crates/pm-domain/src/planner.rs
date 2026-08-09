use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmExecConstraints {
    pub route_allowlist: Vec<String>,
    pub route_priority: Vec<String>,
    pub source_slot_budget_secs: u64,
    pub tool_budget_per_attempt: usize,
    pub pipeline_timeout_secs: u64,
    pub stop_conditions: Vec<String>,
}

impl PmExecConstraints {
    pub fn new(
        route_allowlist: Vec<String>,
        route_priority: Vec<String>,
        source_slot_budget_secs: u64,
        tool_budget_per_attempt: usize,
        pipeline_timeout_secs: u64,
    ) -> Self {
        Self {
            route_allowlist,
            route_priority,
            source_slot_budget_secs,
            tool_budget_per_attempt,
            pipeline_timeout_secs,
            stop_conditions: vec![
                "final_answer_available".to_string(),
                "retry_budget_exhausted".to_string(),
                "pipeline_budget_exhausted".to_string(),
            ],
        }
    }
}
