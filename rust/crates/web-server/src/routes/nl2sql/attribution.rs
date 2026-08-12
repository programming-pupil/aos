use super::agent::{execute_agent_request, execute_agent_request_with_budget};
use super::agent_async::{with_agent_stage_emitter, AgentStageSignal};
use super::agent_executor::DatasourceRequestBudget;
use super::reference::ReferenceUsageDto;
use super::{AgentExecuteRequest, PaginationParams};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::agent_ops::{self, CreateAgentTaskInput};
use crate::state::AppState;
use api::{InputContentBlock, InputMessage, MessageRequest, OutputContentBlock};
use axum::extract::{Extension, Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::FutureExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row};
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit, Semaphore};

const SUPER_ASSISTANT_EXACT_RECENT_TAIL_HEADER: &str =
    "最近会话原文（按时间从旧到新；原文保留，只用于承接上下文，不得覆盖当前问题）：";
const ATTRIBUTION_TASK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AttributionAnalyzeRequest {
    pub question: String,
    /// Model selected by the parent assistant turn. Authorized alternatives
    /// remain available as failover candidates.
    #[serde(default)]
    pub preferred_model: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub datasource_ids: Vec<String>,
    #[serde(default)]
    pub depth: Option<AttributionDepth>,
    #[serde(skip)]
    pub(crate) network_budget: Option<Arc<DatasourceRequestBudget>>,
}

impl Default for AttributionAnalyzeRequest {
    fn default() -> Self {
        Self {
            question: String::new(),
            preferred_model: None,
            conversation_id: None,
            context: None,
            datasource_ids: Vec::new(),
            depth: None,
            network_budget: None,
        }
    }
}

