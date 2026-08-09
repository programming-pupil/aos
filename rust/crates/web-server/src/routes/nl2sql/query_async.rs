use super::{QueryRequest, QueryResponse};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::agent_ops::{self, CreateAgentTaskInput};
use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct QueryStageSignal {
    pub stage: String,
    pub message: String,
}

type StageEmitter = Arc<dyn Fn(QueryStageSignal) + Send + Sync>;

tokio::task_local! {
    static QUERY_STAGE_EMITTER: StageEmitter;
}

pub(crate) async fn with_stage_emitter<F, T>(emitter: StageEmitter, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    QUERY_STAGE_EMITTER.scope(emitter, fut).await
}

pub(crate) fn emit_stage(stage: &str, message: &str) {
    let signal = QueryStageSignal {
        stage: stage.to_string(),
        message: message.to_string(),
    };
    if let Ok(cb) = QUERY_STAGE_EMITTER.try_with(|c| c.clone()) {
        cb(signal);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct QueryTaskEvent {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    pub response: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartQueryTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryTaskStatusResponse {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    pub response: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct QueryTaskRecord {
    tenant_id: String,
    user_id: String,
    created_at: Instant,
    completed_at: Option<Instant>,
    last_event: QueryTaskEvent,
    done: bool,
}

#[derive(Debug, Clone, Copy)]
struct QueryTaskConfig {
    max_concurrent_running: usize,
    max_tasks_in_memory: usize,
    task_ttl: Duration,
    cleanup_interval: Duration,
}

impl QueryTaskConfig {
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
            max_concurrent_running: read_usize("NL2SQL_QUERY_TASK_MAX_CONCURRENT", 8),
            max_tasks_in_memory: read_usize("NL2SQL_QUERY_TASK_MAX_IN_MEMORY", 2000),
            task_ttl: Duration::from_secs(read_u64("NL2SQL_QUERY_TASK_TTL_SECS", 1800)),
            cleanup_interval: Duration::from_secs(read_u64(
                "NL2SQL_QUERY_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
        }
    }
}

#[derive(Clone)]
struct QueryTaskManager {
    inner: Arc<Mutex<HashMap<String, QueryTaskRecord>>>,
    senders: Arc<Mutex<HashMap<String, broadcast::Sender<QueryTaskEvent>>>>,
    run_slots: Arc<Semaphore>,
    config: QueryTaskConfig,
}

impl QueryTaskManager {
    fn new() -> Self {
        let config = QueryTaskConfig::from_env();
        let manager = Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            run_slots: Arc::new(Semaphore::new(config.max_concurrent_running)),
            config,
        };
        tracing::info!(
            max_concurrent_running = config.max_concurrent_running,
            max_tasks_in_memory = config.max_tasks_in_memory,
            task_ttl_secs = config.task_ttl.as_secs(),
            cleanup_interval_secs = config.cleanup_interval.as_secs(),
            "nl2sql query task manager initialized"
        );
        manager.start_cleanup_loop();
        manager
    }

    fn start_cleanup_loop(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(manager.config.cleanup_interval).await;
                let removed = manager.cleanup_expired().await;
                if removed > 0 {
                    tracing::debug!(removed, "nl2sql query tasks cleaned up");
                }
            }
        });
    }

    async fn ensure_sender(&self, task_id: &str) -> broadcast::Sender<QueryTaskEvent> {
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
        let initial = QueryTaskEvent {
            task_id: task_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            message: Some("已加入执行队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            response: None,
            error: None,
        };
        let record = QueryTaskRecord {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            created_at: Instant::now(),
            completed_at: None,
            last_event: initial.clone(),
            done: false,
        };
        {
            let mut guard = self.inner.lock().await;
            if guard.len() >= self.config.max_tasks_in_memory {
                return Err(AppError::TooManyRequests(format!(
                    "too many nl2sql query tasks in memory (limit: {})",
                    self.config.max_tasks_in_memory
                )));
            }
            guard.insert(task_id.to_string(), record);
        }
        let tx = self.ensure_sender(task_id).await;
        let _ = tx.send(initial);
        Ok(())
    }

    async fn publish_stage(&self, task_id: &str, stage: &str, message: &str) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let evt = QueryTaskEvent {
                task_id: task_id.to_string(),
                status: "running".to_string(),
                stage: Some(stage.to_string()),
                message: Some(message.to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                response: None,
                error: None,
            };
            rec.last_event = evt.clone();
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    async fn publish_completed(&self, task_id: &str, response: &QueryResponse) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let response_json = serde_json::to_value(response).ok();
            let (status, stage, message, error) = if let Some(err) = response.error.clone() {
                (
                    "failed".to_string(),
                    "failed".to_string(),
                    "SQL 生成失败".to_string(),
                    Some(err),
                )
            } else if response.clarification_question.is_some() {
                (
                    "clarification_needed".to_string(),
                    "clarification_gate".to_string(),
                    "需要补充信息后继续".to_string(),
                    None,
                )
            } else {
                (
                    "completed".to_string(),
                    "done".to_string(),
                    "SQL 生成完成".to_string(),
                    None,
                )
            };
            let evt = QueryTaskEvent {
                task_id: task_id.to_string(),
                status,
                stage: Some(stage),
                message: Some(message),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                response: response_json,
                error,
            };
            rec.last_event = evt.clone();
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
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let evt = QueryTaskEvent {
                task_id: task_id.to_string(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("SQL 生成失败".to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                response: None,
                error: Some(error),
            };
            rec.last_event = evt.clone();
            rec.done = true;
            rec.completed_at = Some(Instant::now());
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    async fn restore_snapshot(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
        event: QueryTaskEvent,
    ) {
        let done = matches!(
            event.status.as_str(),
            "completed" | "failed" | "cancelled" | "clarification_needed"
        );
        let mut guard = self.inner.lock().await;
        guard
            .entry(task_id.to_string())
            .or_insert_with(|| QueryTaskRecord {
                tenant_id: tenant_id.to_string(),
                user_id: user_id.to_string(),
                created_at: Instant::now(),
                completed_at: done.then(Instant::now),
                last_event: event,
                done,
            });
    }

    async fn snapshot(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<QueryTaskEvent> {
        self.inner
            .lock()
            .await
            .get(task_id)
            .filter(|rec| rec.tenant_id == tenant_id && rec.user_id == user_id)
            .map(|rec| rec.last_event.clone())
    }

    async fn subscribe(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<broadcast::Receiver<QueryTaskEvent>> {
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
                "too many concurrent nl2sql query tasks (limit: {})",
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

fn agent_ops_stage_projection(stage: &str) -> (&'static str, i32) {
    match stage {
        "request_validation" => (agent_ops::PHASE_INTAKE, 8),
        "load_schema" | "load_context" | "cache_lookup" => (agent_ops::PHASE_RETRIEVING, 25),
        "query_understanding" | "clarification_gate" => (agent_ops::PHASE_PLANNING, 40),
        "generate_sql" => (agent_ops::PHASE_MODEL_CALLING, 58),
        "semantic_review" | "explain_preflight" | "policy_enforcement" => {
            (agent_ops::PHASE_VALIDATING, 78)
        }
        "persist_result" => (agent_ops::PHASE_FINALIZING, 92),
        "done" => (agent_ops::PHASE_FINALIZING, 98),
        _ => (agent_ops::PHASE_EXECUTING, 50),
    }
}

async fn load_durable_query_task_event(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    query_task_id: &str,
) -> Result<Option<QueryTaskEvent>> {
    let Some(row) = sqlx::query(
        "SELECT status, phase, last_event, error_message,
                CAST(output_json AS TEXT) AS output_json,
                CAST(((julianday(COALESCE(completed_at, CURRENT_TIMESTAMP)) - julianday(created_at)) * 86400000000) / 1000 AS INTEGER) AS elapsed_ms
         FROM agent_tasks
         WHERE tenant_id = ? AND owner_user_id = ?
           AND source = 'nl2sql_async' AND source_ref = ?
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(query_task_id)
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(None);
    };
    let agent_status: String = row.get("status");
    let output = row
        .get::<Option<String>, _>("output_json")
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
    let response = output
        .as_ref()
        .and_then(|value| value.get("queryTaskResponse"))
        .cloned();
    let status = match agent_status.as_str() {
        "completed" => "completed",
        "failed" | "timed_out" | "stale" => "failed",
        "cancelled" => "cancelled",
        "waiting_input" => "clarification_needed",
        "created" | "queued" => "queued",
        _ => "running",
    };
    Ok(Some(QueryTaskEvent {
        task_id: query_task_id.to_string(),
        status: status.to_string(),
        stage: Some(row.get("phase")),
        message: row.get("last_event"),
        elapsed_ms: row.get("elapsed_ms"),
        stage_elapsed_ms: None,
        response,
        error: row.get("error_message"),
    }))
}

fn task_manager() -> &'static QueryTaskManager {
    static MANAGER: OnceLock<QueryTaskManager> = OnceLock::new();
    MANAGER.get_or_init(QueryTaskManager::new)
}

async fn query_task_snapshot_with_restore(
    state: &AppState,
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> Result<QueryTaskEvent> {
    let manager = task_manager();
    if let Some(snapshot) = manager.snapshot(task_id, tenant_id, user_id).await {
        return Ok(snapshot);
    }
    let snapshot = load_durable_query_task_event(state, tenant_id, user_id, task_id)
        .await?
        .ok_or_else(|| AppError::NotFound("query task not found".to_string()))?;
    if matches!(snapshot.status.as_str(), "queued" | "running") {
        // Active execution ownership is reconciled by the AgentOps projector. A read
        // endpoint must not mutate task state or recreate a worker that does not exist.
        return Ok(snapshot);
    }
    manager
        .restore_snapshot(task_id, tenant_id, user_id, snapshot.clone())
        .await;
    Ok(snapshot)
}

pub(crate) async fn query_task_worker_registered(
    task_id: &str,
    tenant_id: &str,
    user_id: &str,
) -> bool {
    task_manager()
        .snapshot(task_id, tenant_id, user_id)
        .await
        .is_some()
}

fn nl2sql_notification_preview(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = normalized.chars().take(max_chars).collect::<String>();
    if normalized.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[cfg(any(feature = "agent", feature = "bot-agents", feature = "pm"))]
fn spawn_nl2sql_query_completed_notification(
    state: AppState,
    tenant_id: String,
    task_id: String,
    question: String,
    response: &QueryResponse,
) {
    if response.error.is_some() || response.clarification_question.is_some() {
        return;
    }
    let question_preview = nl2sql_notification_preview(&question, 800);
    let sql_preview = response
        .sql
        .as_deref()
        .map(|sql| nl2sql_notification_preview(sql, 2500))
        .filter(|sql| !sql.is_empty())
        .unwrap_or_else(|| "-".to_string());
    let text = format!(
        "数据探索问答已完成\n\n任务ID: {task_id}\n查询ID: {}\n\n问题:\n{question_preview}\n\n生成 SQL:\n{sql_preview}",
        response.query_id
    );
    tokio::spawn(async move {
        if !crate::routes::task_control_worker::legacy_capability_notifications_enabled(
            &state, &tenant_id,
        )
        .await
        {
            return;
        }
        match crate::routes::bot_agents_outbound::notify_capability_event(
            &state,
            &tenant_id,
            "nl2sql",
            "nl2sql.answer_completed",
            crate::routes::bot_agents_outbound::BotOutboundMessage {
                title: Some("数据探索问答完成".to_string()),
                text,
                external_conversation_id: None,
            },
        )
        .await
        {
            Ok(summary) if summary.attempted > 0 => {
                tracing::info!(
                    tenant_id = %tenant_id,
                    task_id = %task_id,
                    attempted = summary.attempted,
                    sent = summary.sent,
                    failed = summary.failed,
                    "nl2sql query completion bot notification dispatched"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    task_id = %task_id,
                    "nl2sql query completion bot notification skipped: {}",
                    error
                );
            }
        }
    });
}

#[cfg(not(any(feature = "agent", feature = "bot-agents", feature = "pm")))]
fn spawn_nl2sql_query_completed_notification(
    _state: AppState,
    _tenant_id: String,
    _task_id: String,
    _question: String,
    _response: &QueryResponse,
) {
}

pub(crate) async fn start_query_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<StartQueryTaskResponse>> {
    let task_id = format!("nl2sql-task-{}", uuid::Uuid::new_v4());
    let agent_task = agent_ops::create_task_with_outcome(
        &state,
        CreateAgentTaskInput {
            tenant_id: claims.tenant_id.clone(),
            source: "nl2sql_async".to_string(),
            source_ref: Some(task_id.clone()),
            source_label: Some("数据探索".to_string()),
            capability_key: "nl2sql".to_string(),
            agent_id: None,
            agent_name: Some("数据探索".to_string()),
            title: req.question.chars().take(80).collect(),
            summary: Some("异步生成、校验并持久化 SQL".to_string()),
            owner_user_id: Some(claims.sub.clone()),
            correlation_id: Some(task_id.clone()),
            parent_task_id: None,
            external_platform: None,
            external_channel_id: None,
            external_conversation_id: None,
            external_message_id: None,
            idempotency_key: Some(format!("nl2sql-async:{task_id}")),
            input_json: Some(serde_json::json!({
                "dataSourceId": &req.data_source_id,
                "questionChars": req.question.chars().count(),
                "conversationId": &req.conversation_id,
                "routingMethod": &req.routing_method,
                "hasSemanticContext": req.semantic_context.is_some(),
                "hasReferenceBindings": req.reference_bindings.is_some(),
            })),
        },
    )
    .await?;
    let agent_task_id = agent_task.id;
    if let Err(error) = agent_ops::link_task_resource(
        &state,
        &claims.tenant_id,
        &agent_task_id,
        "nl2sql_async_query",
        &task_id,
    )
    .await
    {
        let _ = agent_ops::fail_task(
            &state,
            &claims.tenant_id,
            &agent_task_id,
            "nl2sql_async_link_failed",
            &error.to_string(),
        )
        .await;
        return Err(error);
    }
    let manager = task_manager().clone();
    let run_slot = match manager.try_acquire_run_slot() {
        Ok(slot) => slot,
        Err(error) => {
            let _ = agent_ops::fail_task(
                &state,
                &claims.tenant_id,
                &agent_task_id,
                "nl2sql_async_queue_saturated",
                &error.to_string(),
            )
            .await;
            return Err(error);
        }
    };
    if let Err(error) = manager
        .create_task(&task_id, &claims.tenant_id, &claims.sub)
        .await
    {
        let _ = agent_ops::fail_task(
            &state,
            &claims.tenant_id,
            &agent_task_id,
            "nl2sql_async_queue_rejected",
            &error.to_string(),
        )
        .await;
        return Err(error);
    }

    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let req_clone = req.clone();
    let notify_state = state.clone();
    let notify_tenant_id = claims.tenant_id.clone();
    let notify_question = req.question.clone();
    let task_id_clone = task_id.clone();
    let agent_task_id_clone = agent_task_id.clone();
    let agent_tenant_id = claims.tenant_id.clone();
    tokio::spawn(async move {
        let _run_slot = run_slot;
        let manager2 = task_manager().clone();
        let _ = agent_ops::mark_task_running(
            &state_clone,
            &agent_tenant_id,
            &agent_task_id_clone,
            agent_ops::PHASE_INTAKE,
            "NL2SQL 异步任务开始执行",
            5,
        )
        .await;
        manager2
            .publish_stage(&task_id_clone, "request_validation", "开始校验请求")
            .await;

        let task_id_for_stage = task_id_clone.clone();
        let state_for_stage = state_clone.clone();
        let tenant_for_stage = agent_tenant_id.clone();
        let agent_task_for_stage = agent_task_id_clone.clone();
        let stage_emitter: StageEmitter = Arc::new(move |signal: QueryStageSignal| {
            let stage_task_id = task_id_for_stage.clone();
            let stage_state = state_for_stage.clone();
            let stage_tenant = tenant_for_stage.clone();
            let stage_agent_task = agent_task_for_stage.clone();
            tokio::spawn(async move {
                let manager = task_manager().clone();
                manager
                    .publish_stage(&stage_task_id, &signal.stage, &signal.message)
                    .await;
                let (phase, progress) = agent_ops_stage_projection(&signal.stage);
                if let Err(error) = agent_ops::mark_task_running(
                    &stage_state,
                    &stage_tenant,
                    &stage_agent_task,
                    phase,
                    &signal.message,
                    progress,
                )
                .await
                {
                    tracing::error!(
                        tenant_id = %stage_tenant,
                        agent_task_id = %stage_agent_task,
                        query_task_id = %stage_task_id,
                        error = %error,
                        error_debug = ?error,
                        "failed to persist NL2SQL async progress in AgentOps"
                    );
                }
            });
        });

        let result = with_stage_emitter(stage_emitter, async move {
            super::query(State(state_clone), Extension(claims_clone), Json(req_clone)).await
        })
        .await;

        match result {
            Ok(Json(resp)) => {
                manager2.publish_completed(&task_id_clone, &resp).await;
                let response_json = serde_json::to_value(&resp).unwrap_or(serde_json::Value::Null);
                let durable_output = Some(serde_json::json!({
                    "queryTaskId": &task_id_clone,
                    "queryTaskResponse": &response_json,
                }));
                if resp.clarification_question.is_some() {
                    let _ = agent_ops::mark_task_waiting_input(
                        &notify_state,
                        &agent_tenant_id,
                        &agent_task_id_clone,
                        agent_ops::PHASE_PLANNING,
                        "NL2SQL 需要用户补充信息",
                        45,
                        durable_output.clone(),
                    )
                    .await;
                    let _ = sqlx::query(
                        "UPDATE agent_tasks SET output_json = ?, updated_at = CURRENT_TIMESTAMP
                         WHERE tenant_id = ? AND id = ? AND status = 'waiting_input'",
                    )
                    .bind(durable_output.as_ref().map(serde_json::Value::to_string))
                    .bind(&agent_tenant_id)
                    .bind(&agent_task_id_clone)
                    .execute(&notify_state.db)
                    .await;
                } else if let Some(error) = resp.error.as_deref() {
                    let _ = agent_ops::fail_task(
                        &notify_state,
                        &agent_tenant_id,
                        &agent_task_id_clone,
                        "nl2sql_query_failed",
                        error,
                    )
                    .await;
                } else {
                    if let Err(error) = agent_ops::link_task_resource(
                        &notify_state,
                        &agent_tenant_id,
                        &agent_task_id_clone,
                        "nl2sql_agent_query",
                        &resp.query_id,
                    )
                    .await
                    {
                        let _ = agent_ops::fail_task(
                            &notify_state,
                            &agent_tenant_id,
                            &agent_task_id_clone,
                            "nl2sql_result_link_failed",
                            &error.to_string(),
                        )
                        .await;
                    } else {
                        let _ = agent_ops::complete_task(
                            &notify_state,
                            &agent_tenant_id,
                            &agent_task_id_clone,
                            "NL2SQL 已生成并校验 SQL",
                            durable_output,
                        )
                        .await;
                    }
                }
                match run_lifecycle_hooks(
                    &notify_state,
                    &notify_tenant_id,
                    "nl2sql",
                    HookEventType::TaskCompleted,
                    "nl2sql.query_completed",
                    serde_json::json!({
                        "taskId": &task_id_clone,
                        "question": &notify_question,
                        "queryId": &resp.query_id,
                    }),
                    Some(serde_json::to_value(&resp).unwrap_or(serde_json::Value::Null)),
                    resp.error.is_some(),
                )
                .await
                {
                    Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
                        tracing::warn!(
                            tenant_id = %notify_tenant_id,
                            task_id = %task_id_clone,
                            "nl2sql task_completed hook completed with warning: {}",
                            hook_result.messages().join("\n")
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            tenant_id = %notify_tenant_id,
                            task_id = %task_id_clone,
                            error = %error,
                            "nl2sql task_completed hook failed to execute"
                        );
                    }
                }
                spawn_nl2sql_query_completed_notification(
                    notify_state,
                    notify_tenant_id,
                    task_id_clone.clone(),
                    notify_question,
                    &resp,
                );
            }
            Err(e) => {
                let error_message = e.to_string();
                manager2
                    .publish_failed(&task_id_clone, error_message.clone())
                    .await;
                let _ = agent_ops::fail_task(
                    &notify_state,
                    &agent_tenant_id,
                    &agent_task_id_clone,
                    "nl2sql_async_execution_failed",
                    &error_message,
                )
                .await;
            }
        }
    });

    Ok(Json(StartQueryTaskResponse {
        task_id,
        status: "queued".to_string(),
    }))
}

pub(crate) async fn get_query_task_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<QueryTaskStatusResponse>> {
    let snapshot =
        query_task_snapshot_with_restore(&state, &task_id, &claims.tenant_id, &claims.sub).await?;
    Ok(Json(QueryTaskStatusResponse {
        task_id: snapshot.task_id,
        status: snapshot.status,
        stage: snapshot.stage,
        message: snapshot.message,
        elapsed_ms: snapshot.elapsed_ms,
        stage_elapsed_ms: snapshot.stage_elapsed_ms,
        response: snapshot.response,
        error: snapshot.error,
    }))
}

pub(crate) async fn stream_query_task_events(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<
    Sse<
        impl futures_util::stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>,
    >,
> {
    let manager = task_manager().clone();
    let snapshot =
        query_task_snapshot_with_restore(&state, &task_id, &claims.tenant_id, &claims.sub).await?;
    let mut rx = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("query task not found".to_string()))?;

    let stream = async_stream::stream! {
        let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        yield Ok(Event::default().event("task_event").data(snapshot_payload));
        if matches!(
            snapshot.status.as_str(),
            "completed" | "failed" | "cancelled" | "clarification_needed"
        ) {
            return;
        }

        while let Ok(evt) = rx.recv().await {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
            if evt.status == "completed"
                || evt.status == "failed"
                || evt.status == "cancelled"
                || evt.status == "clarification_needed"
            {
                break;
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nl2sql_agent_ops_stages_are_monotonic_and_meaningful() {
        let stages = [
            "request_validation",
            "load_schema",
            "query_understanding",
            "generate_sql",
            "semantic_review",
            "persist_result",
            "done",
        ];
        let projected = stages
            .iter()
            .map(|stage| agent_ops_stage_projection(stage))
            .collect::<Vec<_>>();
        assert_eq!(projected[0], (agent_ops::PHASE_INTAKE, 8));
        assert_eq!(projected[3], (agent_ops::PHASE_MODEL_CALLING, 58));
        assert_eq!(projected[4], (agent_ops::PHASE_VALIDATING, 78));
        assert!(projected.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    }

    #[tokio::test]
    async fn durable_snapshot_can_rehydrate_the_in_memory_sse_owner_guard() {
        let manager = QueryTaskManager::new();
        let event = QueryTaskEvent {
            task_id: "query-restore".to_string(),
            status: "completed".to_string(),
            stage: Some("finalizing".to_string()),
            message: Some("done".to_string()),
            elapsed_ms: 42,
            stage_elapsed_ms: None,
            response: Some(serde_json::json!({ "queryId": "sql-1" })),
            error: None,
        };
        manager
            .restore_snapshot("query-restore", "tenant-a", "user-a", event.clone())
            .await;
        assert_eq!(
            manager
                .snapshot("query-restore", "tenant-a", "user-a")
                .await
                .map(|snapshot| snapshot.status),
            Some("completed".to_string())
        );
        assert!(manager
            .snapshot("query-restore", "tenant-a", "user-b")
            .await
            .is_none());
    }
}
