use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{agent_profiles::RdAgentProfileDto, RdContextProfile};

#[derive(Debug, Deserialize)]
pub(super) struct RdTaskListQuery {
    pub(super) status: Option<String>,
    pub(super) repository_id: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) page: Option<u32>,
    pub(super) per_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskCreateRequest {
    pub(super) repository_id: Option<String>,
    pub(super) spec_id: Option<String>,
    pub(super) agent_profile_id: Option<String>,
    pub(super) workflow_id: Option<String>,
    pub(super) parent_task_id: Option<String>,
    pub(super) baseline_policy: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) context_profile: Option<String>,
    pub(super) context_depth: Option<String>,
    pub(super) should_deep_scan: Option<bool>,
    pub(super) title: Option<String>,
    pub(super) prompt: String,
    pub(super) model: Option<String>,
    pub(super) agent_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntentRouteRequest {
    pub(super) prompt: String,
    pub(super) model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdIntentRouteResponse {
    pub(super) mode: String,
    pub(super) confidence: f32,
    pub(super) reason: Option<String>,
    pub(super) source: String,
    pub(super) model: Option<String>,
    pub(super) profile: String,
    pub(super) profile_name: String,
    pub(super) depth: String,
    pub(super) should_deep_scan: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskDto {
    pub(super) id: String,
    pub(super) thread_id: Option<String>,
    pub(super) parent_task_id: Option<String>,
    pub(super) iteration_no: i32,
    pub(super) thread_title: Option<String>,
    pub(super) repository_id: Option<String>,
    pub(super) spec_id: Option<String>,
    pub(super) agent_profile_id: Option<String>,
    pub(super) workflow_id: Option<String>,
    pub(super) runtime_session_id: Option<String>,
    pub(super) mode: String,
    pub(super) context_profile: Option<String>,
    pub(super) context_profile_name: Option<String>,
    pub(super) context_depth: Option<String>,
    pub(super) should_deep_scan: bool,
    pub(super) status: String,
    pub(super) title: String,
    pub(super) prompt: String,
    pub(super) model: Option<String>,
    pub(super) plan_md: Option<String>,
    pub(super) answer_md: Option<String>,
    pub(super) review_md: Option<String>,
    pub(super) pr_title: Option<String>,
    pub(super) pr_description: Option<String>,
    pub(super) error_message: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: String,
    pub(super) completed_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskListResponse {
    pub(super) tasks: Vec<RdTaskDto>,
    pub(super) total: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdFileChangeDto {
    pub(super) id: String,
    pub(super) task_id: String,
    pub(super) repository_id: Option<String>,
    pub(super) file_path: String,
    pub(super) change_type: String,
    pub(super) diff_patch: String,
    pub(super) applied: bool,
    pub(super) applied_at: Option<String>,
    pub(super) created_at: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdTaskApplyRequest {
    pub(super) change_ids: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskApplyResponse {
    pub(super) applied: usize,
    pub(super) skipped: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskRollbackResponse {
    pub(super) rolled_back: usize,
    pub(super) skipped: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskApplyHunksRequest {
    pub(super) change_id: String,
    pub(super) hunk_indexes: Vec<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTaskApplyHunksResponse {
    pub(super) applied_hunks: usize,
    pub(super) total_hunks: usize,
    pub(super) remaining_change_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RdTaskTestRequest {
    pub(super) command: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTestRunDto {
    pub(super) id: String,
    pub(super) task_id: String,
    pub(super) repository_id: Option<String>,
    pub(super) command: String,
    pub(super) status: String,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout_text: Option<String>,
    pub(super) stderr_text: Option<String>,
    pub(super) duration_ms: Option<i64>,
    pub(super) created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentMarketQuery {
    pub(super) q: Option<String>,
    pub(super) item_type: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentMarketItemDto {
    pub(super) id: String,
    pub(super) item_type: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) tags: Vec<String>,
    pub(super) source: String,
    pub(super) installed: bool,
    pub(super) install_target_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentMarketSearchResponse {
    pub(super) total: usize,
    pub(super) items: Vec<RdAgentMarketItemDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentMarketInstallRequest {
    pub(super) default_model: Option<String>,
    pub(super) enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentMarketInstallResponse {
    pub(super) item: RdAgentMarketItemDto,
    pub(super) agent_profile: Option<RdAgentProfileDto>,
    pub(super) workflow: Option<RdAgentWorkflowDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentWorkflowDto {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) definition_json: Value,
    pub(super) source: String,
    pub(super) source_item_id: Option<String>,
    pub(super) enabled: bool,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdAgentWorkflowRequest {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) definition_json: Value,
    pub(super) enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdWorkflowStageSpec {
    pub(super) id: String,
    pub(super) agent: String,
    pub(super) mode: String,
    pub(super) goal: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RdWorkflowStageKind {
    Preflight,
    MainImplementation,
    Postflight,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RdTokenUsageSnapshot {
    pub(super) input_tokens: u32,
    pub(super) output_tokens: u32,
    pub(super) cache_creation_tokens: u32,
    pub(super) cache_read_tokens: u32,
    pub(super) total_tokens: u32,
    pub(super) model: String,
}

impl RdTokenUsageSnapshot {
    pub(super) fn from_gateway(usage: &agent_gateway::TokenUsageRecord) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            total_tokens: usage.total_tokens,
            model: usage.model.clone(),
        }
    }

    pub(super) fn from_api(usage: &api::Usage, model: &str) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            total_tokens: usage.total_tokens(),
            model: model.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RdIntentRouteDecision {
    pub(super) mode: String,
    pub(super) confidence: f32,
    pub(super) reason: Option<String>,
    pub(super) profile: RdContextProfile,
    pub(super) depth: String,
    pub(super) should_deep_scan: bool,
}

#[derive(Debug, Clone)]
pub(super) struct RdLlmContextPlan {
    pub(super) profile: RdContextProfile,
    pub(super) depth: String,
    pub(super) should_deep_scan: bool,
    pub(super) priority_files: Vec<String>,
    pub(super) search_terms: Vec<String>,
    pub(super) stages: Vec<String>,
    pub(super) stop_conditions: Vec<String>,
    pub(super) reasoning: Option<String>,
    pub(super) confidence: f32,
    pub(super) model: String,
    pub(super) provider: String,
}

#[derive(Debug, Clone)]
pub(super) struct RdTaskContextStrategy {
    pub(super) profile: RdContextProfile,
    pub(super) depth: String,
    pub(super) should_deep_scan: bool,
}

#[derive(Debug, Clone)]
pub(super) struct RdRuntimeToolGovernancePlan {
    pub(super) profile: RdContextProfile,
    pub(super) depth: String,
    pub(super) should_deep_scan: bool,
    pub(super) suggested_read_file_count: usize,
    pub(super) suggested_search_count: usize,
    pub(super) soft_limit_multiplier: f64,
    pub(super) effect_first: bool,
    pub(super) blocking: bool,
    pub(super) strategy: &'static str,
    pub(super) instructions: Vec<String>,
}

impl RdRuntimeToolGovernancePlan {
    pub(super) fn soft_read_threshold(&self) -> usize {
        ((self.suggested_read_file_count as f64 * self.soft_limit_multiplier).ceil() as usize)
            .max(self.suggested_read_file_count)
    }

    pub(super) fn soft_search_threshold(&self) -> usize {
        ((self.suggested_search_count as f64 * self.soft_limit_multiplier).ceil() as usize)
            .max(self.suggested_search_count)
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "profile": self.profile.as_str(),
            "profileName": self.profile.display_name(),
            "depth": &self.depth,
            "shouldDeepScan": self.should_deep_scan,
            "suggestedReadFileCount": self.suggested_read_file_count,
            "suggestedSearchCount": self.suggested_search_count,
            "softReadThreshold": self.soft_read_threshold(),
            "softSearchThreshold": self.soft_search_threshold(),
            "softLimitMultiplier": self.soft_limit_multiplier,
            "effectFirst": self.effect_first,
            "blocking": self.blocking,
            "strategy": self.strategy,
            "instructions": &self.instructions,
        })
    }

    pub(super) fn to_prompt_section(&self) -> String {
        format!(
            "## Runtime 自适应上下文治理计划\n\
             - profile：{}（{}），深度：{}，深度扫描：{}。\n\
             - 建议读取节奏：read_file≈{}，grep/glob≈{}；这是效果优先的软建议，不是机械截断。\n\
             - 软阈值：read_file≈{}，grep/glob≈{}；如果证据不足可以超过，但需要先解释为什么必须扩大范围。\n\
             - 行动原则：\n  - {}",
            self.profile.as_str(),
            self.profile.display_name(),
            self.depth,
            self.should_deep_scan,
            self.suggested_read_file_count,
            self.suggested_search_count,
            self.soft_read_threshold(),
            self.soft_search_threshold(),
            self.instructions.join("\n  - ")
        )
    }
}

#[derive(Debug, Default)]
pub(super) struct RdRepositoryInstructionContext {
    pub(super) text: String,
    pub(super) files: Vec<String>,
}

impl RdRepositoryInstructionContext {
    pub(super) fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}