#[derive(Debug, Clone)]
struct PreviousAttributionContext {
    task_id: String,
    question: String,
    response: AttributionAnalyzeResponse,
    evidence_cards: Vec<AttributionEvidenceCard>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionDepth {
    Fast,
    Standard,
    Deep,
}

impl AttributionDepth {
    fn max_steps(self) -> usize {
        match self {
            Self::Fast => 3,
            Self::Standard => 4,
            Self::Deep => 8,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }

    fn agent_max_steps(self) -> usize {
        match self {
            Self::Fast => 2,
            Self::Standard => 3,
            Self::Deep => 4,
        }
    }
}

fn max_observation_rows() -> usize {
    env::var("NL2SQL_ATTRIBUTION_OBSERVATION_ROWS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(80)
}

fn attribution_query_concurrency() -> usize {
    env::var("NL2SQL_ATTRIBUTION_QUERY_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 4))
        .unwrap_or(2)
}

fn attribution_total_budget(depth: AttributionDepth) -> Duration {
    let (env_name, default_secs) = match depth {
        AttributionDepth::Fast => ("NL2SQL_ATTRIBUTION_FAST_TIMEOUT_SECS", 3 * 60),
        AttributionDepth::Standard => ("NL2SQL_ATTRIBUTION_STANDARD_TIMEOUT_SECS", 6 * 60),
        AttributionDepth::Deep => ("NL2SQL_ATTRIBUTION_DEEP_TIMEOUT_SECS", 8 * 60),
    };
    Duration::from_secs(
        env::var(env_name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 60)
            .unwrap_or(default_secs),
    )
}

fn remaining_attribution_budget(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn attribution_planning_budget() -> Duration {
    Duration::from_secs(
        env::var("NL2SQL_ATTRIBUTION_PLANNING_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 10)
            .unwrap_or(45),
    )
}

fn attribution_step_budget(depth: AttributionDepth) -> Duration {
    let default_secs = match depth {
        AttributionDepth::Fast => 75,
        AttributionDepth::Standard => 120,
        AttributionDepth::Deep => 150,
    };
    Duration::from_secs(
        env::var("NL2SQL_ATTRIBUTION_STEP_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 30)
            .unwrap_or(default_secs),
    )
}

fn attribution_step_budget_for(depth: AttributionDepth, step: &AttributionPlanStep) -> Duration {
    let base = attribution_step_budget(depth);
    let extra_secs = match step.id.as_str() {
        "main_metric" => match depth {
            AttributionDepth::Fast => 30,
            AttributionDepth::Standard => 45,
            AttributionDepth::Deep => 60,
        },
        "metric_decomposition" | "dimension_drilldown" => 20,
        _ => 0,
    };
    base.saturating_add(Duration::from_secs(extra_secs))
}

fn attribution_primary_attempt_deadline(step_deadline: Instant) -> Instant {
    let remaining = remaining_attribution_budget(step_deadline);
    if remaining <= Duration::from_secs(45) {
        return step_deadline;
    }
    let reserve = (remaining / 3)
        .max(Duration::from_secs(35))
        .min(Duration::from_secs(70));
    step_deadline.checked_sub(reserve).unwrap_or(step_deadline)
}

fn attribution_synthesis_reserve() -> Duration {
    Duration::from_secs(
        env::var("NL2SQL_ATTRIBUTION_SYNTHESIS_RESERVE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 20)
            .unwrap_or(55),
    )
}

fn bounded_phase_deadline(overall_deadline: Instant, budget: Duration) -> Instant {
    overall_deadline.min(Instant::now() + budget)
}

fn attribution_execution_deadline(overall_deadline: Instant) -> Instant {
    overall_deadline
        .checked_sub(attribution_synthesis_reserve())
        .unwrap_or(overall_deadline)
}

fn attribution_helper_model_budget() -> Duration {
    Duration::from_secs(
        env::var("NL2SQL_ATTRIBUTION_HELPER_MODEL_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value >= 10)
            .unwrap_or(45),
    )
}

fn max_evidence_card_rows() -> usize {
    env::var("NL2SQL_ATTRIBUTION_EVIDENCE_CARD_ROWS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(12)
}

fn max_diagnostic_digest_rows() -> usize {
    env::var("NL2SQL_ATTRIBUTION_DIAGNOSTIC_DIGEST_ROWS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

fn max_evidence_card_columns() -> usize {
    env::var("NL2SQL_ATTRIBUTION_EVIDENCE_CARD_COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(28)
}

fn format_attribution_panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

impl Default for AttributionDepth {
    fn default() -> Self {
        Self::Standard
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionPlan {
    #[serde(default)]
    pub needs_clarification: bool,
    #[serde(default)]
    pub clarification_question: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub analysis_focus: Vec<String>,
    #[serde(default)]
    pub steps: Vec<AttributionPlanStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionPlanStep {
    pub id: String,
    pub title: String,
    pub purpose: String,
    pub question: String,
    #[serde(default)]
    pub priority: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticFollowupPlan {
    #[serde(default)]
    done: bool,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    steps: Vec<AttributionPlanStep>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FollowupContextAnswer {
    #[serde(default)]
    answerable: bool,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    report: Option<AttributionReport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObservationDigest {
    step_id: String,
    title: String,
    purpose: String,
    question: String,
    columns: Vec<String>,
    row_count: usize,
    sampled: bool,
    rows: Vec<serde_json::Value>,
    error: Option<String>,
    sql_count: usize,
    reference_files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionEvidenceCard {
    pub step_id: String,
    pub title: String,
    pub purpose: String,
    pub question: String,
    #[serde(default)]
    pub datasource_ids: Vec<String>,
    #[serde(default)]
    pub time_context: Option<String>,
    pub status: String,
    pub row_count: usize,
    #[serde(default)]
    pub sampled: bool,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows_preview: Vec<serde_json::Value>,
    #[serde(default)]
    pub numeric_highlights: Vec<String>,
    #[serde(default)]
    pub sql_count: usize,
    #[serde(default)]
    pub reference_files: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<AttributionEvidenceRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionEvidenceRef {
    pub row_index: usize,
    pub column: String,
    pub value_preview: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionObservation {
    pub step_id: String,
    pub title: String,
    pub purpose: String,
    pub question: String,
    #[serde(default)]
    pub datasource_ids: Vec<String>,
    #[serde(default)]
    pub time_context: Option<String>,
    pub query_id: Option<String>,
    pub conversation_id: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub row_count: usize,
    pub sampled: bool,
    pub sqls: Vec<String>,
    pub used_references: Vec<ReferenceUsageDto>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

impl AttributionObservation {
    fn execution_succeeded(&self) -> bool {
        self.error.is_none() && self.sqls.iter().any(|sql| !sql.trim().is_empty())
    }

    fn has_usable_evidence(&self) -> bool {
        self.execution_succeeded()
            && self.row_count > 0
            && (!self.rows.is_empty() || !self.columns.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionDriver {
    pub title: String,
    pub explanation: String,
    #[serde(default)]
    pub impact: Option<String>,
    #[serde(default)]
    pub evidence_step_ids: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionReport {
    pub title: String,
    pub executive_summary: String,
    #[serde(default)]
    pub metric_answer: Option<String>,
    #[serde(default)]
    pub main_causes: Vec<AttributionDriver>,
    #[serde(default)]
    pub recommendations: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub next_questions: Vec<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub coverage: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionEvidenceHealth {
    pub total_steps: usize,
    #[serde(default)]
    pub execution_succeeded_steps: usize,
    #[serde(default)]
    pub usable_evidence_steps: usize,
    #[serde(default)]
    pub zero_row_steps: usize,
    pub successful_steps: usize,
    pub failed_steps: usize,
    pub sampled_steps: usize,
    pub total_rows: usize,
}

impl AttributionEvidenceHealth {
    fn empty() -> Self {
        Self {
            total_steps: 0,
            execution_succeeded_steps: 0,
            usable_evidence_steps: 0,
            zero_row_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            sampled_steps: 0,
            total_rows: 0,
        }
    }

    fn from_observations(observations: &[AttributionObservation]) -> Self {
        let execution_succeeded_steps = observations
            .iter()
            .filter(|observation| observation.execution_succeeded())
            .count();
        let usable_evidence_steps = observations
            .iter()
            .filter(|observation| observation.has_usable_evidence())
            .count();
        let zero_row_steps = observations
            .iter()
            .filter(|observation| observation.execution_succeeded() && observation.row_count == 0)
            .count();
        Self {
            total_steps: observations.len(),
            execution_succeeded_steps,
            usable_evidence_steps,
            zero_row_steps,
            successful_steps: usable_evidence_steps,
            failed_steps: observations.len().saturating_sub(execution_succeeded_steps),
            sampled_steps: observations.iter().filter(|o| o.sampled).count(),
            total_rows: observations.iter().map(|o| o.row_count).sum(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributionAnalyzeResponse {
    pub status: String,
    pub question: String,
    pub depth: String,
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<AttributionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<AttributionPlan>,
    #[serde(default)]
    pub observations: Vec<AttributionObservation>,
    pub evidence_health: AttributionEvidenceHealth,
    #[serde(default)]
    pub evidence_cards: Vec<AttributionEvidenceCard>,
    pub total_execution_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct AttributionTaskEvent {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<AttributionObservation>,
    pub response: Option<AttributionAnalyzeResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartAttributionTaskResponse {
    pub task_id: String,
    pub status: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributionConversationItem {
    pub id: String,
    pub message_count: i64,
    pub summary: Option<String>,
    pub last_question: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributionConversationListResponse {
    pub total: usize,
    pub conversations: Vec<AttributionConversationItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributionConversationTaskItem {
    pub task_id: String,
    pub conversation_id: String,
    pub question: String,
    pub depth: String,
    pub status: String,
    pub summary: Option<String>,
    pub response: Option<AttributionAnalyzeResponse>,
    pub error: Option<String>,
    pub total_execution_ms: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributionConversationDetailResponse {
    pub id: String,
    pub message_count: i64,
    pub summary: Option<String>,
    pub last_question: Option<String>,
    pub tasks: Vec<AttributionConversationTaskItem>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttributionTaskStatusResponse {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<AttributionObservation>,
    pub response: Option<AttributionAnalyzeResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct AttributionTaskRecord {
    tenant_id: String,
    user_id: String,
    created_at: Instant,
    completed_at: Option<Instant>,
    last_event: AttributionTaskEvent,
    events: Vec<AttributionTaskEvent>,
    done: bool,
    cancel_requested: bool,
}

#[derive(Debug, Clone, Copy)]
struct AttributionTaskConfig {
    max_concurrent_running: usize,
    max_tasks_in_memory: usize,
    task_ttl: Duration,
    cleanup_interval: Duration,
}

impl AttributionTaskConfig {
    fn from_env() -> Self {
        fn read_usize(name: &str, default: usize) -> usize {
            env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }

        fn read_u64(name: &str, default: u64) -> u64 {
            env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }

        Self {
            max_concurrent_running: read_usize("NL2SQL_ATTRIBUTION_TASK_MAX_CONCURRENT", 4),
            max_tasks_in_memory: read_usize("NL2SQL_ATTRIBUTION_TASK_MAX_IN_MEMORY", 1000),
            task_ttl: Duration::from_secs(read_u64("NL2SQL_ATTRIBUTION_TASK_TTL_SECS", 3600)),
            cleanup_interval: Duration::from_secs(read_u64(
                "NL2SQL_ATTRIBUTION_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
        }
    }
}

#[derive(Clone)]
struct AttributionTaskManager {
    inner: Arc<Mutex<HashMap<String, AttributionTaskRecord>>>,
    senders: Arc<Mutex<HashMap<String, broadcast::Sender<AttributionTaskEvent>>>>,
    run_slots: Arc<Semaphore>,
    config: AttributionTaskConfig,
}

impl AttributionTaskManager {
    fn new() -> Self {
        let config = AttributionTaskConfig::from_env();
        let manager = Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            run_slots: Arc::new(Semaphore::new(config.max_concurrent_running)),
            config,
        };
        manager.start_cleanup_loop();
        manager
    }

    fn start_cleanup_loop(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(manager.config.cleanup_interval).await;
                let _ = manager.cleanup_expired().await;
            }
        });
    }

    async fn ensure_sender(&self, task_id: &str) -> broadcast::Sender<AttributionTaskEvent> {
        let mut map = self.senders.lock().await;
        if let Some(s) = map.get(task_id) {
            return s.clone();
        }
        let (tx, _) = broadcast::channel(256);
        map.insert(task_id.to_string(), tx.clone());
        tx
    }

    async fn create_task(&self, task_id: &str, tenant_id: &str, user_id: &str) -> Result<()> {
        self.cleanup_expired().await;
        let initial = AttributionTaskEvent {
            task_id: task_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            message: Some("已加入数据归因队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            progress_percent: Some(3),
            step_index: None,
            step_total: None,
            observation: None,
            response: None,
            error: None,
        };
        let record = AttributionTaskRecord {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            created_at: Instant::now(),
            completed_at: None,
            last_event: initial.clone(),
            events: vec![initial.clone()],
            done: false,
            cancel_requested: false,
        };
        {
            let mut guard = self.inner.lock().await;
            if guard.len() >= self.config.max_tasks_in_memory {
                return Err(AppError::TooManyRequests(format!(
                    "too many nl2sql attribution tasks in memory (limit: {})",
                    self.config.max_tasks_in_memory
                )));
            }
            guard.insert(task_id.to_string(), record);
        }
        let tx = self.ensure_sender(task_id).await;
        let _ = tx.send(initial);
        Ok(())
    }

    async fn publish_stage_progress(
        &self,
        task_id: &str,
        stage: &str,
        message: &str,
        progress_percent: Option<u8>,
        step_index: Option<usize>,
        step_total: Option<usize>,
        observation: Option<AttributionObservation>,
    ) -> Option<AttributionTaskEvent> {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            if rec.done || rec.cancel_requested {
                return None;
            }
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let evt = AttributionTaskEvent {
                task_id: task_id.to_string(),
                status: "running".to_string(),
                stage: Some(stage.to_string()),
                message: Some(message.to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                progress_percent,
                step_index,
                step_total,
                observation,
                response: None,
                error: None,
            };
            rec.last_event = evt.clone();
            rec.events.push(evt.clone());
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt.clone());
            return Some(evt);
        }
        None
    }

    async fn publish_completed(&self, task_id: &str, response: AttributionAnalyzeResponse) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            if rec.done || rec.cancel_requested {
                return;
            }
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let status = if response.error.is_some() {
                "failed"
            } else if response.clarification_question.is_some() {
                "clarification_needed"
            } else if matches!(
                response.status.as_str(),
                "completed" | "partial" | "no_data"
            ) {
                response.status.as_str()
            } else {
                "completed"
            };
            let message = match status {
                "clarification_needed" => "需要补充关键信息",
                "failed" => "数据归因失败",
                "partial" => "数据归因已基于现有证据完成",
                "no_data" => "数据归因未取得可用数据",
                _ => "数据归因完成",
            };
            let evt = AttributionTaskEvent {
                task_id: task_id.to_string(),
                status: status.to_string(),
                stage: Some(status.to_string()),
                message: Some(message.to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                progress_percent: Some(100),
                step_index: None,
                step_total: None,
                observation: None,
                response: Some(response),
                error: None,
            };
            rec.last_event = evt.clone();
            rec.events.push(evt.clone());
            rec.done = true;
            rec.completed_at = Some(Instant::now());
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    async fn publish_failed(&self, task_id: &str, error: String) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            if rec.done || rec.cancel_requested {
                return;
            }
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let evt = AttributionTaskEvent {
                task_id: task_id.to_string(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("数据归因失败".to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                progress_percent: Some(100),
                step_index: None,
                step_total: None,
                observation: None,
                response: None,
                error: Some(error),
            };
            rec.last_event = evt.clone();
            rec.events.push(evt.clone());
            rec.done = true;
            rec.completed_at = Some(Instant::now());
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    async fn cancel(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<AttributionTaskEvent> {
        let mut guard = self.inner.lock().await;
        let rec = guard
            .get_mut(task_id)
            .filter(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)?;
        if rec.done {
            return Some(rec.last_event.clone());
        }
        let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
        let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
        let evt = AttributionTaskEvent {
            task_id: task_id.to_string(),
            status: "cancelled".to_string(),
            stage: Some("cancelled".to_string()),
            message: Some("数据归因任务已取消".to_string()),
            elapsed_ms: now_elapsed,
            stage_elapsed_ms: Some(stage_elapsed),
            progress_percent: Some(100),
            step_index: None,
            step_total: None,
            observation: None,
            response: None,
            error: None,
        };
        rec.cancel_requested = true;
        rec.done = true;
        rec.completed_at = Some(Instant::now());
        rec.last_event = evt.clone();
        rec.events.push(evt.clone());
        drop(guard);
        let tx = self.ensure_sender(task_id).await;
        let _ = tx.send(evt.clone());
        Some(evt)
    }

    async fn is_cancelled(&self, task_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.cancel_requested || rec.last_event.status == "cancelled")
            .unwrap_or(false)
    }

    async fn snapshot(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<AttributionTaskEvent> {
        self.inner
            .lock()
            .await
            .get(task_id)
            .filter(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .map(|rec| rec.last_event.clone())
    }

    async fn history(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<Vec<AttributionTaskEvent>> {
        self.inner
            .lock()
            .await
            .get(task_id)
            .filter(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .map(|rec| rec.events.clone())
    }

    async fn subscribe(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<broadcast::Receiver<AttributionTaskEvent>> {
        let owner_ok = self
            .inner
            .lock()
            .await
            .get(task_id)
            .map(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .unwrap_or(false);
        if !owner_ok {
            return None;
        }
        let tx = self.ensure_sender(task_id).await;
        Some(tx.subscribe())
    }

    fn try_acquire_run_slot(&self) -> Result<OwnedSemaphorePermit> {
        self.run_slots.clone().try_acquire_owned().map_err(|_| {
            AppError::TooManyRequests(format!(
                "too many concurrent nl2sql attribution tasks (limit: {})",
                self.config.max_concurrent_running
            ))
        })
    }

    async fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let ttl = self.config.task_ttl;
        let mut expired_ids: Vec<String> = Vec::new();
        {
            let mut guard = self.inner.lock().await;
            guard.retain(|task_id, rec| {
                let expired = rec.done
                    && rec
                        .completed_at
                        .map(|done_at| now.duration_since(done_at) >= ttl)
                        .unwrap_or(false);
                if expired {
                    expired_ids.push(task_id.clone());
                    return false;
                }
                true
            });
        }
        if expired_ids.is_empty() {
            return 0;
        }
        let mut senders = self.senders.lock().await;
        for task_id in &expired_ids {
            senders.remove(task_id);
        }
        expired_ids.len()
    }
}

fn task_manager() -> &'static AttributionTaskManager {
    static MANAGER: OnceLock<AttributionTaskManager> = OnceLock::new();
    MANAGER.get_or_init(AttributionTaskManager::new)
}

pub(crate) async fn load_attribution_task_progress_events(
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
    after_event_id: u64,
    limit: usize,
) -> Vec<(u64, AttributionTaskEvent)> {
    let Some(history) = task_manager().history(task_id, tenant_id, user_id).await else {
        return Vec::new();
    };
    let after = usize::try_from(after_event_id).unwrap_or(usize::MAX);
    history
        .into_iter()
        .enumerate()
        .skip(after)
        .take(limit.clamp(1, 256))
        .filter_map(|(index, event)| {
            u64::try_from(index.saturating_add(1))
                .ok()
                .map(|event_id| (event_id, event))
        })
        .collect()
}

pub(crate) async fn attribution_task_progress_snapshot(
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Option<AttributionTaskEvent> {
    task_manager().snapshot(task_id, tenant_id, user_id).await
}

fn attribution_conversation_id(input: Option<String>) -> String {
    input
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("attr-{}", uuid::Uuid::new_v4()))
}

fn attribution_response_summary(resp: &AttributionAnalyzeResponse) -> Option<String> {
    resp.report.as_ref().map(|report| {
        let mut summary = report.title.trim().to_string();
        if !report.executive_summary.trim().is_empty() {
            if !summary.is_empty() {
                summary.push_str("：");
            }
            summary.push_str(report.executive_summary.trim());
        }
        if summary.chars().count() > 500 {
            summary.chars().take(500).collect()
        } else {
            summary
        }
    })
}

fn super_assistant_session_id_from_attribution_conversation(conversation_id: &str) -> Option<&str> {
    conversation_id
        .trim()
        .strip_prefix("super-assistant-")
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
}

fn format_attribution_terminal_message(response: &AttributionAnalyzeResponse) -> String {
    if let Some(question) = response.clarification_question.as_deref() {
        return format!("需要补充信息：{}", question.trim())
            .trim()
            .to_string();
    }
    let mut lines = Vec::new();
    if let Some(report) = response.report.as_ref() {
        let title = report.title.trim();
        if !title.is_empty() {
            lines.push(format!("## {title}"));
        }
        let executive_summary = report.executive_summary.trim();
        if !executive_summary.is_empty() {
            lines.push(executive_summary.to_string());
        }
        if let Some(metric_answer) = report.metric_answer.as_deref() {
            let metric_answer = metric_answer.trim();
            if !metric_answer.is_empty() {
                lines.push(format!("\n**核心结论**\n{metric_answer}"));
            }
        }
        if !report.main_causes.is_empty() {
            lines.push("\n**主要原因**".to_string());
            for cause in report.main_causes.iter().take(5) {
                let title = cause.title.trim();
                let explanation = cause.explanation.trim();
                if !title.is_empty() || !explanation.is_empty() {
                    lines.push(format!("- {title}: {explanation}"));
                }
            }
        }
        if !report.recommendations.is_empty() {
            lines.push("\n**建议动作**".to_string());
            for item in report.recommendations.iter().take(5) {
                let item = item.trim();
                if !item.is_empty() {
                    lines.push(format!("- {item}"));
                }
            }
        }
        if !report.caveats.is_empty() {
            lines.push("\n**注意事项**".to_string());
            for item in report.caveats.iter().take(3) {
                let item = item.trim();
                if !item.is_empty() {
                    lines.push(format!("- {item}"));
                }
            }
        }
    }
    lines.push(format!(
        "\n证据覆盖：成功 {}/{} 步，返回 {} 行。",
        response.evidence_health.successful_steps,
        response.evidence_health.total_steps,
        response.evidence_health.total_rows
    ));
    let text = lines.join("\n").trim().to_string();
    if !text.is_empty() {
        return text;
    }
    response
        .error
        .clone()
        .unwrap_or_else(|| "数据归因任务已完成。".to_string())
}

fn attribution_observation_tool_calls(
    observations: &[AttributionObservation],
) -> Vec<agent_gateway::ToolCallRecord> {
    observations
        .iter()
        .enumerate()
        .map(|(index, observation)| {
            let input = serde_json::to_string_pretty(&serde_json::json!({
                "stepId": observation.step_id,
                "title": observation.title,
                "purpose": observation.purpose,
                "question": observation.question,
                "sqls": observation.sqls,
                "queryId": observation.query_id,
                "conversationId": observation.conversation_id,
            }))
            .unwrap_or_else(|_| observation.question.clone());
            let output = serde_json::to_string_pretty(&serde_json::json!({
                "columns": observation.columns,
                "rows": observation.rows,
                "rowCount": observation.row_count,
                "sampled": observation.sampled,
                "usedReferences": observation.used_references,
                "error": observation.error,
            }))
            .unwrap_or_else(|_| {
                observation
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("returned {} rows", observation.row_count))
            });
            agent_gateway::ToolCallRecord {
                index: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
                tool_name: "nl2sql_attribution_query".to_string(),
                source: "builtin".to_string(),
                source_name: "nl2sql_attribution".to_string(),
                input,
                output,
                is_error: observation.error.is_some(),
                duration_ms: observation.elapsed_ms,
            }
        })
        .collect()
}

async fn persist_attribution_turn_exact_archive(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    conversation_id: Option<&str>,
    question: &str,
    assistant_text: &str,
    observations: &[AttributionObservation],
    had_shared_context: bool,
    status: &str,
) {
    let Some(conversation_id) = conversation_id
        .map(str::trim)
        .filter(|conversation_id| !conversation_id.is_empty())
    else {
        return;
    };
    let session_id = super_assistant_session_id_from_attribution_conversation(conversation_id)
        .unwrap_or(conversation_id);
    let tool_calls = attribution_observation_tool_calls(observations);
    crate::routes::memory_continuity::persist_agent_turn_exact_archive(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        session_id,
        "nl2sql",
        task_id,
        question,
        None,
        assistant_text,
        &tool_calls,
        Some(serde_json::json!({
            "source": "nl2sql_attribution",
            "taskId": task_id,
            "conversationId": conversation_id,
            "status": status,
            "hadSharedContext": had_shared_context,
            "observationCount": observations.len()
        })),
    )
    .await;
}

async fn persist_attribution_terminal_message_to_super_assistant_session(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    conversation_id: Option<&str>,
    text: String,
) {
    let Some(session_id) =
        conversation_id.and_then(super_assistant_session_id_from_attribution_conversation)
    else {
        return;
    };
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if let Err(error) = state
        .agent_manager()
        .append_visible_message_when_idle(
            session_id,
            &claims.tenant_id,
            &claims.sub,
            runtime::MessageRole::Assistant,
            text.to_string(),
            Duration::from_secs(120),
        )
        .await
    {
        tracing::warn!(
            task_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            session_id,
            error = %error,
            "failed to persist attribution terminal message to super assistant session"
        );
    }
}

async fn upsert_attribution_conversation(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    conversation_id: &str,
    question: &str,
) {
    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_attribution_conversations \
            (id, tenant_id, user_id, message_count, last_question) \
         VALUES (?, ?, ?, 1, ?) \
         ON CONFLICT DO UPDATE SET \
            message_count = message_count + 1, \
            last_question = excluded.last_question, \
            deleted_at = NULL, \
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(conversation_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(question)
    .execute(db)
    .await
    {
        tracing::warn!(
            tenant_id,
            user_id,
            conversation_id,
            error = %e,
            "failed to upsert attribution conversation"
        );
    }
}

async fn update_attribution_conversation_summary(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    conversation_id: &str,
    summary: Option<&str>,
) {
    if let Some(summary) = summary.filter(|s| !s.trim().is_empty()) {
        if let Err(e) = sqlx::query::<sqlx::Sqlite>(
            "UPDATE nl2sql_attribution_conversations \
             SET summary = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE tenant_id = ? AND id = ?",
        )
        .bind(summary)
        .bind(tenant_id)
        .bind(conversation_id)
        .execute(db)
        .await
        {
            tracing::warn!(
                tenant_id,
                conversation_id,
                error = %e,
                "failed to update attribution conversation summary"
            );
        }
    }
}

async fn latest_attribution_task_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    conversation_id: &str,
) -> Option<String> {
    sqlx::query_scalar::<sqlx::Sqlite, _>(
        "SELECT task_id FROM nl2sql_attribution_tasks \
         WHERE tenant_id = ? AND user_id = ? AND conversation_id = ? \
           AND response_json IS NOT NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(conversation_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

async fn persist_attribution_task_started(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
    conversation_id: &str,
    parent_task_id: Option<&str>,
    req: &AttributionAnalyzeRequest,
    depth: AttributionDepth,
) -> Result<()> {
    let datasource_ids_json =
        serde_json::to_string(&req.datasource_ids).unwrap_or_else(|_| "[]".to_string());
    let result = sqlx::query::<sqlx::Sqlite>(
        "INSERT INTO nl2sql_attribution_tasks \
            (task_id, tenant_id, user_id, conversation_id, parent_task_id, question, depth, datasource_ids_json, status, cancel_requested) \
         VALUES (?, ?, ?, ?, ?, ?, ?, json(?), 'queued', 0) \
         ON CONFLICT DO UPDATE SET \
            question = excluded.question, \
            depth = excluded.depth, \
            datasource_ids_json = excluded.datasource_ids_json, \
            status = 'queued', \
            cancel_requested = 0, \
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(conversation_id)
    .bind(parent_task_id)
    .bind(&req.question)
    .bind(depth.label())
    .bind(datasource_ids_json)
    .execute(db)
    .await;
    if let Err(error) = &result {
        tracing::warn!(
            task_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            conversation_id,
            error = %error,
            "failed to persist attribution task start"
        );
    }
    result?;
    Ok(())
}

async fn persist_attribution_task_completed(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
    response: &AttributionAnalyzeResponse,
) {
    let response_json = match serde_json::to_string(response) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(task_id, error = %e, "failed to serialize attribution response");
            return;
        }
    };
    let evidence_cards_json =
        serde_json::to_string(&response.evidence_cards).unwrap_or_else(|_| "[]".to_string());
    let summary = attribution_response_summary(response);
    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks \
         SET status = ?, summary = ?, response_json = json(?), \
             evidence_cards_json = json(?), error = ?, total_execution_ms = ?, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
           AND status IN ('queued', 'running') AND cancel_requested = 0",
    )
    .bind(&response.status)
    .bind(&summary)
    .bind(response_json)
    .bind(evidence_cards_json)
    .bind(&response.error)
    .bind(crate::sqlite_i64(response.total_execution_ms))
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(task_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            task_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            error = %e,
            "failed to persist attribution task completion"
        );
    }
    if let Some(conversation_id) = response.conversation_id.as_deref() {
        update_attribution_conversation_summary(
            db,
            &claims.tenant_id,
            conversation_id,
            summary.as_deref(),
        )
        .await;
    }
}

async fn persist_attribution_progress_event(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    event: &AttributionTaskEvent,
) {
    let event_json = match serde_json::to_string(event) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                task_id = %event.task_id,
                error = %error,
                "failed to serialize data-attribution progress event"
            );
            return;
        }
    };
    if let Err(error) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks
         SET progress_events_json = json_insert(
               CASE WHEN json_valid(progress_events_json) THEN progress_events_json ELSE '[]' END,
               '$[#]', json(?)),
             updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?",
    )
    .bind(event_json)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&event.task_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            task_id = %event.task_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            error = %error,
            "failed to persist data-attribution progress event"
        );
    }
}

async fn load_persisted_attribution_progress_events(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
) -> Vec<AttributionTaskEvent> {
    let raw = sqlx::query_scalar::<sqlx::Sqlite, Option<String>>(
        "SELECT CAST(progress_events_json AS TEXT)
         FROM nl2sql_attribution_tasks
         WHERE task_id = ? AND tenant_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();
    raw.and_then(|value| serde_json::from_str::<Vec<AttributionTaskEvent>>(&value).ok())
        .unwrap_or_default()
}

pub(crate) async fn recover_interrupted_attribution_tasks(db: &sqlx::SqlitePool) -> Result<u64> {
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks
         SET status = 'failed',
             error = COALESCE(error, 'AOS restarted before this attribution task completed; completed progress remains available for review and the task can be retried.'),
             updated_at = CURRENT_TIMESTAMP
         WHERE status IN ('queued', 'running') AND cancel_requested = 0",
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

async fn persist_attribution_task_heartbeat(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
) -> bool {
    match sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks
         SET status = 'running', updated_at = CURRENT_TIMESTAMP
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
           AND status IN ('queued', 'running') AND cancel_requested = 0",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(task_id)
    .execute(db)
    .await
    {
        Ok(result) => result.rows_affected() == 1,
        Err(error) => {
            tracing::warn!(
                task_id,
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                error = %error,
                "failed to heartbeat data-attribution task"
            );
            false
        }
    }
}

async fn persist_attribution_task_failed(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
    error: &str,
) {
    if let Err(e) = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks \
         SET status = 'failed', error = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
           AND status IN ('queued', 'running') AND cancel_requested = 0",
    )
    .bind(error)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(task_id)
    .execute(db)
    .await
    {
        tracing::warn!(
            task_id,
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            error = %e,
            "failed to persist attribution task failure"
        );
    }
}

async fn persist_attribution_task_cancelled(
    db: &sqlx::SqlitePool,
    claims: &Claims,
    task_id: &str,
) -> Result<bool> {
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_tasks \
         SET status = 'cancelled', cancel_requested = 1, error = NULL,
             updated_at = CURRENT_TIMESTAMP \
         WHERE tenant_id = ? AND user_id = ? AND task_id = ?
           AND status IN ('queued', 'running')",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(task_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn load_previous_attribution_context(
    state: &AppState,
    claims: &Claims,
    conversation_id: &str,
) -> Option<PreviousAttributionContext> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id, question, CAST(response_json AS TEXT) AS response_json, \
                CAST(evidence_cards_json AS TEXT) AS evidence_cards_json \
         FROM nl2sql_attribution_tasks \
         WHERE tenant_id = ? AND user_id = ? AND conversation_id = ? \
           AND response_json IS NOT NULL \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(conversation_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;
    let task_id: String = row.try_get("task_id").ok()?;
    let question: String = row.try_get("question").unwrap_or_default();
    let response_raw: String = row
        .try_get::<Option<String>, _>("response_json")
        .ok()
        .flatten()?;
    let response: AttributionAnalyzeResponse = serde_json::from_str(&response_raw).ok()?;
    let evidence_cards = row
        .try_get::<Option<String>, _>("evidence_cards_json")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<AttributionEvidenceCard>>(&raw).ok())
        .filter(|cards| !cards.is_empty())
        .unwrap_or_else(|| build_evidence_cards(&response.observations));
    Some(PreviousAttributionContext {
        task_id,
        question,
        response,
        evidence_cards,
    })
}

fn build_contextual_attribution_question(
    question: &str,
    previous: &PreviousAttributionContext,
) -> String {
    let report = previous.response.report.as_ref();
    let prior_summary = report
        .map(|r| {
            format!(
                "{}\n核心结论：{}\n核心指标：{}",
                r.title,
                r.executive_summary,
                r.metric_answer.clone().unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let cards = previous
        .evidence_cards
        .iter()
        .take(8)
        .map(|card| {
            serde_json::json!({
                "stepId": card.step_id,
                "title": card.title,
                "status": card.status,
                "rowCount": card.row_count,
                "columns": card.columns,
                "highlights": card.numeric_highlights,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "这是同一个数据归因会话里的追问。\n上一轮问题：{}\n上一轮任务：{}\n上一轮摘要：{}\n上一轮证据卡：\n{}\n\n当前追问：{}\n\n请优先复用上一轮数据源、SQL 知识库、指标口径和证据；只有当前追问需要新证据时才补充查询。",
        previous.question,
        previous.task_id,
        prior_summary,
        cards,
        question
    )
}

fn truncate_attribution_context(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else if let Some(index) = trimmed.rfind(SUPER_ASSISTANT_EXACT_RECENT_TAIL_HEADER) {
        let recent_tail = &trimmed[index..];
        if recent_tail.chars().count() <= max_chars {
            format!("...[older attribution context truncated]\n{recent_tail}")
        } else {
            recent_tail
                .chars()
                .rev()
                .take(max_chars)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        }
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

fn build_attribution_analysis_question(question: &str, context: Option<&str>) -> String {
    let question = question.trim();
    let Some(context) = context.map(str::trim).filter(|value| !value.is_empty()) else {
        return question.to_string();
    };
    let context = truncate_attribution_context(context, 12_000);
    format!(
        "共享会话背景（只用于理解代词、业务对象、指标口径和已知约束；不得覆盖用户当前问题）：\n{context}\n\n用户当前问题（最高优先级）：\n{question}"
    )
}

fn normalize_followup_shared_context(shared_context: Option<&str>) -> Option<String> {
    shared_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| truncate_attribution_context(value, 12_000))
}

async fn answer_followup_from_previous_context(
    state: &AppState,
    claims: &Claims,
    question: &str,
    depth: AttributionDepth,
    previous: &PreviousAttributionContext,
    shared_context: Option<&str>,
    preferred_model: Option<&str>,
) -> anyhow::Result<Option<AttributionReport>> {
    #[derive(Serialize)]
    struct FollowupInput<'a> {
        current_question: &'a str,
        depth: &'a str,
        shared_context: Option<&'a str>,
        previous_task_id: &'a str,
        previous_question: &'a str,
        previous_report: &'a Option<AttributionReport>,
        previous_evidence_cards: &'a [AttributionEvidenceCard],
    }

    let system = r#"你是数据归因追问判断器。
你的任务是判断“当前追问”是否能完全基于上一轮报告和证据卡回答。

只输出 JSON：
{
  "answerable": true,
  "rationale": "为什么可以或不可以只用上一轮证据回答",
  "report": {
    "title": "简短标题",
    "executiveSummary": "老板能直接读懂的回答",
    "metricAnswer": "如有直接指标答案则填写",
    "mainCauses": [],
    "recommendations": [],
    "caveats": [],
    "nextQuestions": [],
    "confidence": "高/中/低",
    "coverage": "本次只复用上一轮证据"
  }
}

规则：
- 如果追问只是解释上一轮结论、换一种表达、追问“为什么这么判断/哪个最主要/证据在哪”，且证据卡足够，answerable=true。
- 如果追问要求新维度、新时间范围、新过滤条件、新指标、重新计算、验证新假设，answerable=false，report=null。
- 不能编造上一轮证据卡里没有的数据、字段、时间范围或结论。
- answerable=true 时 mainCauses 的 evidenceStepIds 必须来自 previousEvidenceCards.stepId。
- 如果只部分能回答，也必须 answerable=false，让系统补查。
- shared_context 是当前超级助手会话的最近原文/共享背景，只用于理解当前追问里的代词、业务对象、时间范围、指标口径和已知约束；如果 shared_context 表明当前追问换了对象、时间、指标或口径，answerable=false。
"#;

    let shared_context = normalize_followup_shared_context(shared_context);
    let prompt = serde_json::to_string(&FollowupInput {
        current_question: question,
        depth: depth.label(),
        shared_context: shared_context.as_deref(),
        previous_task_id: &previous.task_id,
        previous_question: &previous.question,
        previous_report: &previous.response.report,
        previous_evidence_cards: &previous.evidence_cards,
    })?;
    let text = call_llm_text(state, claims, system, &prompt, 8192, 0.0, preferred_model).await?;
    let json_text = extract_json_object(&text).unwrap_or_else(|| text.clone());
    let answer: FollowupContextAnswer = serde_json::from_str(&json_text)?;
    if answer.answerable {
        Ok(answer.report)
    } else {
        Ok(None)
    }
}

pub(crate) async fn start_attribution_task_from_super_assistant(
    state: &AppState,
    claims: Claims,
    req: AttributionAnalyzeRequest,
) -> Result<StartAttributionTaskResponse> {
    start_attribution_task_inner(state, claims, req, false).await
}

async fn start_attribution_task_inner(
    state: &AppState,
    claims: Claims,
    mut req: AttributionAnalyzeRequest,
    create_agent_ops_root: bool,
) -> Result<StartAttributionTaskResponse> {
    let question = req.question.trim();
    if question.is_empty() {
        return Err(AppError::ValidationError("请输入要分析的问题".to_string()));
    }

    let task_id = format!("nl2sql-attribution-task-{}", uuid::Uuid::new_v4());
    let agent_task_id = if create_agent_ops_root {
        let outcome = agent_ops::create_task_with_outcome(
            state,
            CreateAgentTaskInput {
                tenant_id: claims.tenant_id.clone(),
                source: "nl2sql_attribution".to_string(),
                source_ref: Some(task_id.clone()),
                source_label: Some("数据归因".to_string()),
                capability_key: "data_attribution".to_string(),
                agent_id: None,
                agent_name: Some("数据归因".to_string()),
                title: question.chars().take(80).collect(),
                summary: Some("多轮 SQL 下钻与归因分析".to_string()),
                owner_user_id: Some(claims.sub.clone()),
                correlation_id: req.conversation_id.clone(),
                parent_task_id: None,
                external_platform: None,
                external_channel_id: None,
                external_conversation_id: None,
                external_message_id: None,
                idempotency_key: Some(format!("nl2sql-attribution:{task_id}")),
                input_json: Some(serde_json::json!({
                    "questionChars": question.chars().count(),
                    "conversationId": &req.conversation_id,
                    "dataSourceCount": req.datasource_ids.len(),
                    "depth": req.depth.unwrap_or_default().label(),
                })),
            },
        )
        .await?;
        if let Err(error) = agent_ops::link_task_resource(
            state,
            &claims.tenant_id,
            &outcome.id,
            "nl2sql_attribution_task",
            &task_id,
        )
        .await
        {
            let _ = agent_ops::fail_task(
                state,
                &claims.tenant_id,
                &outcome.id,
                "attribution_resource_link_failed",
                &error.to_string(),
            )
            .await;
            return Err(error);
        }
        Some(outcome.id)
    } else {
        None
    };
    let manager = task_manager().clone();
    let run_slot = match manager.try_acquire_run_slot() {
        Ok(slot) => slot,
        Err(error) => {
            if let Some(agent_task_id) = agent_task_id.as_deref() {
                let _ = agent_ops::fail_task(
                    state,
                    &claims.tenant_id,
                    agent_task_id,
                    "attribution_queue_saturated",
                    &error.to_string(),
                )
                .await;
            }
            return Err(error);
        }
    };
    if let Err(error) = manager
        .create_task(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        if let Some(agent_task_id) = agent_task_id.as_deref() {
            let _ = agent_ops::fail_task(
                state,
                &claims.tenant_id,
                agent_task_id,
                "attribution_queue_rejected",
                &error.to_string(),
            )
            .await;
        }
        return Err(error);
    }
    let conversation_id = attribution_conversation_id(req.conversation_id.clone());
    req.conversation_id = Some(conversation_id.clone());
    let depth = req.depth.unwrap_or_default();
    let parent_task_id =
        latest_attribution_task_id(&state.db, &claims.tenant_id, &claims.sub, &conversation_id)
            .await;
    upsert_attribution_conversation(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &conversation_id,
        question,
    )
    .await;
    if let Err(error) = persist_attribution_task_started(
        &state.db,
        &claims,
        &task_id,
        &conversation_id,
        parent_task_id.as_deref(),
        &req,
        depth,
    )
    .await
    {
        manager.publish_failed(&task_id, error.to_string()).await;
        if let Some(agent_task_id) = agent_task_id.as_deref() {
            let _ = agent_ops::fail_task(
                state,
                &claims.tenant_id,
                agent_task_id,
                "attribution_persistence_failed",
                &error.to_string(),
            )
            .await;
        }
        return Err(error);
    }
    if let Some(initial_event) = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        persist_attribution_progress_event(&state.db, &claims, &initial_event).await;
    }

    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let task_id_clone = task_id.clone();
    let agent_task_id_clone = agent_task_id.clone();
    tokio::spawn(async move {
        let _run_slot = run_slot;
        let manager2 = task_manager().clone();
        if let Some(agent_task_id) = agent_task_id_clone.as_deref() {
            let _ = agent_ops::mark_task_running(
                &state_clone,
                &claims_clone.tenant_id,
                agent_task_id,
                agent_ops::PHASE_PLANNING,
                "数据归因开始理解问题",
                8,
            )
            .await;
        }
        publish(
            &state_clone,
            &claims_clone,
            &task_id_clone,
            "understand",
            "正在理解归因问题和需要澄清的信息",
            Some(8),
            None,
            None,
            None,
        )
        .await;
        let request_conversation_id = req.conversation_id.clone();
        let archive_question = req.question.clone();
        let archive_had_shared_context = req
            .context
            .as_deref()
            .is_some_and(|context| !context.trim().is_empty());
        let run_result = {
            let execution = std::panic::AssertUnwindSafe(analyze_attribution(
                &state_clone,
                &claims_clone,
                req,
                &task_id_clone,
            ))
            .catch_unwind();
            tokio::pin!(execution);
            if !persist_attribution_task_heartbeat(&state_clone.db, &claims_clone, &task_id_clone)
                .await
            {
                None
            } else {
                let mut heartbeat = tokio::time::interval_at(
                    tokio::time::Instant::now() + ATTRIBUTION_TASK_HEARTBEAT_INTERVAL,
                    ATTRIBUTION_TASK_HEARTBEAT_INTERVAL,
                );
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        result = &mut execution => break Some(result),
                        _ = heartbeat.tick() => {
                            if !persist_attribution_task_heartbeat(
                                &state_clone.db,
                                &claims_clone,
                                &task_id_clone,
                            )
                            .await {
                                break None;
                            }
                        }
                    }
                }
            }
        };

        let Some(run_result) = run_result else {
            tracing::info!(
                task_id = %task_id_clone,
                tenant_id = %claims_clone.tenant_id,
                user_id = %claims_clone.sub,
                "data-attribution worker stopped after durable cancellation or lease loss"
            );
            return;
        };

        match run_result {
            Ok(Ok(resp)) => {
                if manager2.is_cancelled(&task_id_clone).await {
                    return;
                }
                persist_attribution_task_completed(
                    &state_clone.db,
                    &claims_clone,
                    &task_id_clone,
                    &resp,
                )
                .await;
                if let Some(agent_task_id) = agent_task_id_clone.as_deref() {
                    let _ = agent_ops::sync_linked_resource_status(
                        &state_clone,
                        &claims_clone.tenant_id,
                        agent_task_id,
                    )
                    .await;
                }
                let terminal_message = format_attribution_terminal_message(&resp);
                persist_attribution_turn_exact_archive(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    resp.conversation_id.as_deref(),
                    &archive_question,
                    &terminal_message,
                    &resp.observations,
                    archive_had_shared_context,
                    "completed",
                )
                .await;
                persist_attribution_terminal_message_to_super_assistant_session(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    resp.conversation_id.as_deref(),
                    terminal_message,
                )
                .await;
                manager2.publish_completed(&task_id_clone, resp).await;
            }
            Ok(Err(e)) => {
                if manager2.is_cancelled(&task_id_clone).await {
                    return;
                }
                let error = e.to_string();
                persist_attribution_task_failed(
                    &state_clone.db,
                    &claims_clone,
                    &task_id_clone,
                    &error,
                )
                .await;
                if let Some(agent_task_id) = agent_task_id_clone.as_deref() {
                    let _ = agent_ops::sync_linked_resource_status(
                        &state_clone,
                        &claims_clone.tenant_id,
                        agent_task_id,
                    )
                    .await;
                }
                let terminal_message = format!("数据归因执行失败：{error}");
                persist_attribution_turn_exact_archive(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    request_conversation_id.as_deref(),
                    &archive_question,
                    &terminal_message,
                    &[],
                    archive_had_shared_context,
                    "failed",
                )
                .await;
                persist_attribution_terminal_message_to_super_assistant_session(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    request_conversation_id.as_deref(),
                    terminal_message,
                )
                .await;
                manager2.publish_failed(&task_id_clone, error).await;
            }
            Err(panic_payload) => {
                if manager2.is_cancelled(&task_id_clone).await {
                    return;
                }
                let panic_msg = format_attribution_panic_payload(panic_payload.as_ref());
                tracing::error!(
                    task_id = %task_id_clone,
                    tenant_id = %claims_clone.tenant_id,
                    user_id = %claims_clone.sub,
                    panic = %panic_msg,
                    "nl2sql attribution task panicked"
                );
                let error = format!("数据归因任务异常中断：{panic_msg}");
                persist_attribution_task_failed(
                    &state_clone.db,
                    &claims_clone,
                    &task_id_clone,
                    &error,
                )
                .await;
                if let Some(agent_task_id) = agent_task_id_clone.as_deref() {
                    let _ = agent_ops::sync_linked_resource_status(
                        &state_clone,
                        &claims_clone.tenant_id,
                        agent_task_id,
                    )
                    .await;
                }
                persist_attribution_turn_exact_archive(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    request_conversation_id.as_deref(),
                    &archive_question,
                    &error,
                    &[],
                    archive_had_shared_context,
                    "failed",
                )
                .await;
                persist_attribution_terminal_message_to_super_assistant_session(
                    &state_clone,
                    &claims_clone,
                    &task_id_clone,
                    request_conversation_id.as_deref(),
                    error.clone(),
                )
                .await;
                manager2.publish_failed(&task_id_clone, error).await;
            }
        }
    });

    Ok(StartAttributionTaskResponse {
        task_id,
        status: "queued".to_string(),
        conversation_id,
    })
}

pub(crate) async fn start_attribution_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AttributionAnalyzeRequest>,
) -> Result<Json<StartAttributionTaskResponse>> {
    Ok(Json(
        start_attribution_task_inner(&state, claims, req, true).await?,
    ))
}

pub(crate) async fn get_attribution_task_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<AttributionTaskStatusResponse>> {
    let snapshot = match task_manager()
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        Some(snapshot) => Some(snapshot),
        None => persisted_attribution_task_snapshot(&state, &claims, &task_id).await,
    };
    let snapshot =
        snapshot.ok_or_else(|| AppError::NotFound("attribution task not found".to_string()))?;
    Ok(Json(AttributionTaskStatusResponse {
        task_id: snapshot.task_id,
        status: snapshot.status,
        stage: snapshot.stage,
        message: snapshot.message,
        elapsed_ms: snapshot.elapsed_ms,
        stage_elapsed_ms: snapshot.stage_elapsed_ms,
        progress_percent: snapshot.progress_percent,
        step_index: snapshot.step_index,
        step_total: snapshot.step_total,
        observation: snapshot.observation,
        response: snapshot.response,
        error: snapshot.error,
    }))
}

async fn persisted_attribution_task_snapshot(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
) -> Option<AttributionTaskEvent> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT conversation_id, question, depth, status, summary,
                CAST(response_json AS TEXT) AS response_json,
                CAST(progress_events_json AS TEXT) AS progress_events_json,
                error, CAST(total_execution_ms AS INTEGER) AS total_execution_ms
         FROM nl2sql_attribution_tasks
         WHERE task_id = ? AND tenant_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;
    let status = row.try_get::<String, _>("status").ok()?;
    let total_execution_ms = row
        .try_get::<Option<i64>, _>("total_execution_ms")
        .ok()
        .flatten()
        .unwrap_or(0)
        .max(0) as u64;
    let error = row.try_get::<Option<String>, _>("error").ok().flatten();
    let response = row
        .try_get::<Option<String>, _>("response_json")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<AttributionAnalyzeResponse>(&raw).ok())
        .or_else(|| {
            recover_attribution_response_from_progress(
                row.try_get::<String, _>("question").unwrap_or_default(),
                row.try_get::<String, _>("depth")
                    .unwrap_or_else(|_| "standard".to_string()),
                row.try_get::<String, _>("conversation_id")
                    .unwrap_or_default(),
                status.clone(),
                total_execution_ms,
                error.clone(),
                row.try_get::<Option<String>, _>("progress_events_json")
                    .ok()
                    .flatten(),
            )
        });
    Some(AttributionTaskEvent {
        task_id: task_id.to_string(),
        status: status.clone(),
        stage: Some(status.clone()),
        message: row
            .try_get::<Option<String>, _>("summary")
            .ok()
            .flatten()
            .or_else(|| {
                Some(if attribution_status_is_terminal(&status) {
                    "数据归因任务已结束".to_string()
                } else {
                    "数据归因任务仍在执行".to_string()
                })
            }),
        elapsed_ms: total_execution_ms,
        stage_elapsed_ms: None,
        progress_percent: Some(if attribution_status_is_terminal(&status) {
            100
        } else {
            50
        }),
        step_index: None,
        step_total: None,
        observation: None,
        response,
        error,
    })
}

pub(crate) async fn cancel_attribution_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<AttributionTaskStatusResponse>> {
    let exists = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM nl2sql_attribution_tasks
         WHERE task_id = ? AND tenant_id = ? AND user_id = ?",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;
    if exists == 0 {
        return Err(AppError::NotFound("attribution task not found".to_string()));
    }
    let _ = persist_attribution_task_cancelled(&state.db, &claims, &task_id).await?;
    let snapshot = match task_manager()
        .cancel(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        Some(snapshot) => snapshot,
        None => persisted_attribution_task_snapshot(&state, &claims, &task_id)
            .await
            .ok_or_else(|| AppError::NotFound("attribution task not found".to_string()))?,
    };
    Ok(Json(AttributionTaskStatusResponse {
        task_id: snapshot.task_id,
        status: snapshot.status,
        stage: snapshot.stage,
        message: snapshot.message,
        elapsed_ms: snapshot.elapsed_ms,
        stage_elapsed_ms: snapshot.stage_elapsed_ms,
        progress_percent: snapshot.progress_percent,
        step_index: snapshot.step_index,
        step_total: snapshot.step_total,
        observation: snapshot.observation,
        response: snapshot.response,
        error: snapshot.error,
    }))
}

fn attribution_status_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "clarification_needed" | "no_data" | "partial" | "failed" | "cancelled"
    )
}

pub(crate) async fn stream_attribution_task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<
    Sse<
        impl futures_util::stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>,
    >,
> {
    let manager = task_manager().clone();
    let mut live_rx = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await;
    let memory_history = manager
        .history(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .unwrap_or_default();
    let history = if memory_history.is_empty() {
        load_persisted_attribution_progress_events(&state.db, &claims, &task_id).await
    } else {
        memory_history
    };
    if history.is_empty()
        && persisted_attribution_task_snapshot(&state, &claims, &task_id)
            .await
            .is_none()
    {
        return Err(AppError::NotFound("attribution task not found".to_string()));
    }

    let stream = async_stream::stream! {
        let history_terminal = history
            .last()
            .map(|evt| attribution_status_is_terminal(evt.status.as_str()))
            .unwrap_or(false);
        for evt in history {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
        }
        if history_terminal {
            return;
        }

        if let Some(rx) = live_rx.as_mut() {
            while let Ok(evt) = rx.recv().await {
                let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                let terminal = attribution_status_is_terminal(evt.status.as_str());
                yield Ok(Event::default().event("task_event").data(payload));
                if terminal {
                    break;
                }
            }
        } else if let Some(snapshot) = persisted_attribution_task_snapshot(&state, &claims, &task_id).await {
            let payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
        } else {
            let missing = AttributionTaskEvent {
                task_id: task_id.clone(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("数据归因任务记录不存在".to_string()),
                elapsed_ms: 0,
                stage_elapsed_ms: None,
                progress_percent: Some(100),
                step_index: None,
                step_total: None,
                observation: None,
                response: None,
                error: Some("attribution task not found".to_string()),
            };
            let payload = serde_json::to_string(&missing).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    ))
}

pub(crate) async fn list_attribution_conversations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<AttributionConversationListResponse>> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).clamp(1, 100);
    let offset = i64::from((page - 1).saturating_mul(per_page));
    let limit = i64::from(per_page);
    let total = sqlx::query_scalar::<sqlx::Sqlite, i64>(
        "SELECT CAST(COUNT(*) AS INTEGER) FROM nl2sql_attribution_conversations \
         WHERE tenant_id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_one(&state.db)
    .await?;
    let rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, CAST(message_count AS INTEGER) AS message_count, summary, last_question, \
                strftime('%Y-%m-%d %H:%M:%S', created_at) AS created_at, \
                strftime('%Y-%m-%d %H:%M:%S', updated_at) AS updated_at \
         FROM nl2sql_attribution_conversations \
         WHERE tenant_id = ? AND user_id = ? AND deleted_at IS NULL \
         ORDER BY updated_at DESC LIMIT ? OFFSET ?",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let conversations = rows
        .into_iter()
        .map(|row| AttributionConversationItem {
            id: row.try_get("id").unwrap_or_default(),
            message_count: row.try_get("message_count").unwrap_or(0),
            summary: row.try_get::<Option<String>, _>("summary").ok().flatten(),
            last_question: row
                .try_get::<Option<String>, _>("last_question")
                .ok()
                .flatten(),
            created_at: row.try_get("created_at").unwrap_or_default(),
            updated_at: row.try_get("updated_at").unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    Ok(Json(AttributionConversationListResponse {
        total: usize::try_from(total.max(0)).unwrap_or(conversations.len()),
        conversations,
    }))
}

fn recover_attribution_response_from_progress(
    question: String,
    depth: String,
    conversation_id: String,
    status: String,
    total_execution_ms: u64,
    error: Option<String>,
    progress_events_json: Option<String>,
) -> Option<AttributionAnalyzeResponse> {
    let events = progress_events_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Vec<AttributionTaskEvent>>(raw).ok())?;
    let mut observations_by_step = HashMap::new();
    let mut step_order = Vec::new();
    for event in events {
        let Some(observation) = event.observation else {
            continue;
        };
        if !observations_by_step.contains_key(&observation.step_id) {
            step_order.push(observation.step_id.clone());
        }
        observations_by_step.insert(observation.step_id.clone(), observation);
    }
    let observations = step_order
        .into_iter()
        .filter_map(|step_id| observations_by_step.remove(&step_id))
        .collect::<Vec<_>>();
    if observations.is_empty() {
        return None;
    }
    let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
    let evidence_cards = build_evidence_cards(&observations);
    let report = Some(fallback_report(&question, &observations));
    Some(AttributionAnalyzeResponse {
        status,
        question,
        depth,
        conversation_id: (!conversation_id.trim().is_empty()).then_some(conversation_id),
        clarification_question: None,
        report,
        plan: None,
        observations,
        evidence_health,
        evidence_cards,
        total_execution_ms,
        error,
    })
}

fn attribution_task_row_to_item(row: sqlx::sqlite::SqliteRow) -> AttributionConversationTaskItem {
    let task_id: String = row.try_get("task_id").unwrap_or_default();
    let conversation_id: String = row.try_get("conversation_id").unwrap_or_default();
    let question: String = row.try_get("question").unwrap_or_default();
    let depth: String = row
        .try_get("depth")
        .unwrap_or_else(|_| "standard".to_string());
    let status: String = row
        .try_get("status")
        .unwrap_or_else(|_| "queued".to_string());
    let error = row.try_get::<Option<String>, _>("error").ok().flatten();
    let total_execution_ms = row
        .try_get::<Option<u64>, _>("total_execution_ms")
        .ok()
        .flatten()
        .unwrap_or(0);
    let response = row
        .try_get::<Option<String>, _>("response_json")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<AttributionAnalyzeResponse>(&raw).ok())
        .or_else(|| {
            recover_attribution_response_from_progress(
                question.clone(),
                depth.clone(),
                conversation_id.clone(),
                status.clone(),
                total_execution_ms,
                error.clone(),
                row.try_get::<Option<String>, _>("progress_events_json")
                    .ok()
                    .flatten(),
            )
        });
    AttributionConversationTaskItem {
        task_id,
        conversation_id,
        question,
        depth,
        status,
        summary: row.try_get::<Option<String>, _>("summary").ok().flatten(),
        response,
        error,
        total_execution_ms,
        created_at: row.try_get("created_at").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
    }
}

pub(crate) async fn get_attribution_conversation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(conversation_id): Path<String>,
) -> Result<Json<AttributionConversationDetailResponse>> {
    let meta = sqlx::query::<sqlx::Sqlite>(
        "SELECT id, CAST(message_count AS INTEGER) AS message_count, summary, last_question, \
                strftime('%Y-%m-%d %H:%M:%S', created_at) AS created_at, \
                strftime('%Y-%m-%d %H:%M:%S', updated_at) AS updated_at \
         FROM nl2sql_attribution_conversations \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND deleted_at IS NULL \
         LIMIT 1",
    )
    .bind(&conversation_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("attribution conversation not found".to_string()))?;

    let task_id_rows = sqlx::query::<sqlx::Sqlite>(
        "SELECT task_id \
         FROM nl2sql_attribution_tasks \
         WHERE tenant_id = ? AND user_id = ? AND conversation_id = ? \
         ORDER BY created_at ASC, task_id ASC LIMIT 100",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&conversation_id)
    .fetch_all(&state.db)
    .await?;
    let task_ids = task_id_rows
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("task_id").ok())
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();

    let mut task_by_id: HashMap<String, AttributionConversationTaskItem> = HashMap::new();
    if !task_ids.is_empty() {
        let mut qb: QueryBuilder<sqlx::Sqlite> = QueryBuilder::new(
            "SELECT task_id, conversation_id, question, depth, status, summary, \
                CAST(response_json AS TEXT) AS response_json, \
                CAST(progress_events_json AS TEXT) AS progress_events_json, error, \
                CAST(total_execution_ms AS INTEGER) AS total_execution_ms, \
                strftime('%Y-%m-%d %H:%M:%S', created_at) AS created_at, \
                strftime('%Y-%m-%d %H:%M:%S', updated_at) AS updated_at \
         FROM nl2sql_attribution_tasks \
         WHERE tenant_id = ",
        );
        qb.push_bind(&claims.tenant_id)
            .push(" AND user_id = ")
            .push_bind(&claims.sub)
            .push(" AND conversation_id = ")
            .push_bind(&conversation_id)
            .push(" AND task_id IN (");
        {
            let mut separated = qb.separated(", ");
            for task_id in &task_ids {
                separated.push_bind(task_id);
            }
            separated.push_unseparated(")");
        }
        let task_rows = qb.build().fetch_all(&state.db).await?;
        for row in task_rows {
            let item = attribution_task_row_to_item(row);
            if !item.task_id.is_empty() {
                task_by_id.insert(item.task_id.clone(), item);
            }
        }
    }
    let tasks = task_ids
        .into_iter()
        .filter_map(|task_id| task_by_id.remove(&task_id))
        .collect::<Vec<_>>();

    Ok(Json(AttributionConversationDetailResponse {
        id: meta.try_get("id").unwrap_or_default(),
        message_count: meta.try_get("message_count").unwrap_or(0),
        summary: meta.try_get::<Option<String>, _>("summary").ok().flatten(),
        last_question: meta
            .try_get::<Option<String>, _>("last_question")
            .ok()
            .flatten(),
        tasks,
        created_at: meta.try_get("created_at").unwrap_or_default(),
        updated_at: meta.try_get("updated_at").unwrap_or_default(),
    }))
}

pub(crate) async fn delete_attribution_conversation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(conversation_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let result = sqlx::query::<sqlx::Sqlite>(
        "UPDATE nl2sql_attribution_conversations \
         SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(&conversation_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "attribution conversation not found".to_string(),
        ));
    }
    Ok(Json(
        serde_json::json!({ "deleted": true, "id": conversation_id }),
    ))
}

fn diagnostic_loop_rounds(depth: AttributionDepth) -> usize {
    match depth {
        AttributionDepth::Fast => 1,
        AttributionDepth::Standard => 1,
        AttributionDepth::Deep => 3,
    }
}

fn diagnostic_loop_step_budget(depth: AttributionDepth) -> usize {
    match depth {
        AttributionDepth::Fast => 1,
        AttributionDepth::Standard => 2,
        AttributionDepth::Deep => 6,
    }
}

fn diagnostic_steps_per_round(depth: AttributionDepth) -> usize {
    match depth {
        AttributionDepth::Fast => 1,
        AttributionDepth::Standard => 2,
        AttributionDepth::Deep => 3,
    }
}

fn should_run_diagnostic_loop(observations: &[AttributionObservation]) -> bool {
    let successful = observations
        .iter()
        .filter(|observation| observation.has_usable_evidence())
        .collect::<Vec<_>>();
    if successful.is_empty() {
        return false;
    }
    let has_step = |step_id: &str| {
        successful
            .iter()
            .any(|observation| observation.step_id == step_id)
    };
    let core_evidence_complete = has_step("main_metric")
        && has_step("metric_decomposition")
        && has_step("dimension_drilldown")
        && successful.iter().any(|observation| {
            observation.step_id.contains("diagnostic")
                || observation.step_id.contains("robustness")
                || observation.step_id.contains("quality")
        });

    // Initial metric/decomposition/dimension rows establish candidates, not a
    // proven root cause. At least one counter-check or robustness observation
    // is required before the deterministic convergence gate can close.
    !core_evidence_complete
}

fn should_skip_attribution_synthesis(observations: &[AttributionObservation]) -> bool {
    observations.is_empty()
        || observations
            .iter()
            .all(|observation| !observation.has_usable_evidence())
}

fn clarification_after_completed_steps(
    candidates: &[(String, String)],
    observations: &[AttributionObservation],
) -> Option<String> {
    let has_success = observations
        .iter()
        .any(AttributionObservation::execution_succeeded);
    candidates
        .iter()
        .find(|(step_id, _)| {
            matches!(step_id.as_str(), "main_metric" | "metric_decomposition") || !has_success
        })
        .map(|(_, question)| question.clone())
}

fn observation_digests(observations: &[AttributionObservation]) -> Vec<ObservationDigest> {
    let row_limit = max_diagnostic_digest_rows();
    let col_limit = max_evidence_card_columns();
    observations
        .iter()
        .map(|obs| {
            let mut reference_files = obs
                .used_references
                .iter()
                .map(|r| r.filename.clone())
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>();
            reference_files.sort();
            reference_files.dedup();
            ObservationDigest {
                step_id: obs.step_id.clone(),
                title: obs.title.clone(),
                purpose: obs.purpose.clone(),
                question: obs.question.clone(),
                columns: obs.columns.iter().take(col_limit).cloned().collect(),
                row_count: obs.row_count,
                sampled: obs.sampled,
                rows: obs.rows.iter().take(row_limit).cloned().collect(),
                error: obs.error.clone(),
                sql_count: obs.sqls.len(),
                reference_files,
            }
        })
        .collect()
}

fn stringify_evidence_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(v) => v.to_string(),
        serde_json::Value::Number(v) => v.to_string(),
        serde_json::Value::String(v) => v.clone(),
        _ => value.to_string(),
    }
}

fn evidence_numeric_highlights(obs: &AttributionObservation) -> Vec<String> {
    let mut highlights = Vec::new();
    for row in obs.rows.iter().take(6) {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let mut parts = Vec::new();
        for (key, value) in obj.iter().take(10) {
            if value.is_number() || value.is_boolean() || value.is_string() {
                parts.push(format!("{key}={}", stringify_evidence_value(value)));
            }
        }
        if !parts.is_empty() {
            highlights.push(parts.join(", "));
        }
        if highlights.len() >= 6 {
            break;
        }
    }
    highlights
}

fn attribution_time_context(question: &str) -> Option<String> {
    static TIME_CONTEXT_RE: OnceLock<Regex> = OnceLock::new();
    let regex = TIME_CONTEXT_RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:\d{4}[-/]\d{1,2}[-/]\d{1,2}|今天|昨日|昨天|前天|本周|上周|本月|上月|近\s*\d+\s*(?:天|日|周|月)|today|yesterday|last\s+\d+\s+(?:days?|weeks?|months?))",
        )
        .expect("attribution time context regex")
    });
    let values = regex
        .find_iter(question)
        .map(|item| item.as_str().trim().to_string())
        .filter(|item| !item.is_empty())
        .take(8)
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("; "))
}

fn attribution_evidence_refs(obs: &AttributionObservation) -> Vec<AttributionEvidenceRef> {
    let mut refs = Vec::new();
    for (row_index, row) in obs.rows.iter().take(max_evidence_card_rows()).enumerate() {
        let Some(object) = row.as_object() else {
            continue;
        };
        for (column, value) in object.iter().take(max_evidence_card_columns()) {
            refs.push(AttributionEvidenceRef {
                row_index,
                column: column.clone(),
                value_preview: stringify_evidence_value(value).chars().take(240).collect(),
            });
        }
    }
    refs
}

fn build_evidence_cards(observations: &[AttributionObservation]) -> Vec<AttributionEvidenceCard> {
    let row_limit = max_evidence_card_rows();
    let col_limit = max_evidence_card_columns();
    observations
        .iter()
        .map(|obs| {
            let mut reference_files = obs
                .used_references
                .iter()
                .map(|r| r.filename.clone())
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>();
            reference_files.sort();
            reference_files.dedup();
            AttributionEvidenceCard {
                step_id: obs.step_id.clone(),
                title: obs.title.clone(),
                purpose: obs.purpose.clone(),
                question: obs.question.clone(),
                datasource_ids: obs.datasource_ids.clone(),
                time_context: obs.time_context.clone(),
                status: if obs.error.is_some() {
                    "failed".to_string()
                } else if !obs.has_usable_evidence() {
                    "no_data".to_string()
                } else {
                    "success".to_string()
                },
                row_count: obs.row_count,
                sampled: obs.sampled,
                columns: obs.columns.iter().take(col_limit).cloned().collect(),
                rows_preview: obs.rows.iter().take(row_limit).cloned().collect(),
                numeric_highlights: evidence_numeric_highlights(obs),
                sql_count: obs.sqls.len(),
                reference_files,
                error: obs.error.clone(),
                evidence_refs: attribution_evidence_refs(obs),
            }
        })
        .collect()
}

fn normalize_step_question_key(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn sanitize_followup_steps(
    steps: Vec<AttributionPlanStep>,
    round: usize,
    remaining_budget: usize,
    seen_ids: &mut std::collections::HashSet<String>,
    seen_questions: &mut std::collections::HashSet<String>,
) -> Vec<AttributionPlanStep> {
    let mut out = Vec::new();
    for (idx, mut step) in steps.into_iter().enumerate() {
        if out.len() >= remaining_budget {
            break;
        }
        if step.title.trim().is_empty()
            || step.purpose.trim().is_empty()
            || step.question.trim().is_empty()
        {
            continue;
        }
        if step.id.trim().is_empty() {
            step.id = format!("diagnostic_r{round}_{}", idx + 1);
        }
        step.id = step
            .id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if step.id.trim().is_empty() || seen_ids.contains(&step.id) {
            step.id = format!("diagnostic_r{round}_{}", idx + 1);
        }
        let mut suffix = 2usize;
        let base_id = step.id.clone();
        while seen_ids.contains(&step.id) {
            step.id = format!("{base_id}_{suffix}");
            suffix += 1;
        }
        let question_key = normalize_step_question_key(&step.question);
        if question_key.is_empty() || seen_questions.contains(&question_key) {
            continue;
        }
        step.priority = 80u8
            .saturating_add(u8::try_from(round).unwrap_or(0).saturating_mul(5))
            .saturating_add(u8::try_from(out.len()).unwrap_or(0));
        seen_ids.insert(step.id.clone());
        seen_questions.insert(question_key);
        out.push(step);
    }
    out
}

async fn run_diagnostic_loop(
    state: &AppState,
    claims: &Claims,
    req: &AttributionAnalyzeRequest,
    conversation_id: &str,
    depth: AttributionDepth,
    task_id: &str,
    plan: &mut AttributionPlan,
    observations: &mut Vec<AttributionObservation>,
    deadline: Instant,
) {
    if !should_run_diagnostic_loop(observations) {
        return;
    }

    let mut remaining_budget = diagnostic_loop_step_budget(depth);
    if remaining_budget == 0 {
        return;
    }
    let mut seen_ids = plan
        .steps
        .iter()
        .map(|s| s.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut seen_questions = plan
        .steps
        .iter()
        .map(|s| normalize_step_question_key(&s.question))
        .collect::<std::collections::HashSet<_>>();

    for round in 1..=diagnostic_loop_rounds(depth) {
        if task_manager().is_cancelled(task_id).await || Instant::now() >= deadline {
            return;
        }
        if remaining_budget == 0 {
            break;
        }
        publish(
            state,
            claims,
            task_id,
            "diagnose",
            &format!("正在判断第 {round} 轮是否还需要继续下钻"),
            Some(80 + u8::try_from(round).unwrap_or(0).min(8)),
            None,
            None,
            None,
        )
        .await;

        let max_steps = remaining_budget.min(diagnostic_steps_per_round(depth));
        let followup = match tokio::time::timeout(
            remaining_attribution_budget(deadline),
            build_diagnostic_followup_plan(
                state,
                claims,
                &req.question,
                depth,
                round,
                max_steps,
                observations,
                req.preferred_model.as_deref(),
            ),
        )
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    round,
                    "diagnostic follow-up planner failed; using conservative generic drilldown fallback"
                );
                fallback_diagnostic_followup_plan(&req.question, round, max_steps)
            }
            Err(_) => break,
        };
        if followup.done {
            tracing::info!(
                round,
                rationale = followup.rationale.as_deref().unwrap_or(""),
                "diagnostic follow-up planner decided evidence is sufficient"
            );
            break;
        }

        let mut steps = sanitize_followup_steps(
            followup.steps,
            round,
            remaining_budget,
            &mut seen_ids,
            &mut seen_questions,
        );
        if steps.is_empty() {
            break;
        }

        let total = steps.len();
        for (idx, step) in steps.drain(..).enumerate() {
            if task_manager().is_cancelled(task_id).await || Instant::now() >= deadline {
                return;
            }
            if remaining_budget == 0 {
                break;
            }
            let step_index = idx + 1;
            publish(
                state,
                claims,
                task_id,
                "diagnose",
                &format!(
                    "正在执行第 {round} 轮下钻 {}/{}：{}",
                    step_index, total, step.title
                ),
                Some(82 + u8::try_from(round * 2 + idx).unwrap_or(0).min(8)),
                Some(step_index),
                Some(total),
                None,
            )
            .await;
            let observation = execute_attribution_step(
                state,
                claims,
                req,
                conversation_id,
                &step,
                depth,
                task_id,
                Some(step_index),
                Some(total),
                bounded_phase_deadline(deadline, attribution_step_budget_for(depth, &step)),
            )
            .await;
            let summary = if observation.error.is_some() {
                format!("第 {round} 轮下钻未成功，继续保留其它证据：{}", step.title)
            } else {
                format!(
                    "已完成第 {round} 轮下钻：{}，返回 {} 行",
                    step.title, observation.row_count
                )
            };
            publish(
                state,
                claims,
                task_id,
                "diagnose",
                &summary,
                Some(84 + u8::try_from(round * 2 + idx).unwrap_or(0).min(7)),
                Some(step_index),
                Some(total),
                Some(observation.clone()),
            )
            .await;
            plan.steps.push(step);
            observations.push(observation);
            remaining_budget = remaining_budget.saturating_sub(1);
        }
    }
}

fn cancelled_attribution_response(
    question: &str,
    depth: AttributionDepth,
    conversation_id: &str,
    start: &Instant,
    plan: Option<AttributionPlan>,
    observations: Vec<AttributionObservation>,
) -> AttributionAnalyzeResponse {
    let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
    let evidence_cards = build_evidence_cards(&observations);
    AttributionAnalyzeResponse {
        status: "cancelled".to_string(),
        question: question.to_string(),
        depth: depth.label().to_string(),
        conversation_id: Some(conversation_id.to_string()),
        clarification_question: None,
        report: None,
        plan,
        observations,
        evidence_health,
        evidence_cards,
        total_execution_ms: start.elapsed().as_millis() as u64,
        error: None,
    }
}

async fn analyze_attribution(
    state: &AppState,
    claims: &Claims,
    req: AttributionAnalyzeRequest,
    task_id: &str,
) -> Result<AttributionAnalyzeResponse> {
    super::require_nl2sql_embedding_config(state, &claims.tenant_id).await?;
    let start = Instant::now();
    let depth = req.depth.unwrap_or_default();
    let deadline = start + attribution_total_budget(depth);
    let conversation_id = attribution_conversation_id(req.conversation_id.clone());
    let previous_context = load_previous_attribution_context(state, claims, &conversation_id).await;
    if task_manager().is_cancelled(task_id).await {
        return Ok(cancelled_attribution_response(
            &req.question,
            depth,
            &conversation_id,
            &start,
            None,
            Vec::new(),
        ));
    }

    if is_obviously_ambiguous(&req.question)
        && previous_context.is_none()
        && req
            .context
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Ok(AttributionAnalyzeResponse {
            status: "clarification_needed".to_string(),
            question: req.question,
            depth: depth.label().to_string(),
            conversation_id: Some(conversation_id),
            clarification_question: Some(
                "我还不知道你要分析哪个业务对象、哪个指标和哪个时间范围。请补充类似：昨天某产品的 ROI 为什么比前天下降？"
                    .to_string(),
            ),
            report: None,
            plan: None,
            observations: Vec::new(),
            evidence_health: AttributionEvidenceHealth::empty(),
            evidence_cards: Vec::new(),
            total_execution_ms: start.elapsed().as_millis() as u64,
            error: None,
        });
    }

    if let Some(previous) = previous_context.as_ref() {
        publish(
            state,
            claims,
            task_id,
            "synthesize",
            "正在判断是否可以直接复用上一轮证据回答追问",
            Some(18),
            None,
            None,
            None,
        )
        .await;
        match answer_followup_from_previous_context(
            state,
            claims,
            &req.question,
            depth,
            previous,
            req.context.as_deref(),
            req.preferred_model.as_deref(),
        )
        .await
        {
            Ok(Some(report)) => {
                if task_manager().is_cancelled(task_id).await {
                    return Ok(cancelled_attribution_response(
                        &req.question,
                        depth,
                        &conversation_id,
                        &start,
                        previous.response.plan.clone(),
                        previous.response.observations.clone(),
                    ));
                }
                let observations = previous.response.observations.clone();
                let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
                publish(
                    state,
                    claims,
                    task_id,
                    "synthesize",
                    "已复用上一轮证据生成追问回答",
                    Some(96),
                    None,
                    None,
                    None,
                )
                .await;
                return Ok(AttributionAnalyzeResponse {
                    status: "completed".to_string(),
                    question: req.question,
                    depth: depth.label().to_string(),
                    conversation_id: Some(conversation_id),
                    clarification_question: None,
                    report: Some(sanitize_attribution_report(report, &observations)),
                    plan: previous.response.plan.clone(),
                    evidence_cards: previous.evidence_cards.clone(),
                    observations,
                    evidence_health,
                    total_execution_ms: start.elapsed().as_millis() as u64,
                    error: None,
                });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    task_id,
                    error = %e,
                    "failed to answer attribution follow-up from previous evidence; continuing with incremental analysis"
                );
            }
        }
    }

    let analysis_question = previous_context
        .as_ref()
        .map(|previous| build_contextual_attribution_question(&req.question, previous))
        .unwrap_or_else(|| req.question.clone());
    let analysis_question =
        build_attribution_analysis_question(&analysis_question, req.context.as_deref());
    let execution_req = AttributionAnalyzeRequest {
        question: analysis_question.clone(),
        preferred_model: req.preferred_model.clone(),
        conversation_id: req.conversation_id.clone(),
        context: None,
        datasource_ids: req.datasource_ids.clone(),
        depth: req.depth,
        network_budget: Some(DatasourceRequestBudget::new(3)),
    };

    publish(
        state,
        claims,
        task_id,
        "plan",
        "正在生成归因分析路径",
        Some(18),
        None,
        None,
        None,
    )
    .await;
    let plan = match tokio::time::timeout(
        remaining_attribution_budget(deadline).min(attribution_planning_budget()),
        build_attribution_plan(
            state,
            claims,
            &analysis_question,
            depth,
            req.preferred_model.as_deref(),
        ),
    )
    .await
    {
        Ok(Ok(plan)) => plan,
        Ok(Err(error)) => {
            tracing::warn!(%error, "attribution plan LLM failed, using default analysis path");
            default_attribution_plan(&analysis_question, depth)
        }
        Err(_) => default_attribution_plan(&analysis_question, depth),
    };
    if task_manager().is_cancelled(task_id).await {
        return Ok(cancelled_attribution_response(
            &req.question,
            depth,
            &conversation_id,
            &start,
            None,
            Vec::new(),
        ));
    }
    let mut plan = normalize_attribution_plan(
        soften_plan_clarification(plan, &analysis_question, depth),
        &analysis_question,
        depth,
    );

    if plan.needs_clarification {
        return Ok(AttributionAnalyzeResponse {
            status: "clarification_needed".to_string(),
            question: req.question,
            depth: depth.label().to_string(),
            conversation_id: Some(conversation_id),
            clarification_question: plan.clarification_question.clone().or_else(|| {
                Some(
                    "这个问题缺少关键口径或对比范围，请补充要分析的指标、对象和时间范围。"
                        .to_string(),
                )
            }),
            report: None,
            plan: Some(plan),
            observations: Vec::new(),
            evidence_health: AttributionEvidenceHealth::empty(),
            evidence_cards: Vec::new(),
            total_execution_ms: start.elapsed().as_millis() as u64,
            error: None,
        });
    }

    let mut steps = plan.steps.clone();
    if steps.is_empty() {
        steps = default_attribution_plan(&analysis_question, depth).steps;
    }
    steps.sort_by_key(|s| s.priority);
    steps.truncate(depth.max_steps());

    let mut observations = Vec::new();
    let total_steps = steps.len();
    let execution_deadline = attribution_execution_deadline(deadline);
    let mut indexed_steps = steps.into_iter().enumerate().collect::<Vec<_>>();

    // Establish the requested metric and comparison first. Running every
    // expensive branch at once used to waste the full task budget when the
    // metric itself needed one clarification, and all branches then surfaced
    // the same deadline error. Auxiliary evidence remains concurrent after the
    // core query has proved executable.
    if let Some(core_index) = indexed_steps
        .iter()
        .position(|(_, step)| step.id == "main_metric")
    {
        let (idx, step) = indexed_steps.remove(core_index);
        let step_index = idx + 1;
        publish(
            state,
            claims,
            task_id,
            "execute",
            &format!(
                "正在执行归因查询 {}/{}：{}",
                step_index, total_steps, step.title
            ),
            Some(execute_progress_percent(0, total_steps)),
            Some(step_index),
            Some(total_steps),
            None,
        )
        .await;
        let step_deadline = bounded_phase_deadline(
            execution_deadline,
            attribution_step_budget_for(depth, &step),
        );
        let observation = execute_attribution_step(
            state,
            claims,
            &execution_req,
            &conversation_id,
            &step,
            depth,
            task_id,
            Some(step_index),
            Some(total_steps),
            step_deadline,
        )
        .await;
        let step_summary = if observation.error.is_some() {
            format!(
                "归因查询 {}/{} 未成功，正在判断是否需要补充口径：{}",
                step_index, total_steps, step.title
            )
        } else {
            format!(
                "已完成归因查询 {}/{}：{}，返回 {} 行",
                step_index, total_steps, step.title, observation.row_count
            )
        };
        publish(
            state,
            claims,
            task_id,
            "execute",
            &step_summary,
            Some(execute_progress_percent(1, total_steps)),
            Some(step_index),
            Some(total_steps),
            Some(observation.clone()),
        )
        .await;
        let core_clarification = observation
            .error
            .as_deref()
            .and_then(extract_clarification_from_error);
        let core_failed = observation.error.is_some();
        observations.push(observation);
        if let Some(question) = core_clarification {
            let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
            return Ok(AttributionAnalyzeResponse {
                status: "clarification_needed".to_string(),
                question: req.question,
                depth: depth.label().to_string(),
                conversation_id: Some(conversation_id),
                clarification_question: Some(question),
                report: None,
                plan: Some(plan),
                observations,
                evidence_health,
                evidence_cards: Vec::new(),
                total_execution_ms: start.elapsed().as_millis() as u64,
                error: None,
            });
        }
        // A failed or timed-out core query is terminal for this request. Do
        // not start auxiliary branches while the remote statement may still
        // be unwinding; this is the key protection against fan-out storms.
        if core_failed {
            indexed_steps.clear();
        }
    }

    let mut completed = Vec::new();
    for (idx, step) in indexed_steps.into_iter() {
        let execution_req = &execution_req;
        let conversation_id = &conversation_id;
        let step_index = idx + 1;
        // `buffer_unordered` starts queued futures as earlier steps finish.
        // Re-check cancellation here so a stopped attribution turn cannot
        // launch another model/SQL request from the pending queue.
        if task_manager().is_cancelled(task_id).await || Instant::now() >= execution_deadline {
            completed.push((
                idx,
                step.clone(),
                deadline_attribution_observation(&step, conversation_id),
            ));
            continue;
        }
        publish(
            state,
            claims,
            task_id,
            "execute",
            &format!(
                "正在执行归因查询 {}/{}：{}",
                step_index, total_steps, step.title
            ),
            Some(execute_progress_percent(0, total_steps)),
            Some(step_index),
            Some(total_steps),
            None,
        )
        .await;
        let step_deadline = bounded_phase_deadline(
            execution_deadline,
            attribution_step_budget_for(depth, &step),
        );
        let observation = execute_attribution_step(
            state,
            claims,
            execution_req,
            conversation_id,
            &step,
            depth,
            task_id,
            Some(step_index),
            Some(total_steps),
            step_deadline,
        )
        .await;
        let stop_after_step = observation.error.is_some();
        let completed_count = observations.len() + completed.len() + 1;
        let step_summary = if observation.error.is_some() {
            format!(
                "归因查询 {}/{} 未成功，继续查看其它方向：{}",
                step_index, total_steps, step.title
            )
        } else {
            format!(
                "已完成归因查询 {}/{}：{}，返回 {} 行",
                step_index, total_steps, step.title, observation.row_count
            )
        };
        publish(
            state,
            claims,
            task_id,
            "execute",
            &step_summary,
            Some(execute_progress_percent(completed_count, total_steps)),
            Some(step_index),
            Some(total_steps),
            Some(observation.clone()),
        )
        .await;
        completed.push((idx, step, observation));
        if stop_after_step {
            break;
        }
    }
    completed.sort_by_key(|(idx, _, _)| *idx);

    if task_manager().is_cancelled(task_id).await {
        observations.extend(
            completed
                .iter()
                .map(|(_, _, observation)| observation.clone()),
        );
        return Ok(cancelled_attribution_response(
            &req.question,
            depth,
            &conversation_id,
            &start,
            Some(plan),
            observations,
        ));
    }

    let mut clarification_candidates = Vec::new();
    for (_, step, observation) in completed {
        let clarification = observation
            .error
            .as_deref()
            .and_then(extract_clarification_from_error);
        if let Some(question) = clarification {
            clarification_candidates.push((step.id.clone(), question));
        }
        observations.push(observation);
    }
    if let Some(question) =
        clarification_after_completed_steps(&clarification_candidates, &observations)
    {
        let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
        return Ok(AttributionAnalyzeResponse {
            status: "clarification_needed".to_string(),
            question: req.question,
            depth: depth.label().to_string(),
            conversation_id: Some(conversation_id),
            clarification_question: Some(question),
            report: None,
            plan: Some(plan),
            evidence_cards: build_evidence_cards(&observations),
            observations,
            evidence_health,
            total_execution_ms: start.elapsed().as_millis() as u64,
            error: None,
        });
    }

    run_diagnostic_loop(
        state,
        claims,
        &execution_req,
        &conversation_id,
        depth,
        task_id,
        &mut plan,
        &mut observations,
        execution_deadline,
    )
    .await;
    if task_manager().is_cancelled(task_id).await {
        return Ok(cancelled_attribution_response(
            &req.question,
            depth,
            &conversation_id,
            &start,
            Some(plan),
            observations,
        ));
    }

    let evidence_health = AttributionEvidenceHealth::from_observations(&observations);
    let mut report = if should_skip_attribution_synthesis(&observations) {
        // A failed/blocked datasource query already contains the useful
        // evidence for the user: generated SQL, query id (when accepted),
        // and the connector error. Calling the LLM here cannot create data
        // evidence and used to add three 45-second retries to every outage.
        tracing::info!(
            task_id,
            "skipping attribution report synthesis because no query produced executable evidence"
        );
        fallback_report(&analysis_question, &observations)
    } else {
        publish(
            state,
            claims,
            task_id,
            "synthesize",
            "正在把查询结果整理成老板可读的根因结论",
            Some(92),
            None,
            None,
            None,
        )
        .await;
        match tokio::time::timeout(
            remaining_attribution_budget(deadline),
            synthesize_attribution_report(
                state,
                claims,
                &analysis_question,
                depth,
                &plan,
                &observations,
                req.preferred_model.as_deref(),
            ),
        )
        .await
        {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                tracing::warn!(%error, "attribution report synthesis failed, returning evidence-aware fallback");
                fallback_report(&analysis_question, &observations)
            }
            Err(_) => fallback_report(&analysis_question, &observations),
        }
    };
    let evidence_cards = build_evidence_cards(&observations);
    let success_count = evidence_health.successful_steps;
    let deadline_reached = Instant::now() >= deadline;
    if deadline_reached {
        report
            .caveats
            .push("已达到本次归因的总时间预算，报告基于截止时已经验证的证据生成。".to_string());
    }
    let status = if success_count == 0 {
        "no_data".to_string()
    } else if deadline_reached || success_count < observations.len() {
        "partial".to_string()
    } else {
        "completed".to_string()
    };

    Ok(AttributionAnalyzeResponse {
        status,
        question: req.question,
        depth: depth.label().to_string(),
        conversation_id: Some(conversation_id),
        clarification_question: None,
        report: Some(report),
        plan: Some(plan),
        observations,
        evidence_health,
        evidence_cards,
        total_execution_ms: start.elapsed().as_millis() as u64,
        error: None,
    })
}

fn execute_progress_percent(completed_or_started_steps: usize, total_steps: usize) -> u8 {
    if total_steps == 0 {
        return 80;
    }
    let ratio = completed_or_started_steps as f64 / total_steps as f64;
    (35.0 + ratio.clamp(0.0, 1.0) * 45.0).round() as u8
}

async fn publish(
    state: &AppState,
    claims: &Claims,
    task_id: &str,
    stage: &str,
    message: &str,
    progress_percent: Option<u8>,
    step_index: Option<usize>,
    step_total: Option<usize>,
    observation: Option<AttributionObservation>,
) {
    if let Some(event) = task_manager()
        .publish_stage_progress(
            task_id,
            stage,
            message,
            progress_percent,
            step_index,
            step_total,
            observation,
        )
        .await
    {
        persist_attribution_progress_event(&state.db, claims, &event).await;
    }
}

fn attribution_step_retrieval_question(original_question: &str, step_question: &str) -> String {
    format!("{}\n{}", original_question.trim(), step_question.trim())
}

async fn execute_attribution_step(
    state: &AppState,
    claims: &Claims,
    req: &AttributionAnalyzeRequest,
    conversation_id: &str,
    step: &AttributionPlanStep,
    depth: AttributionDepth,
    task_id: &str,
    step_index: Option<usize>,
    step_total: Option<usize>,
    deadline: Instant,
) -> AttributionObservation {
    let start = Instant::now();
    let agent_question = format!(
        "原始归因问题：{}\n\n当前需要执行的归因查询：{}\n\n请围绕原始问题生成当前查询需要的 SQL，不要丢失原始问题里的业务对象、指标、时间范围和筛选条件。",
        req.question.trim(),
        step.question.trim()
    );
    let agent_req = AgentExecuteRequest {
        question: agent_question,
        retrieval_question: Some(attribution_step_retrieval_question(
            &req.question,
            &step.question,
        )),
        preferred_model: req.preferred_model.clone(),
        shared_context: req.context.clone(),
        datasource_ids: req.datasource_ids.clone(),
        conversation_id: Some(conversation_id.to_string()),
        max_steps: Some(match step.id.as_str() {
            "main_metric" | "metric_decomposition" | "dimension_drilldown" => {
                depth.agent_max_steps().min(2)
            }
            _ => depth.agent_max_steps(),
        }),
        bounded: true,
    };
    // Forward the executor's fine-grained stages. Without this scope the
    // attribution worker only exposed its final observation, which looked like
    // a frozen task during schema loading, SQL generation, and execution.
    let task_id_for_stage = task_id.to_string();
    let state_for_stage = state.clone();
    let claims_for_stage = claims.clone();
    let stage_prefix = format!("execute_{}", attribution_stage_component(&step.id));
    let step_title = step.title.clone();
    let stage_emitter = Arc::new(move |signal: AgentStageSignal| {
        let task_id = task_id_for_stage.clone();
        let state = state_for_stage.clone();
        let claims = claims_for_stage.clone();
        let stage = format!(
            "{stage_prefix}_{}",
            attribution_stage_component(&signal.stage)
        );
        let message = format!("{}：{}", step_title, signal.message);
        tokio::spawn(async move {
            publish(
                &state,
                &claims,
                &task_id,
                &stage,
                &message,
                Some(48),
                step_index,
                step_total,
                None,
            )
            .await;
        });
    });
    let execution = with_agent_stage_emitter(stage_emitter.clone(), async move {
        match req.network_budget.clone() {
            Some(budget) => {
                execute_agent_request_with_budget(state, claims, agent_req, budget).await
            }
            None => execute_agent_request(state, claims, agent_req).await,
        }
    });
    tokio::pin!(execution);
    let primary_deadline = attribution_primary_attempt_deadline(deadline);
    let response = loop {
        tokio::select! {
            result = &mut execution => break Some(result),
            _ = tokio::time::sleep_until(primary_deadline.into()) => break None,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if task_manager().is_cancelled(task_id).await {
                    break None;
                }
            }
        }
    };
    // Never launch a fallback while the original future is still in flight.
    // Cancelling a client-side future does not guarantee that the remote Trino
    // statement has stopped, so a second recovery query could overlap it and
    // multiply load. A timeout is terminal for this step; retain the timeout
    // evidence and let the caller decide whether any already-completed steps
    // are sufficient for a partial report.
    let Some(response) = response else {
        let mut observation = cancelled_attribution_observation(step, conversation_id);
        if !task_manager().is_cancelled(task_id).await {
            observation.error = Some(
                "该分支的完整查询和单 SQL 轻量恢复均未在预算内完成；系统已保留其它已验证证据并继续综合。"
                    .to_string(),
            );
        }
        observation.elapsed_ms = start.elapsed().as_millis() as u64;
        return observation;
    };
    match response {
        Ok(resp) => {
            let query_id = resp.query_id.clone();
            let mut datasource_ids = resp
                .steps
                .iter()
                .filter_map(|step| step.datasource_id.clone())
                .filter(|id| !id.trim().is_empty())
                .collect::<Vec<_>>();
            datasource_ids.sort();
            datasource_ids.dedup();
            let snapshot = if let Some(qid) = query_id.as_deref() {
                load_agent_result_snapshot(state, claims, qid).await
            } else {
                None
            };
            let (columns, rows, sampled) = snapshot.unwrap_or_else(|| {
                let sampled = resp.final_result.row_count > resp.final_result.rows.len();
                (
                    resp.final_result.columns.clone(),
                    resp.final_result.rows.clone(),
                    sampled,
                )
            });
            AttributionObservation {
                step_id: step.id.clone(),
                title: step.title.clone(),
                purpose: step.purpose.clone(),
                question: step.question.clone(),
                datasource_ids,
                time_context: attribution_time_context(&req.question),
                query_id,
                conversation_id: resp.conversation_id,
                columns,
                rows,
                row_count: resp.final_result.row_count,
                sampled,
                sqls: resp
                    .steps
                    .iter()
                    .filter_map(|s| s.sql.clone())
                    .collect::<Vec<_>>(),
                used_references: resp.used_references,
                error: resp.error,
                elapsed_ms: start.elapsed().as_millis() as u64,
            }
        }
        Err(e) => AttributionObservation {
            step_id: step.id.clone(),
            title: step.title.clone(),
            purpose: step.purpose.clone(),
            question: step.question.clone(),
            datasource_ids: req.datasource_ids.clone(),
            time_context: attribution_time_context(&req.question),
            query_id: None,
            conversation_id: Some(conversation_id.to_string()),
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some(e.to_string()),
            elapsed_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn attribution_stage_component(value: &str) -> String {
    let mut normalized = value
        .chars()
        .take(48)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        "step".to_string()
    } else {
        normalized
    }
}

fn cancelled_attribution_observation(
    step: &AttributionPlanStep,
    conversation_id: &str,
) -> AttributionObservation {
    AttributionObservation {
        step_id: step.id.clone(),
        title: step.title.clone(),
        purpose: step.purpose.clone(),
        question: step.question.clone(),
        datasource_ids: Vec::new(),
        time_context: attribution_time_context(&step.question),
        query_id: None,
        conversation_id: Some(conversation_id.to_string()),
        columns: Vec::new(),
        rows: Vec::new(),
        row_count: 0,
        sampled: false,
        sqls: Vec::new(),
        used_references: Vec::new(),
        error: Some("cancelled".to_string()),
        elapsed_ms: 0,
    }
}

fn deadline_attribution_observation(
    step: &AttributionPlanStep,
    conversation_id: &str,
) -> AttributionObservation {
    let mut observation = cancelled_attribution_observation(step, conversation_id);
    observation.error =
        Some("本步骤未在归因查询预算内启动；系统已保留其它已完成证据并进入总结。".to_string());
    observation
}

async fn load_agent_result_snapshot(
    state: &AppState,
    claims: &Claims,
    query_id: &str,
) -> Option<(Vec<String>, Vec<serde_json::Value>, bool)> {
    let row = sqlx::query::<sqlx::Sqlite>(
        "SELECT CAST(columns_json AS TEXT) AS columns_json, \
                CAST(rows_json AS TEXT) AS rows_json, \
                CAST(total_rows AS INTEGER) AS total_rows \
         FROM nl2sql_agent_query_results \
         WHERE tenant_id = ? AND user_id = ? AND query_id = ? \
         LIMIT 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(query_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let columns_raw: String = row
        .try_get::<Option<String>, _>("columns_json")
        .ok()
        .flatten()
        .unwrap_or_else(|| "[]".to_string());
    let rows_raw: String = row
        .try_get::<Option<String>, _>("rows_json")
        .ok()
        .flatten()
        .unwrap_or_else(|| "[]".to_string());
    let total_rows: i64 = row.try_get("total_rows").unwrap_or(0);
    let columns: Vec<String> = serde_json::from_str(&columns_raw).ok()?;
    let all_rows: Vec<serde_json::Value> = serde_json::from_str(&rows_raw).ok()?;
    let cap = max_observation_rows();
    let sampled = all_rows.len() > cap
        || usize::try_from(total_rows.max(0))
            .ok()
            .map(|n| n > cap)
            .unwrap_or(false);
    let rows = all_rows.into_iter().take(cap).collect();
    Some((columns, rows, sampled))
}

async fn build_attribution_plan(
    state: &AppState,
    claims: &Claims,
    question: &str,
    depth: AttributionDepth,
    preferred_model: Option<&str>,
) -> anyhow::Result<AttributionPlan> {
    #[derive(Serialize)]
    struct PlanInput<'a> {
        question: &'a str,
        depth: &'a str,
        max_steps: usize,
    }

    let system = r#"你是企业数据归因分析的任务规划器，服务对象是不懂 SQL 的老板、产品策略和运营负责人。
你的任务不是生成 SQL，而是判断问题是否足够清楚，并把问题拆成可自动查数的自然语言子任务。

原则：
- 不要假设行业。不要写死游戏、广告、电商、金融、医疗等任何行业。
- 如果用户没有说清楚指标、对象、时间范围或对比基线，且无法从问题自然推断，必须先澄清。
- 如果问题要求“为什么变化/下降/上涨/异常/波动/归因/原因”，要规划：主指标对比、指标组成拆解、候选维度下钻、异常/数据质量检查。
- 候选维度必须让后续 NL2SQL 根据真实 schema、指标库和 SQL 知识库自动选择；你只描述分析目标，不要编造字段名。
- 输出 JSON，不要 markdown。

JSON 结构：
{
  "needsClarification": false,
  "clarificationQuestion": null,
  "confidence": 0.0,
  "analysisFocus": ["..."],
  "steps": [
    {
      "id": "main_metric",
      "title": "主指标对比",
      "purpose": "确认目标周期、对比周期和核心变化幅度",
      "question": "给 NL2SQL agent 的自然语言查数任务",
      "priority": 1
    }
  ]
}"#;

    let prompt = serde_json::to_string(&PlanInput {
        question,
        depth: depth.label(),
        max_steps: depth.max_steps(),
    })?;
    let text = call_llm_text(state, claims, system, &prompt, 8192, 0.1, preferred_model).await?;
    let json_text = extract_json_object(&text).unwrap_or_else(|| text.clone());
    let mut plan: AttributionPlan = serde_json::from_str(&json_text).map_err(|error| {
        anyhow::anyhow!(
            "attribution planner returned malformed JSON after bounded repair: {error}; text_chars={}",
            text.chars().count()
        )
    })?;
    if plan.steps.len() > depth.max_steps() {
        plan.steps.truncate(depth.max_steps());
    }
    Ok(plan)
}

async fn build_diagnostic_followup_plan(
    state: &AppState,
    claims: &Claims,
    question: &str,
    depth: AttributionDepth,
    round: usize,
    max_steps: usize,
    observations: &[AttributionObservation],
    preferred_model: Option<&str>,
) -> anyhow::Result<DiagnosticFollowupPlan> {
    #[derive(Serialize)]
    struct FollowupInput<'a> {
        question: &'a str,
        depth: &'a str,
        round: usize,
        max_steps: usize,
        evidence_health: AttributionEvidenceHealth,
        observations: Vec<ObservationDigest>,
    }

    let system = r#"你是老板级数据归因诊断器，负责在已有查数结果基础上决定是否继续下钻。
你的目标是找到“主因/次因/非主因”，而不是机械补查询。

你必须只输出 JSON：
{
  "done": false,
  "rationale": "为什么证据已足够或为什么还要继续查",
  "steps": [
    {
      "id": "diagnostic_xxx",
      "title": "贡献度下钻",
      "purpose": "验证哪个维度贡献了主要变化",
      "question": "给 NL2SQL agent 的自然语言查数任务",
      "priority": 80
    }
  ]
}

判断规则：
- 如果 observations 已经能清楚回答核心指标变化、组成项变化、主要拖动维度和数据质量风险，则 done=true，steps=[]。
- 如果还不能解释“为什么变化”，必须继续生成 1 到 max_steps 个下钻任务。
- 下钻任务必须让 NL2SQL 基于真实 schema、指标库、SQL 知识库自动选字段；不要编造字段名，不要写死任何行业。
- 优先补“贡献度排序”：目标期 vs 对比期，各分组的指标值、变化量、变化率、对整体变化贡献。
- 如果已有某个维度显示贡献最大，下一轮要继续 drill down 到它的子维度或组成项。
- 如果指标可能是比率，必须拆分分子/分母，避免只看比率误判。
- 如果结果可能由样本结构、数据缺失、口径变化或异常值导致，要补质量/口径验证查询。
- 如果某些查询失败，不要重复同样问题；换一个更保守、更可执行的自然语言任务。
- step.id 使用英文小写、数字、下划线，必须唯一。
- step.question 写给内部 NL2SQL agent，必须包含原始问题、要验证的假设、需要返回的列：目标期、对比期、变化量、变化率、贡献度/排序。
"#;

    let prompt = serde_json::to_string(&FollowupInput {
        question,
        depth: depth.label(),
        round,
        max_steps,
        evidence_health: AttributionEvidenceHealth::from_observations(observations),
        observations: observation_digests(observations),
    })?;
    let text = call_llm_text(state, claims, system, &prompt, 8192, 0.1, preferred_model).await?;
    let json_text = extract_json_object(&text).unwrap_or_else(|| text.clone());
    let mut plan: DiagnosticFollowupPlan = serde_json::from_str(&json_text)?;
    if plan.steps.len() > max_steps {
        plan.steps.truncate(max_steps);
    }
    Ok(plan)
}

fn fallback_diagnostic_followup_plan(
    question: &str,
    round: usize,
    max_steps: usize,
) -> DiagnosticFollowupPlan {
    let mut steps = vec![
        AttributionPlanStep {
            id: format!("diagnostic_r{round}_component_check"),
            title: "组成项验证".to_string(),
            purpose: "验证核心指标变化是由组成项、分子分母还是口径差异驱动".to_string(),
            question: format!(
                "请基于真实 schema、SQL 知识库和可用字段，对这个归因问题做指标组成验证：{question}。返回目标期、对比期、组成项数值、变化量、变化率，以及对整体变化的贡献；如果无法拆分，请说明缺少哪些字段或口径。"
            ),
            priority: 80,
        },
        AttributionPlanStep {
            id: format!("diagnostic_r{round}_contribution_rank"),
            title: "贡献度排序".to_string(),
            purpose: "找出对整体变化贡献最大的分组，区分主因和次因".to_string(),
            question: format!(
                "请基于真实 schema、SQL 知识库和可用字段，对这个归因问题做贡献度排序：{question}。自动选择最相关且真实存在的分组维度，返回各分组目标期、对比期、变化量、变化率、贡献度排序；不要编造字段。"
            ),
            priority: 81,
        },
        AttributionPlanStep {
            id: format!("diagnostic_r{round}_quality_check"),
            title: "数据质量检查".to_string(),
            purpose: "排查数据缺失、异常值、样本结构变化或口径变化导致的误判".to_string(),
            question: format!(
                "请基于真实 schema、SQL 知识库和可用字段，对这个归因问题做数据质量和口径检查：{question}。返回目标期与对比期的样本量、缺失/异常变化、关键过滤条件覆盖情况；无法验证的点请明确说明。"
            ),
            priority: 82,
        },
    ];
    steps.truncate(max_steps);
    DiagnosticFollowupPlan {
        done: steps.is_empty(),
        rationale: Some("诊断规划模型失败，使用通用归因下钻兜底，避免只给浅层结论。".to_string()),
        steps,
    }
}

async fn synthesize_attribution_report(
    state: &AppState,
    claims: &Claims,
    question: &str,
    depth: AttributionDepth,
    plan: &AttributionPlan,
    observations: &[AttributionObservation],
    preferred_model: Option<&str>,
) -> anyhow::Result<AttributionReport> {
    #[derive(Serialize)]
    struct ReportInput<'a> {
        question: &'a str,
        depth: &'a str,
        plan: &'a AttributionPlan,
        evidence_cards: Vec<AttributionEvidenceCard>,
        evidence_health: AttributionEvidenceHealth,
    }

    let system = r#"你是面向老板、产品策略和运营负责人的数据归因分析师。
用户看不懂 SQL，所以先给大白话结论；SQL、字段和来源只能作为证据，不要放在开头。

你必须：
- 只基于 evidenceCards 中真实执行出的数据摘要、错误和来源做结论，不能编造不存在的数据。
- 如果某些查询失败，说明影响，但仍然基于已成功的证据给出有用判断。
- 把“确定原因”和“疑似原因”区分清楚。
- 优先从 diagnostic / drilldown / dimension / decomposition 类型的观察里识别主因；如果这些观察给出了贡献度、变化量、变化率或排序，必须据此判断主因和次因。
- 对比类问题必须说明：目标期、对比期、变化方向、变化幅度；原因类问题必须说明：谁贡献最大、为什么、证据来自哪一步。
- 如果指标是比率，必须同时解释分子、分母或组成项变化；不能只凭比率涨跌下结论。
- 如果有多个候选原因，要按“对整体变化贡献大小 + 证据可靠性”排序，不要平均罗列。
- 如果数据不足以回答为什么，明确说还缺什么，并给下一步要补查的方向。
- 如果 evidenceCards 中 sampled=true，说明模型看到的是结果样本/截断快照，不能声称已检查所有明细行；但聚合 SQL 的结果仍可支持聚合结论。
- mainCauses 必须引用 evidenceStepIds；没有证据支撑的判断只能放到 nextQuestions，不能写成原因。
- 用自然语言解释“指标是多少、比对比期变了多少、主要是谁拖动、下一步怎么做”。
- 不要输出 SQL，不要 markdown 表格，不要 JSON 外文字。

返回 JSON：
{
  "title": "简短标题",
  "executiveSummary": "2-4 句老板能直接读懂的核心结论",
  "metricAnswer": "如果证据足够，直接回答核心指标和变化；不足则说明缺口",
  "mainCauses": [
    {
      "title": "原因标题",
      "explanation": "为什么这么判断",
      "impact": "影响方向和大小；没有精确值就写相对判断",
      "evidenceStepIds": ["main_metric"],
      "confidence": "高/中/低"
    }
  ],
  "recommendations": ["具体下一步动作"],
  "caveats": ["口径、样本、失败查询、缺失维度等注意事项"],
  "nextQuestions": ["建议继续追问的问题"],
  "confidence": "高/中/低",
  "coverage": "本次覆盖了哪些数据和哪些没覆盖"
}"#;

    let prompt = serde_json::to_string(&ReportInput {
        question,
        depth: depth.label(),
        plan,
        evidence_cards: build_evidence_cards(observations),
        evidence_health: AttributionEvidenceHealth::from_observations(observations),
    })?;
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=3 {
        match call_llm_text(state, claims, system, &prompt, 16_384, 0.2, preferred_model).await {
            Ok(text) => {
                let json_text = extract_json_object(&text).unwrap_or_else(|| text.clone());
                match serde_json::from_str::<AttributionReport>(&json_text) {
                    Ok(report) => return Ok(sanitize_attribution_report(report, observations)),
                    Err(e) => {
                        last_error = Some(anyhow::anyhow!("report JSON parse failed: {e}"));
                    }
                }
            }
            Err(e) => {
                last_error = Some(e);
            }
        }
        tracing::warn!(
            attempt,
            error = %last_error
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            "attribution report synthesis attempt failed"
        );
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("report synthesis failed")))
}

async fn call_llm_text(
    state: &AppState,
    claims: &Claims,
    system: &str,
    prompt: &str,
    max_tokens: u32,
    temperature: f64,
    preferred_model: Option<&str>,
) -> anyhow::Result<String> {
    let mut chat_candidates = crate::nl2sql::resolve_chat_config_candidates(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve chat config: {e}"))?;
    crate::nl2sql::prioritize_chat_candidates(&mut chat_candidates, preferred_model);

    if chat_candidates.is_empty() {
        return Err(anyhow::anyhow!("no candidate API keys"));
    }
    let total_candidates = chat_candidates.len();
    let mut last_error = None;
    for (candidate_index, chat_cfg) in chat_candidates.into_iter().enumerate() {
        if total_candidates > 1
            && candidate_index + 1 < total_candidates
            && super::nl2sql_candidate_is_suppressed(&claims.tenant_id, &chat_cfg)
        {
            tracing::info!(
                tenant_id = %claims.tenant_id,
                candidate_index = candidate_index + 1,
                total_candidates,
                provider = %chat_cfg.provider,
                model = %chat_cfg.model,
                "data-attribution helper skipping temporarily suppressed candidate"
            );
            continue;
        }
        let effective_max_tokens = max_tokens.min(chat_cfg.max_output_tokens).max(1);
        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: effective_max_tokens,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: prompt.to_string(),
                }],
            }],
            system: Some(system.to_string()),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(temperature),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };
        let response = match tokio::time::timeout(
            attribution_helper_model_budget(),
            chat_cfg.client.send_message(&request),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    candidate_index = candidate_index + 1,
                    total_candidates,
                    provider = %chat_cfg.provider,
                    model = %chat_cfg.model,
                    error = %error,
                    "data-attribution helper model call failed; trying next candidate"
                );
                last_error = Some(anyhow::anyhow!("LLM call failed: {error}"));
                continue;
            }
            Err(_) => {
                let timeout_secs = attribution_helper_model_budget().as_secs();
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    candidate_index = candidate_index + 1,
                    total_candidates,
                    provider = %chat_cfg.provider,
                    model = %chat_cfg.model,
                    timeout_secs,
                    "data-attribution helper model timed out; trying next candidate"
                );
                last_error = Some(anyhow::anyhow!(
                    "LLM helper timed out after {timeout_secs}s"
                ));
                continue;
            }
        };
        let text = super::collect_output_text(&response.content);
        if !text.trim().is_empty() {
            super::clear_nl2sql_candidate_suppression(&claims.tenant_id, &chat_cfg);
            return Ok(text);
        }
        let content_block_types = response
            .content
            .iter()
            .map(|block| match block {
                OutputContentBlock::Text { .. } => "text",
                OutputContentBlock::ToolUse { .. } => "tool_use",
                OutputContentBlock::Thinking { .. } => "thinking",
                OutputContentBlock::RedactedThinking { .. } => "redacted_thinking",
            })
            .collect::<Vec<_>>()
            .join(",");
        let thinking_only_length = super::is_thinking_only_length_response(&response, &text);
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            candidate_index = candidate_index + 1,
            total_candidates,
            provider = %chat_cfg.provider,
            model = %chat_cfg.model,
            stop_reason = ?response.stop_reason,
            content_block_types = %content_block_types,
            content_block_count = response.content.len(),
            input_tokens = response.usage.input_tokens,
            output_tokens = response.usage.output_tokens,
            "data-attribution helper model returned no text; trying next candidate"
        );
        if thinking_only_length && total_candidates > 1 {
            super::suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
        }
        last_error = Some(anyhow::anyhow!("LLM returned empty response"));
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("all candidate API keys were unavailable")))
}

fn default_attribution_plan(question: &str, depth: AttributionDepth) -> AttributionPlan {
    let mut steps = vec![
        AttributionPlanStep {
            id: "main_metric".to_string(),
            title: "主指标对比".to_string(),
            purpose: "确认目标周期、对比周期、核心指标当前值、变化量和变化率".to_string(),
            question: format!(
                "请围绕这个业务问题先做主指标对比查询：{question}。要求返回目标周期和合理对比周期的核心指标、差值、变化率。若问题涉及持续趋势、骤升骤降或异常对象识别，必须返回足以判断趋势的连续时间序列和业务对象维度，并让数据本身决定持续变化与突变候选，不要写死固定阈值。如果时间或对比基线不清楚，请依据知识库中的既有口径选择可解释基线并在结果中注明。"
            ),
            priority: 1,
        },
        AttributionPlanStep {
            id: "metric_decomposition".to_string(),
            title: "指标组成拆解".to_string(),
            purpose: "把核心指标拆成可解释的组成项，判断是分子、分母还是子项变化导致".to_string(),
            question: format!(
                "请根据真实指标口径、SQL 知识库和可用字段，拆解这个问题里的核心指标：{question}。查询目标周期与对比周期下，各组成项的数值、变化量、变化率和贡献方向；对于比率类指标必须同时验证分子、分母及其业务口径，不要使用不存在的字段。"
            ),
            priority: 2,
        },
        AttributionPlanStep {
            id: "dimension_drilldown".to_string(),
            title: "维度下钻归因".to_string(),
            purpose: "自动选择真实存在且适合分组的维度，找出拖动变化最大的分组".to_string(),
            question: format!(
                "请针对这个变化问题做维度下钻：{question}。根据 schema、字段语义、SQL 知识库和历史口径，自动选择最相关且真实存在的业务对象与分组维度，返回各分组的时间序列、目标周期、对比周期、变化量、变化率和贡献排序。先定位哪些对象真正异常，再为异常对象寻找原因。"
            ),
            priority: 3,
        },
    ];
    if matches!(depth, AttributionDepth::Standard | AttributionDepth::Deep) {
        steps.push(AttributionPlanStep {
            id: "funnel_or_quality".to_string(),
            title: "链路和质量检查".to_string(),
            purpose: "检查是否存在链路指标、数据质量或样本结构变化导致的异常".to_string(),
            question: format!(
                "请检查这个归因问题是否存在链路指标、样本结构或数据质量异常：{question}。根据真实字段选择可验证的链路/质量指标，返回目标周期与对比周期的异常点；没有相关字段则明确返回无法覆盖。"
            ),
            priority: 4,
        });
    }
    if matches!(depth, AttributionDepth::Deep) {
        steps.push(AttributionPlanStep {
            id: "longer_baseline".to_string(),
            title: "更长基线验证".to_string(),
            purpose: "判断变化是单日波动、趋势拐点还是周期性变化".to_string(),
            question: format!(
                "请为这个问题补充更长基线验证：{question}。查询合理的近几期趋势或周期对比，判断当前变化是偶发波动、持续趋势还是周期性现象；如果时间字段或历史数据不足则说明限制。"
            ),
            priority: 5,
        });
    }
    AttributionPlan {
        needs_clarification: false,
        clarification_question: None,
        confidence: Some(0.45),
        analysis_focus: vec![
            "主指标变化".to_string(),
            "指标组成拆解".to_string(),
            "维度贡献排序".to_string(),
        ],
        steps,
    }
}

fn soften_plan_clarification(
    plan: AttributionPlan,
    question: &str,
    depth: AttributionDepth,
) -> AttributionPlan {
    if !plan.needs_clarification || is_obviously_ambiguous(question) {
        return plan;
    }

    tracing::info!(
        clarification = plan.clarification_question.as_deref().unwrap_or(""),
        "attribution planner requested clarification for a non-empty question; falling back to executable diagnostic path"
    );
    let mut fallback = default_attribution_plan(question, depth);
    if !plan.analysis_focus.is_empty() {
        fallback.analysis_focus = plan.analysis_focus;
    }
    fallback.confidence = plan.confidence.map(|v| v.min(0.4)).or(Some(0.35));
    fallback
}

fn normalize_attribution_plan(
    mut plan: AttributionPlan,
    question: &str,
    depth: AttributionDepth,
) -> AttributionPlan {
    if plan.needs_clarification {
        return plan;
    }

    let mut required = default_attribution_plan(question, depth).steps;
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let required_ids = required
        .iter()
        .map(|s| s.id.clone())
        .collect::<std::collections::HashSet<_>>();

    for step in required.drain(..) {
        seen.insert(step.id.clone());
        merged.push(step);
    }

    plan.steps.sort_by_key(|s| s.priority);
    for mut step in plan.steps {
        if step.id.trim().is_empty() {
            step.id = format!("extra_{}", merged.len() + 1);
        }
        if step.title.trim().is_empty()
            || step.purpose.trim().is_empty()
            || step.question.trim().is_empty()
        {
            continue;
        }
        if step.priority == 0 {
            step.priority = 50u8.saturating_add(u8::try_from(merged.len()).unwrap_or(0));
        }
        if required_ids.contains(&step.id) {
            if let Some(existing) = merged.iter_mut().find(|s| s.id == step.id) {
                step.priority = existing.priority;
                *existing = step;
            }
        } else if seen.insert(step.id.clone()) {
            merged.push(step);
        }
    }

    // The default core steps are the minimum evidence contract. Model-added
    // extras may fill spare depth budget, but a low numeric priority must not
    // evict metric decomposition or dimension evidence and trigger a second
    // diagnostic SQL loop.
    merged.sort_by_key(|step| (!required_ids.contains(&step.id), step.priority));
    merged.truncate(depth.max_steps());
    plan.steps = merged;
    if plan.analysis_focus.is_empty() {
        plan.analysis_focus = vec![
            "核心指标变化".to_string(),
            "指标组成拆解".to_string(),
            "维度贡献排序".to_string(),
            "异常和数据质量".to_string(),
        ];
    }
    plan
}

fn is_obviously_ambiguous(question: &str) -> bool {
    let trimmed = question.trim();
    if trimmed.is_empty() {
        return true;
    }
    let signal_chars = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
        .count();
    signal_chars < 2
}

fn extract_clarification_from_error(error: &str) -> Option<String> {
    let trimmed = error.trim();
    for marker in ["需要澄清：", "需要澄清:", "CLARIFICATION_NEEDED:"] {
        if let Some((_, rest)) = trimmed.split_once(marker) {
            let q = rest.trim();
            if !q.is_empty() {
                return Some(q.to_string());
            }
        }
    }
    None
}

fn sanitize_attribution_report(
    mut report: AttributionReport,
    observations: &[AttributionObservation],
) -> AttributionReport {
    let valid_step_ids = observations
        .iter()
        .filter(|observation| observation.has_usable_evidence())
        .map(|o| o.step_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    if valid_step_ids.is_empty() {
        report.main_causes.clear();
        if !report
            .caveats
            .iter()
            .any(|c| c.contains("没有成功执行的数据证据"))
        {
            report
                .caveats
                .push("没有成功执行的数据证据，本次不能给出确定原因。".to_string());
        }
        report.confidence = Some("低".to_string());
        return report;
    }

    let before = report.main_causes.len();
    report.main_causes = report
        .main_causes
        .into_iter()
        .filter_map(|mut cause| {
            cause
                .evidence_step_ids
                .retain(|id| valid_step_ids.contains(id.as_str()));
            if cause.evidence_step_ids.is_empty() {
                None
            } else {
                Some(cause)
            }
        })
        .collect();
    if before > report.main_causes.len() {
        report
            .caveats
            .push("已隐藏缺少成功数据证据引用的原因，避免把猜测当成结论。".to_string());
    }
    report
}

fn fallback_report(question: &str, observations: &[AttributionObservation]) -> AttributionReport {
    let success_count = observations
        .iter()
        .filter(|observation| observation.has_usable_evidence())
        .count();
    let failed_count = observations.len().saturating_sub(success_count);
    let executive_summary = if success_count == 0 {
        format!("这次没有成功拿到可用于归因的数据结果，因此不能对“{question}”给出确定原因。建议先检查指标口径、数据源权限、相关表结构和 SQL 知识库是否完整。")
    } else {
        format!("这次成功完成了 {success_count} 个归因查询，失败 {failed_count} 个。可以先参考成功查询的结果判断方向，但由于报告整理模型失败，下面结论只做保守汇总。")
    };
    AttributionReport {
        title: "数据归因结果".to_string(),
        executive_summary,
        metric_answer: None,
        main_causes: observations
            .iter()
            .filter(|observation| observation.has_usable_evidence())
            .take(3)
            .map(|o| AttributionDriver {
                title: o.title.clone(),
                explanation: format!(
                    "该步骤返回了 {} 行结果，可作为判断该方向的证据。请展开证据查看具体数据。",
                    o.row_count
                ),
                impact: None,
                evidence_step_ids: vec![o.step_id.clone()],
                confidence: Some("低".to_string()),
            })
            .collect(),
        recommendations: vec![
            "补齐指标口径和常用归因维度后重新运行，能显著提升准确性。".to_string()
        ],
        caveats: observations
            .iter()
            .filter_map(|o| o.error.as_ref().map(|e| format!("{}：{}", o.title, e)))
            .collect(),
        next_questions: Vec::new(),
        confidence: Some(if success_count == 0 { "低" } else { "中" }.to_string()),
        coverage: Some(format!(
            "成功查询 {success_count} 个方向，失败 {failed_count} 个方向。"
        )),
    }
}

fn extract_json_object(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.starts_with('{')
        && trimmed.ends_with('}')
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Some(trimmed.to_string());
    }
    let start = trimmed.find('{')?;
    let fragment = &trimmed[start..];

    // Models that expose reasoning sometimes spend the entire output budget
    // before emitting the final `]}`. Extracting from the first `{` to the
    // last `}` cannot recover that otherwise valid prefix. Track JSON strings
    // and delimiters, then close only containers that are actually open.
    let (candidate, stack) = json_fragment_and_stack(fragment);
    let repaired = close_json_fragment(&candidate, &stack);
    if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
        return Some(repaired);
    }

    // If truncation happened inside the final array item, discard incomplete
    // tail items one comma at a time. This is bounded and only accepts a
    // result that parses as JSON, so malformed model output never becomes
    // trusted data silently.
    let mut search_end = candidate.len();
    for _ in 0..16 {
        let Some(comma) = candidate[..search_end].rfind(',') else {
            break;
        };
        let prefix = candidate[..comma].trim_end();
        let (prefix, prefix_stack) = json_fragment_and_stack(prefix);
        let repaired = close_json_fragment(&prefix, &prefix_stack);
        if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
            return Some(repaired);
        }
        search_end = comma;
    }
    Some(repaired)
}

fn json_fragment_and_stack(fragment: &str) -> (String, Vec<char>) {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut end = fragment.len();
    for (index, ch) in fragment.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.last().copied() == Some(ch) {
                    stack.pop();
                    if stack.is_empty() {
                        end = index + ch.len_utf8();
                        break;
                    }
                } else {
                    end = index;
                    break;
                }
            }
            _ => {}
        }
    }
    let mut candidate = fragment[..end].trim().to_string();
    while candidate.ends_with(',') || candidate.ends_with(':') {
        candidate.pop();
        candidate = candidate.trim_end().to_string();
    }
    (candidate, stack)
}

fn close_json_fragment(candidate: &str, stack: &[char]) -> String {
    let mut output = candidate.trim().to_string();
    for closer in stack.iter().rev() {
        output.push(*closer);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_retrieval_query_excludes_model_orchestration_boilerplate() {
        let query = attribution_step_retrieval_question(
            "有没有哪些 app 的 ROI 持续下降？",
            "按 app 和日期计算 ROI 趋势及突降幅度",
        );

        assert!(query.contains("ROI"));
        assert!(query.contains("按 app 和日期"));
        assert!(!query.contains("请围绕原始问题生成"));
        assert!(!query.contains("不要继续规划"));
        assert!(!query.contains("超时恢复查询"));
    }

    #[tokio::test]
    async fn attribution_progress_history_is_stable_paged_and_owner_scoped() {
        let task_id = format!("progress-test-{}", uuid::Uuid::new_v4());
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let user_id = format!("user-{}", uuid::Uuid::new_v4());
        task_manager()
            .create_task(&task_id, &tenant_id, &user_id)
            .await
            .expect("task should be created");
        task_manager()
            .publish_stage_progress(&task_id, "plan", "planning", Some(18), None, None, None)
            .await;

        let first_page =
            load_attribution_task_progress_events(&task_id, &tenant_id, &user_id, 0, 1).await;
        let second_page =
            load_attribution_task_progress_events(&task_id, &tenant_id, &user_id, 1, 10).await;

        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].0, 1);
        assert_eq!(first_page[0].1.stage.as_deref(), Some("queued"));
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].0, 2);
        assert_eq!(second_page[0].1.stage.as_deref(), Some("plan"));
        assert!(load_attribution_task_progress_events(
            &task_id,
            &tenant_id,
            "different-user",
            0,
            10,
        )
        .await
        .is_empty());
        assert!(
            attribution_task_progress_snapshot(&task_id, "different-tenant", &user_id)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn attribution_progress_events_round_trip_through_sqlite() {
        let db = crate::test_sqlite_pool().await;
        let claims = Claims::new(
            "user-progress",
            "progress@example.com",
            "admin",
            "tenant-progress",
        );
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_attribution_tasks
             (task_id, tenant_id, user_id, conversation_id, question, status)
             VALUES ('task-progress', 'tenant-progress', 'user-progress', 'conversation-progress', 'why', 'running')",
        )
        .execute(&db)
        .await
        .expect("insert attribution task fixture");
        let event = AttributionTaskEvent {
            task_id: "task-progress".to_string(),
            status: "running".to_string(),
            stage: Some("execute".to_string()),
            message: Some("step completed".to_string()),
            elapsed_ms: 42,
            stage_elapsed_ms: Some(20),
            progress_percent: Some(50),
            step_index: Some(1),
            step_total: Some(2),
            observation: Some(AttributionObservation {
                step_id: "main_metric".to_string(),
                title: "main".to_string(),
                purpose: "compare".to_string(),
                question: "compare metric".to_string(),
                datasource_ids: vec!["ds-1".to_string()],
                time_context: None,
                query_id: Some("query-1".to_string()),
                conversation_id: Some("conversation-progress".to_string()),
                columns: vec!["metric".to_string()],
                rows: vec![serde_json::json!({"metric": 1})],
                row_count: 1,
                sampled: false,
                sqls: vec!["SELECT 1 AS metric".to_string()],
                used_references: Vec::new(),
                error: None,
                elapsed_ms: 40,
            }),
            response: None,
            error: None,
        };

        persist_attribution_progress_event(&db, &claims, &event).await;
        let restored =
            load_persisted_attribution_progress_events(&db, &claims, "task-progress").await;

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].stage.as_deref(), Some("execute"));
        assert_eq!(
            restored[0]
                .observation
                .as_ref()
                .map(|observation| observation.step_id.as_str()),
            Some("main_metric")
        );
    }

    #[tokio::test]
    async fn interrupted_attribution_recovery_keeps_durable_progress() {
        let db = crate::test_sqlite_pool().await;
        sqlx::query::<sqlx::Sqlite>(
            "INSERT INTO nl2sql_attribution_tasks
             (task_id, tenant_id, user_id, conversation_id, question, status, progress_events_json)
             VALUES ('task-interrupted', 'tenant-1', 'user-1', 'conversation-1', 'why', 'running', '[{\"stage\":\"execute\"}]')",
        )
        .execute(&db)
        .await
        .expect("insert interrupted attribution fixture");

        assert_eq!(recover_interrupted_attribution_tasks(&db).await.unwrap(), 1);
        let row = sqlx::query::<sqlx::Sqlite>(
            "SELECT status, error, progress_events_json FROM nl2sql_attribution_tasks WHERE task_id = 'task-interrupted'",
        )
        .fetch_one(&db)
        .await
        .expect("load recovered attribution task");
        assert_eq!(row.get::<String, _>("status"), "failed");
        assert!(row.get::<String, _>("error").contains("restarted"));
        assert_eq!(
            row.get::<String, _>("progress_events_json"),
            "[{\"stage\":\"execute\"}]"
        );
    }

    #[test]
    fn extracts_json_from_markdown_noise() {
        let text = "```json\n{\"title\":\"x\"}\n```";
        assert_eq!(
            extract_json_object(text).as_deref(),
            Some("{\"title\":\"x\"}")
        );
    }

    #[test]
    fn repairs_json_truncated_after_array_item() {
        let text = "prefix {\"steps\":[{\"id\":\"main\",\"title\":\"x\"}";
        let extracted = extract_json_object(text).expect("json should be extracted");
        let value: serde_json::Value = serde_json::from_str(&extracted).expect("json repaired");
        assert_eq!(value["steps"][0]["id"], "main");
    }

    #[test]
    fn default_plan_scales_with_depth() {
        assert_eq!(
            default_attribution_plan("why", AttributionDepth::Fast)
                .steps
                .len(),
            3
        );
        assert_eq!(
            default_attribution_plan("why", AttributionDepth::Standard)
                .steps
                .len(),
            4
        );
        assert_eq!(
            default_attribution_plan("why", AttributionDepth::Deep)
                .steps
                .len(),
            5
        );
    }

    #[test]
    fn obvious_ambiguity_guard_catches_empty_signal() {
        assert!(is_obviously_ambiguous("???"));
        assert!(is_obviously_ambiguous("？"));
        assert!(!is_obviously_ambiguous("昨天收入为什么下降"));
    }

    #[test]
    fn super_assistant_conversation_id_extracts_session_id() {
        assert_eq!(
            super_assistant_session_id_from_attribution_conversation(
                "super-assistant-2170fe94-f735-4a08-9e83-2c74fcc7e099"
            ),
            Some("2170fe94-f735-4a08-9e83-2c74fcc7e099")
        );
        assert_eq!(
            super_assistant_session_id_from_attribution_conversation("attr-123"),
            None
        );
        assert_eq!(
            super_assistant_session_id_from_attribution_conversation("super-assistant-   "),
            None
        );
    }

    #[test]
    fn attribution_archive_tool_call_keeps_sql_results_and_failure_reason() {
        let observations = vec![AttributionObservation {
            step_id: "step-1".to_string(),
            title: "按版本拆解".to_string(),
            purpose: "定位下降来源".to_string(),
            question: "各版本 ROI 是多少".to_string(),
            datasource_ids: Vec::new(),
            time_context: None,
            query_id: Some("query-1".to_string()),
            conversation_id: Some("super-assistant-session-1".to_string()),
            columns: vec!["app_version".to_string(), "roi".to_string()],
            rows: vec![serde_json::json!({"app_version": "1.2.3", "roi": 0.8})],
            row_count: 1,
            sampled: false,
            sqls: vec!["SELECT app_version, roi FROM metrics".to_string()],
            used_references: Vec::new(),
            error: Some("query timeout".to_string()),
            elapsed_ms: 1234,
        }];

        let calls = attribution_observation_tool_calls(&observations);

        assert_eq!(calls.len(), 1);
        assert!(calls[0].input.contains("SELECT app_version, roi"));
        assert!(calls[0].output.contains("1.2.3"));
        assert!(calls[0].output.contains("query timeout"));
        assert!(calls[0].is_error);
        assert_eq!(calls[0].duration_ms, 1234);
    }

    #[test]
    fn attribution_analysis_question_keeps_current_question_authoritative() {
        let text = build_attribution_analysis_question(
            "昨天 ROI 为什么下降？",
            Some("上一轮用户说产品是 A，ROI 口径是收入 / 成本。"),
        );

        assert!(text.contains("共享会话背景"));
        assert!(text.contains("不得覆盖用户当前问题"));
        assert!(text.contains("用户当前问题（最高优先级）：\n昨天 ROI 为什么下降？"));
    }

    #[test]
    fn attribution_analysis_context_is_bounded() {
        let long_context = "x".repeat(13_000);
        let text = build_attribution_analysis_question("查收入", Some(&long_context));

        assert!(text.contains("用户当前问题（最高优先级）：\n查收入"));
        assert!(text.len() < long_context.len() + 200);
    }

    #[test]
    fn attribution_analysis_context_preserves_super_assistant_recent_tail_when_bounded() {
        let context = format!(
            "{}\n\n{SUPER_ASSISTANT_EXACT_RECENT_TAIL_HEADER}\n用户：\n上轮明确产品是 A\n\n助手：\n上轮确认 ROI 口径是收入 / 成本",
            "older recalled attribution context ".repeat(1_000)
        );

        let text = build_attribution_analysis_question("继续分析昨天", Some(&context));

        assert!(text.contains("...[older attribution context truncated]"));
        assert!(text.contains("上轮明确产品是 A"));
        assert!(text.contains("上轮确认 ROI 口径是收入 / 成本"));
        assert!(text.contains("用户当前问题（最高优先级）：\n继续分析昨天"));
        assert!(
            !text.contains("older recalled attribution context older recalled attribution context")
        );
    }

    #[test]
    fn attribution_followup_shared_context_preserves_super_assistant_recent_tail_when_bounded() {
        let context = format!(
            "{}\n\n{SUPER_ASSISTANT_EXACT_RECENT_TAIL_HEADER}\n用户：\n刚刚把分析对象改成 B 产品\n\n助手：\n已确认要重新按 B 产品看 ROI",
            "older followup context ".repeat(1_000)
        );

        let normalized =
            normalize_followup_shared_context(Some(&context)).expect("context should remain");

        assert!(normalized.contains("...[older attribution context truncated]"));
        assert!(normalized.contains("刚刚把分析对象改成 B 产品"));
        assert!(normalized.contains("已确认要重新按 B 产品看 ROI"));
        assert!(!normalized.contains("older followup context older followup context"));
    }

    #[test]
    fn attribution_terminal_message_keeps_boss_readable_report_and_evidence_health() {
        let response = AttributionAnalyzeResponse {
            status: "completed".to_string(),
            question: "昨天 ROI 为什么下降".to_string(),
            depth: "standard".to_string(),
            conversation_id: Some("super-assistant-session-1".to_string()),
            clarification_question: None,
            report: Some(AttributionReport {
                title: "ROI 下滑归因".to_string(),
                executive_summary: "昨天 ROI 环比下降，主要由成本抬升驱动。".to_string(),
                metric_answer: Some("ROI=0.82，环比 -12%。".to_string()),
                main_causes: vec![AttributionDriver {
                    title: "投放成本上升".to_string(),
                    explanation: "渠道 A 成本上涨且收入未同步增长。".to_string(),
                    impact: None,
                    evidence_step_ids: vec!["cost_by_channel".to_string()],
                    confidence: Some("高".to_string()),
                }],
                recommendations: vec!["优先检查渠道 A 出价和素材消耗。".to_string()],
                caveats: Vec::new(),
                next_questions: Vec::new(),
                confidence: Some("高".to_string()),
                coverage: None,
            }),
            plan: None,
            observations: Vec::new(),
            evidence_health: AttributionEvidenceHealth {
                total_steps: 3,
                execution_succeeded_steps: 2,
                usable_evidence_steps: 2,
                zero_row_steps: 0,
                successful_steps: 2,
                failed_steps: 1,
                sampled_steps: 0,
                total_rows: 42,
            },
            evidence_cards: Vec::new(),
            total_execution_ms: 1000,
            error: None,
        };

        let text = format_attribution_terminal_message(&response);
        assert!(text.contains("## ROI 下滑归因"));
        assert!(text.contains("**核心结论**"));
        assert!(text.contains("ROI=0.82"));
        assert!(text.contains("投放成本上升"));
        assert!(text.contains("证据覆盖：成功 2/3 步，返回 42 行。"));
    }

    #[test]
    fn extracts_clarification_from_agent_error() {
        assert_eq!(
            extract_clarification_from_error("需要澄清：请选择 ROI 口径").as_deref(),
            Some("请选择 ROI 口径")
        );
        assert_eq!(
            extract_clarification_from_error("CLARIFICATION_NEEDED: Which baseline?").as_deref(),
            Some("Which baseline?")
        );
        assert!(extract_clarification_from_error("query failed").is_none());
    }

    #[test]
    fn clarification_is_decided_after_all_completed_steps_are_preserved() {
        let failed_observation = |step_id: &str| AttributionObservation {
            step_id: step_id.to_string(),
            title: step_id.to_string(),
            purpose: "test".to_string(),
            question: "test".to_string(),
            datasource_ids: Vec::new(),
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some("needs input".to_string()),
            elapsed_ms: 1,
        };
        let observations = vec![
            failed_observation("metric_decomposition"),
            failed_observation("dimension_drilldown"),
            failed_observation("funnel_or_quality"),
        ];
        let candidates = vec![
            (
                "metric_decomposition".to_string(),
                "补充指标口径".to_string(),
            ),
            (
                "dimension_drilldown".to_string(),
                "补充分组维度".to_string(),
            ),
        ];

        assert_eq!(
            clarification_after_completed_steps(&candidates, &observations).as_deref(),
            Some("补充指标口径")
        );
        assert_eq!(observations.len(), 3);
    }

    #[test]
    fn diagnostic_loop_runs_only_when_successful_core_evidence_is_incomplete() {
        assert!(!should_run_diagnostic_loop(&[]));
        let failed = AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "查主指标".to_string(),
            question: "查收入".to_string(),
            datasource_ids: Vec::new(),
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some("failed".to_string()),
            elapsed_ms: 1,
        };
        assert!(!should_run_diagnostic_loop(&[failed]));
        let success = AttributionObservation {
            error: None,
            row_count: 1,
            rows: vec![serde_json::json!({"metric": 1})],
            sqls: vec!["SELECT 1 AS metric".to_string()],
            ..AttributionObservation {
                step_id: "main_metric".to_string(),
                title: "主指标".to_string(),
                purpose: "查主指标".to_string(),
                question: "查收入".to_string(),
                datasource_ids: Vec::new(),
                time_context: None,
                query_id: None,
                conversation_id: None,
                columns: vec!["metric".to_string()],
                rows: Vec::new(),
                row_count: 0,
                sampled: false,
                sqls: Vec::new(),
                used_references: Vec::new(),
                error: None,
                elapsed_ms: 1,
            }
        };
        assert!(should_run_diagnostic_loop(&[success]));

        let complete = ["main_metric", "metric_decomposition", "dimension_drilldown"]
            .into_iter()
            .map(|step_id| AttributionObservation {
                step_id: step_id.to_string(),
                title: step_id.to_string(),
                purpose: "verified evidence".to_string(),
                question: "query".to_string(),
                datasource_ids: Vec::new(),
                time_context: None,
                query_id: None,
                conversation_id: None,
                columns: vec!["metric".to_string()],
                rows: vec![serde_json::json!({"metric": 1})],
                row_count: 1,
                sampled: false,
                sqls: vec!["SELECT 1".to_string()],
                used_references: Vec::new(),
                error: None,
                elapsed_ms: 1,
            })
            .collect::<Vec<_>>();
        assert!(should_run_diagnostic_loop(&complete));

        let mut complete_with_optional_failure = complete;
        complete_with_optional_failure.push(AttributionObservation {
            step_id: "funnel_or_quality".to_string(),
            title: "链路和质量检查".to_string(),
            purpose: "optional evidence".to_string(),
            question: "check quality".to_string(),
            datasource_ids: Vec::new(),
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some("connector does not expose the optional field".to_string()),
            elapsed_ms: 1,
        });
        assert!(should_run_diagnostic_loop(&complete_with_optional_failure));
        complete_with_optional_failure.push(AttributionObservation {
            step_id: "robustness_check".to_string(),
            title: "稳健性检查".to_string(),
            purpose: "验证候选原因".to_string(),
            question: "检查分层反转".to_string(),
            datasource_ids: Vec::new(),
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: vec!["metric".to_string()],
            rows: vec![serde_json::json!({"metric": 1})],
            row_count: 1,
            sampled: false,
            sqls: vec!["SELECT 1".to_string()],
            used_references: Vec::new(),
            error: None,
            elapsed_ms: 1,
        });
        assert!(!should_run_diagnostic_loop(&complete_with_optional_failure));
    }

    #[test]
    fn failed_or_empty_attribution_evidence_skips_llm_synthesis() {
        let failed = AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "查主指标".to_string(),
            question: "查 ROI".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: vec!["SELECT roi FROM metrics".to_string()],
            used_references: Vec::new(),
            error: Some("trino query failed: connection refused".to_string()),
            elapsed_ms: 10,
        };
        assert!(should_skip_attribution_synthesis(&[failed]));

        let empty = AttributionObservation {
            error: None,
            row_count: 0,
            columns: vec!["roi".to_string()],
            sqls: vec!["SELECT roi FROM metrics".to_string()],
            ..failed_observation_for_synthesis_test()
        };
        assert!(should_skip_attribution_synthesis(&[empty]));

        let usable = AttributionObservation {
            error: None,
            row_count: 1,
            rows: vec![serde_json::json!({"roi": 1.0})],
            columns: vec!["roi".to_string()],
            sqls: vec!["SELECT 1 AS roi".to_string()],
            ..failed_observation_for_synthesis_test()
        };
        assert!(!should_skip_attribution_synthesis(&[usable]));
    }

    fn failed_observation_for_synthesis_test() -> AttributionObservation {
        AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "查主指标".to_string(),
            question: "查 ROI".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: None,
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some("connection failed".to_string()),
            elapsed_ms: 10,
        }
    }

    #[test]
    fn evidence_cards_are_compact_and_keep_step_ids() {
        let obs = AttributionObservation {
            step_id: "dimension_drilldown".to_string(),
            title: "维度下钻".to_string(),
            purpose: "查贡献".to_string(),
            question: "按维度查贡献".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: Some("昨天 vs 前天".to_string()),
            query_id: Some("q1".to_string()),
            conversation_id: Some("c1".to_string()),
            columns: (0..80).map(|i| format!("col_{i}")).collect(),
            rows: (0..40)
                .map(|i| serde_json::json!({"dim": format!("g{i}"), "delta": i}))
                .collect(),
            row_count: 40,
            sampled: true,
            sqls: vec!["select 1".to_string()],
            used_references: Vec::new(),
            error: None,
            elapsed_ms: 1,
        };
        let cards = build_evidence_cards(&[obs]);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].step_id, "dimension_drilldown");
        assert_eq!(cards[0].status, "success");
        assert!(cards[0].columns.len() <= max_evidence_card_columns());
        assert!(cards[0].rows_preview.len() <= max_evidence_card_rows());
        assert!(!cards[0].numeric_highlights.is_empty());
    }

    #[test]
    fn zero_row_execution_is_not_counted_as_usable_attribution_evidence() {
        let observation = AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "验证目标期主指标".to_string(),
            question: "昨天收入是多少".to_string(),
            datasource_ids: vec!["ds-empty".to_string()],
            time_context: Some("昨天 vs 前天".to_string()),
            query_id: Some("query-empty".to_string()),
            conversation_id: Some("conversation-empty".to_string()),
            columns: vec!["revenue".to_string()],
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: vec!["SELECT revenue FROM metrics WHERE day = CURRENT_DATE".to_string()],
            used_references: Vec::new(),
            error: None,
            elapsed_ms: 12,
        };

        let health = AttributionEvidenceHealth::from_observations(&[observation.clone()]);
        assert_eq!(health.total_steps, 1);
        assert_eq!(health.execution_succeeded_steps, 1);
        assert_eq!(health.usable_evidence_steps, 0);
        assert_eq!(health.zero_row_steps, 1);
        assert_eq!(health.successful_steps, 0);
        assert_eq!(health.failed_steps, 0);

        let cards = build_evidence_cards(&[observation]);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].status, "no_data");
        assert_eq!(cards[0].row_count, 0);
    }

    #[test]
    fn contextual_question_preserves_followup_and_previous_evidence() {
        let previous = PreviousAttributionContext {
            task_id: "task_1".to_string(),
            question: "昨天收入为什么下降".to_string(),
            response: AttributionAnalyzeResponse {
                status: "completed".to_string(),
                question: "昨天收入为什么下降".to_string(),
                depth: "standard".to_string(),
                conversation_id: Some("conv_1".to_string()),
                clarification_question: None,
                report: Some(AttributionReport {
                    title: "收入下降归因".to_string(),
                    executive_summary: "渠道 A 贡献最大。".to_string(),
                    metric_answer: Some("收入下降 10%。".to_string()),
                    main_causes: Vec::new(),
                    recommendations: Vec::new(),
                    caveats: Vec::new(),
                    next_questions: Vec::new(),
                    confidence: Some("中".to_string()),
                    coverage: None,
                }),
                plan: None,
                observations: Vec::new(),
                evidence_health: AttributionEvidenceHealth::empty(),
                evidence_cards: vec![AttributionEvidenceCard {
                    step_id: "dimension_drilldown".to_string(),
                    title: "渠道贡献".to_string(),
                    purpose: "查渠道".to_string(),
                    question: "按渠道查".to_string(),
                    datasource_ids: vec!["ds-1".to_string()],
                    time_context: Some("昨天 vs 前天".to_string()),
                    status: "success".to_string(),
                    row_count: 3,
                    sampled: false,
                    columns: vec!["channel".to_string(), "delta".to_string()],
                    rows_preview: Vec::new(),
                    numeric_highlights: vec!["channel=A, delta=-10".to_string()],
                    sql_count: 1,
                    reference_files: Vec::new(),
                    error: None,
                    evidence_refs: Vec::new(),
                }],
                total_execution_ms: 100,
                error: None,
            },
            evidence_cards: vec![AttributionEvidenceCard {
                step_id: "dimension_drilldown".to_string(),
                title: "渠道贡献".to_string(),
                purpose: "查渠道".to_string(),
                question: "按渠道查".to_string(),
                datasource_ids: vec!["ds-1".to_string()],
                time_context: Some("昨天 vs 前天".to_string()),
                status: "success".to_string(),
                row_count: 3,
                sampled: false,
                columns: vec!["channel".to_string(), "delta".to_string()],
                rows_preview: Vec::new(),
                numeric_highlights: vec!["channel=A, delta=-10".to_string()],
                sql_count: 1,
                reference_files: Vec::new(),
                error: None,
                evidence_refs: Vec::new(),
            }],
        };
        let text = build_contextual_attribution_question("那按版本呢？", &previous);
        assert!(text.contains("同一个数据归因会话"));
        assert!(text.contains("昨天收入为什么下降"));
        assert!(text.contains("那按版本呢"));
        assert!(text.contains("dimension_drilldown"));
    }

    #[test]
    fn sanitize_followup_steps_dedupes_and_caps_budget() {
        let mut seen_ids = std::collections::HashSet::from(["existing".to_string()]);
        let mut seen_questions =
            std::collections::HashSet::from([normalize_step_question_key("重复问题")]);
        let steps = vec![
            AttributionPlanStep {
                id: "existing".to_string(),
                title: "渠道贡献".to_string(),
                purpose: "查渠道贡献".to_string(),
                question: "按渠道查询贡献度".to_string(),
                priority: 0,
            },
            AttributionPlanStep {
                id: "duplicate_question".to_string(),
                title: "重复".to_string(),
                purpose: "重复".to_string(),
                question: "重复问题".to_string(),
                priority: 0,
            },
            AttributionPlanStep {
                id: "device drill".to_string(),
                title: "设备贡献".to_string(),
                purpose: "查设备贡献".to_string(),
                question: "按设备查询贡献度".to_string(),
                priority: 0,
            },
        ];
        let out = sanitize_followup_steps(steps, 1, 2, &mut seen_ids, &mut seen_questions);
        assert_eq!(out.len(), 2);
        assert_ne!(out[0].id, "existing");
        assert!(out[0].priority >= 80);
        assert_eq!(out[1].id, "device_drill");
    }

    #[test]
    fn normalize_plan_keeps_required_attribution_steps() {
        let raw = AttributionPlan {
            needs_clarification: false,
            clarification_question: None,
            confidence: Some(0.8),
            analysis_focus: Vec::new(),
            steps: vec![AttributionPlanStep {
                id: "custom".to_string(),
                title: "自定义检查".to_string(),
                purpose: "检查额外方向".to_string(),
                question: "查额外方向".to_string(),
                priority: 1,
            }],
        };
        let plan =
            normalize_attribution_plan(raw, "昨天指标为什么下降", AttributionDepth::Standard);
        let ids = plan.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>();
        assert!(ids.contains(&"main_metric"));
        assert!(ids.contains(&"metric_decomposition"));
        assert!(ids.contains(&"dimension_drilldown"));
        assert!(ids.contains(&"funnel_or_quality"));
        assert!(!ids.contains(&"custom"));
    }

    #[test]
    fn non_empty_question_does_not_stop_on_planner_clarification() {
        let raw = AttributionPlan {
            needs_clarification: true,
            clarification_question: Some("请补充业务对象".to_string()),
            confidence: Some(0.7),
            analysis_focus: vec!["原因分析".to_string()],
            steps: Vec::new(),
        };
        let plan = normalize_attribution_plan(
            soften_plan_clarification(raw, "昨天核心指标为什么下降", AttributionDepth::Standard),
            "昨天核心指标为什么下降",
            AttributionDepth::Standard,
        );
        assert!(!plan.needs_clarification);
        assert!(plan.steps.iter().any(|s| s.id == "main_metric"));
        assert!(plan.steps.iter().any(|s| s.id == "dimension_drilldown"));
    }

    #[test]
    fn attribution_boss_change_question_keeps_diagnostic_workflow() {
        let raw = AttributionPlan {
            needs_clarification: false,
            clarification_question: None,
            confidence: Some(0.9),
            analysis_focus: vec!["原因分析".to_string()],
            steps: Vec::new(),
        };
        let plan = normalize_attribution_plan(
            raw,
            "昨天某产品 ROI 为什么比前天下降这么多？分析下原因",
            AttributionDepth::Standard,
        );
        let ids = plan.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>();

        assert_eq!(ids[0], "main_metric");
        assert!(ids.contains(&"metric_decomposition"));
        assert!(ids.contains(&"dimension_drilldown"));
        assert!(ids.contains(&"funnel_or_quality"));
        assert!(plan
            .steps
            .iter()
            .any(|s| s.question.contains("目标周期") && s.question.contains("对比周期")));
        assert!(plan
            .steps
            .iter()
            .any(|s| s.question.contains("真实字段") || s.question.contains("真实指标口径")));
    }

    #[test]
    fn trend_attribution_plan_preserves_series_object_and_ratio_evidence() {
        let plan = default_attribution_plan(
            "有没有哪些 app 的 ROI 持续下降或者骤降，原因是什么",
            AttributionDepth::Deep,
        );
        let main = plan
            .steps
            .iter()
            .find(|step| step.id == "main_metric")
            .expect("main metric");
        let decomposition = plan
            .steps
            .iter()
            .find(|step| step.id == "metric_decomposition")
            .expect("metric decomposition");
        let drilldown = plan
            .steps
            .iter()
            .find(|step| step.id == "dimension_drilldown")
            .expect("dimension drilldown");

        assert!(main.question.contains("连续时间序列"));
        assert!(main.question.contains("不要写死固定阈值"));
        assert!(decomposition.question.contains("分子、分母"));
        assert!(drilldown.question.contains("先定位哪些对象真正异常"));
        assert!(
            attribution_step_budget_for(AttributionDepth::Deep, main)
                > attribution_step_budget(AttributionDepth::Deep)
        );
    }

    #[test]
    fn step_timeout_reserves_time_for_single_sql_recovery() {
        let deadline = Instant::now() + Duration::from_secs(150);
        let primary_deadline = attribution_primary_attempt_deadline(deadline);
        let recovery_budget = deadline.saturating_duration_since(primary_deadline);

        assert!(primary_deadline < deadline);
        assert!(recovery_budget >= Duration::from_secs(35));
        assert!(recovery_budget <= Duration::from_secs(70));
    }

    #[test]
    fn normalize_plan_prefers_llm_required_step_questions() {
        let raw = AttributionPlan {
            needs_clarification: false,
            clarification_question: None,
            confidence: Some(0.8),
            analysis_focus: Vec::new(),
            steps: vec![AttributionPlanStep {
                id: "main_metric".to_string(),
                title: "GMV 主指标对比".to_string(),
                purpose: "确认 GMV 环比变化".to_string(),
                question: "按订单事实表查询昨天和前天 GMV、订单数、客单价的变化".to_string(),
                priority: 9,
            }],
        };
        let plan = normalize_attribution_plan(raw, "昨天 GMV 为什么下降", AttributionDepth::Fast);
        let main = plan
            .steps
            .iter()
            .find(|s| s.id == "main_metric")
            .expect("main metric step");
        assert_eq!(main.title, "GMV 主指标对比");
        assert!(main.question.contains("订单事实表"));
        assert_eq!(main.priority, 1);
    }

    #[test]
    fn attribution_terminal_statuses_match_stream_contract() {
        for status in [
            "completed",
            "clarification_needed",
            "no_data",
            "partial",
            "failed",
            "cancelled",
        ] {
            assert!(
                attribution_status_is_terminal(status),
                "{status} must close the attribution SSE stream"
            );
        }
        for status in ["queued", "running", "planning", "executing"] {
            assert!(
                !attribution_status_is_terminal(status),
                "{status} must keep the attribution SSE stream open"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_attribution_task_ignores_late_completion() {
        let manager = AttributionTaskManager::new();
        let task_id = format!("test-cancel-{}", uuid::Uuid::new_v4());
        manager
            .create_task(&task_id, "tenant-test", "user-test")
            .await
            .expect("create attribution task");

        let cancelled = manager
            .cancel(&task_id, "tenant-test", "user-test")
            .await
            .expect("cancel attribution task");
        assert_eq!(cancelled.status, "cancelled");

        manager
            .publish_completed(
                &task_id,
                AttributionAnalyzeResponse {
                    status: "completed".to_string(),
                    question: "昨天 ROI 为什么下降".to_string(),
                    depth: "fast".to_string(),
                    conversation_id: Some("conv-test".to_string()),
                    clarification_question: None,
                    report: None,
                    plan: None,
                    observations: Vec::new(),
                    evidence_health: AttributionEvidenceHealth::empty(),
                    evidence_cards: Vec::new(),
                    total_execution_ms: 1,
                    error: None,
                },
            )
            .await;

        let snapshot = manager
            .snapshot(&task_id, "tenant-test", "user-test")
            .await
            .expect("snapshot after late completion");
        assert_eq!(snapshot.status, "cancelled");
        assert!(manager.is_cancelled(&task_id).await);
    }

    #[tokio::test]
    async fn completed_attribution_persists_json_objects_in_sqlite() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::query(
            "CREATE TABLE nl2sql_attribution_tasks (
                task_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                status TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                summary TEXT,
                response_json TEXT,
                evidence_cards_json TEXT,
                error TEXT,
                total_execution_ms INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&db)
        .await
        .expect("create attribution task table");
        sqlx::query(
            "INSERT INTO nl2sql_attribution_tasks
             (task_id, tenant_id, user_id, status) VALUES ('task-1', 'tenant-1', 'user-1', 'running')",
        )
        .execute(&db)
        .await
        .expect("insert attribution task");
        let claims = Claims::new("user-1", "user@example.com", "admin", "tenant-1");
        let response = AttributionAnalyzeResponse {
            status: "completed".to_string(),
            question: "昨天 ROI 为什么下降".to_string(),
            depth: "standard".to_string(),
            conversation_id: None,
            clarification_question: None,
            report: None,
            plan: None,
            observations: Vec::new(),
            evidence_health: AttributionEvidenceHealth::empty(),
            evidence_cards: Vec::new(),
            total_execution_ms: 123,
            error: None,
        };

        persist_attribution_task_completed(&db, &claims, "task-1", &response).await;

        let (response_json, evidence_json): (String, String) = sqlx::query_as(
            "SELECT response_json, evidence_cards_json
             FROM nl2sql_attribution_tasks WHERE task_id = 'task-1'",
        )
        .fetch_one(&db)
        .await
        .expect("read persisted attribution JSON");
        let stored_response: serde_json::Value =
            serde_json::from_str(&response_json).expect("response must remain JSON object text");
        let stored_evidence: serde_json::Value =
            serde_json::from_str(&evidence_json).expect("evidence must remain JSON array text");
        assert_eq!(stored_response["status"], "completed");
        assert_eq!(stored_response["totalExecutionMs"], 123);
        assert_eq!(stored_evidence, serde_json::json!([]));
    }

    #[test]
    fn corrupted_legacy_response_recovers_all_progress_observations() {
        let observation = AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "ROI 趋势".to_string(),
            purpose: "识别下降 app".to_string(),
            question: "按 app 和日期统计 ROI".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: None,
            query_id: Some("query-1".to_string()),
            conversation_id: Some("conv-1".to_string()),
            columns: vec!["app".to_string(), "roi".to_string()],
            rows: vec![serde_json::json!({"app": "demo", "roi": 0.8})],
            row_count: 1,
            sampled: false,
            sqls: vec!["SELECT app, roi FROM roi_daily".to_string()],
            used_references: Vec::new(),
            error: None,
            elapsed_ms: 10,
        };
        let progress = serde_json::to_string(&vec![AttributionTaskEvent {
            task_id: "task-legacy".to_string(),
            status: "running".to_string(),
            stage: Some("execute".to_string()),
            message: Some("完成主指标".to_string()),
            elapsed_ms: 10,
            stage_elapsed_ms: Some(10),
            progress_percent: Some(60),
            step_index: Some(1),
            step_total: Some(1),
            observation: Some(observation),
            response: None,
            error: None,
        }])
        .expect("serialize progress events");

        let recovered = recover_attribution_response_from_progress(
            "哪些 app 的 ROI 下降".to_string(),
            "standard".to_string(),
            "conv-1".to_string(),
            "completed".to_string(),
            10,
            None,
            Some(progress),
        )
        .expect("recover response from progress");

        assert_eq!(recovered.observations.len(), 1);
        assert_eq!(recovered.observations[0].step_id, "main_metric");
        assert_eq!(recovered.evidence_cards.len(), 1);
        assert_eq!(recovered.evidence_health.successful_steps, 1);
    }

    #[test]
    fn sanitize_report_drops_unsupported_causes() {
        let observations = vec![AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "查主指标".to_string(),
            question: "查昨天收入".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: Some("昨天".to_string()),
            query_id: None,
            conversation_id: None,
            columns: vec!["revenue".to_string()],
            rows: vec![serde_json::json!({"revenue": 10})],
            row_count: 1,
            sampled: false,
            sqls: vec!["SELECT revenue FROM revenue_daily".to_string()],
            used_references: Vec::new(),
            error: None,
            elapsed_ms: 1,
        }];
        let report = AttributionReport {
            title: "归因".to_string(),
            executive_summary: "摘要".to_string(),
            metric_answer: None,
            main_causes: vec![
                AttributionDriver {
                    title: "有证据".to_string(),
                    explanation: "来自主指标".to_string(),
                    impact: None,
                    evidence_step_ids: vec!["main_metric".to_string()],
                    confidence: Some("高".to_string()),
                },
                AttributionDriver {
                    title: "无证据".to_string(),
                    explanation: "没有对应 observation".to_string(),
                    impact: None,
                    evidence_step_ids: vec!["missing".to_string()],
                    confidence: Some("高".to_string()),
                },
            ],
            recommendations: Vec::new(),
            caveats: Vec::new(),
            next_questions: Vec::new(),
            confidence: Some("中".to_string()),
            coverage: None,
        };
        let report = sanitize_attribution_report(report, &observations);
        assert_eq!(report.main_causes.len(), 1);
        assert_eq!(report.main_causes[0].title, "有证据");
        assert!(report
            .caveats
            .iter()
            .any(|c| c.contains("缺少成功数据证据")));
    }

    #[test]
    fn sanitize_report_clears_causes_when_no_successful_evidence() {
        let observations = vec![AttributionObservation {
            step_id: "main_metric".to_string(),
            title: "主指标".to_string(),
            purpose: "查主指标".to_string(),
            question: "查昨天收入".to_string(),
            datasource_ids: vec!["ds-1".to_string()],
            time_context: Some("昨天".to_string()),
            query_id: None,
            conversation_id: None,
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            sampled: false,
            sqls: Vec::new(),
            used_references: Vec::new(),
            error: Some("table not found".to_string()),
            elapsed_ms: 1,
        }];
        let report = AttributionReport {
            title: "归因".to_string(),
            executive_summary: "摘要".to_string(),
            metric_answer: None,
            main_causes: vec![AttributionDriver {
                title: "猜测".to_string(),
                explanation: "没有数据也猜".to_string(),
                impact: None,
                evidence_step_ids: vec!["main_metric".to_string()],
                confidence: Some("高".to_string()),
            }],
            recommendations: Vec::new(),
            caveats: Vec::new(),
            next_questions: Vec::new(),
            confidence: Some("高".to_string()),
            coverage: None,
        };
        let report = sanitize_attribution_report(report, &observations);
        assert!(report.main_causes.is_empty());
        assert_eq!(report.confidence.as_deref(), Some("低"));
        assert!(report.caveats.iter().any(|c| c.contains("没有成功执行")));
    }
}
