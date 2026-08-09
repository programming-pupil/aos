use super::agent::agent_execute;
use super::{AgentExecuteRequest, AgentExecuteResponse};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct AgentStageSignal {
    pub stage: String,
    pub message: String,
}

type StageEmitter = Arc<dyn Fn(AgentStageSignal) + Send + Sync>;

tokio::task_local! {
    static AGENT_STAGE_EMITTER: StageEmitter;
}

pub(crate) async fn with_agent_stage_emitter<F, T>(emitter: StageEmitter, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    AGENT_STAGE_EMITTER.scope(emitter, fut).await
}

pub(crate) fn emit_agent_stage(stage: &str, message: &str) {
    let signal = AgentStageSignal {
        stage: stage.to_string(),
        message: message.to_string(),
    };
    if let Ok(cb) = AGENT_STAGE_EMITTER.try_with(|c| c.clone()) {
        cb(signal);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct AgentTaskEvent {
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
pub(crate) struct StartAgentTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentTaskStatusResponse {
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
struct AgentTaskRecord {
    tenant_id: String,
    user_id: String,
    created_at: Instant,
    completed_at: Option<Instant>,
    last_event: AgentTaskEvent,
    done: bool,
}

#[derive(Debug, Clone, Copy)]
struct AgentTaskConfig {
    max_concurrent_running: usize,
    max_tasks_in_memory: usize,
    task_ttl: Duration,
    cleanup_interval: Duration,
}

impl AgentTaskConfig {
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
            max_concurrent_running: read_usize("NL2SQL_AGENT_TASK_MAX_CONCURRENT", 8),
            max_tasks_in_memory: read_usize("NL2SQL_AGENT_TASK_MAX_IN_MEMORY", 2000),
            task_ttl: Duration::from_secs(read_u64("NL2SQL_AGENT_TASK_TTL_SECS", 1800)),
            cleanup_interval: Duration::from_secs(read_u64(
                "NL2SQL_AGENT_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
        }
    }
}

#[derive(Clone)]
struct AgentTaskManager {
    inner: Arc<Mutex<HashMap<String, AgentTaskRecord>>>,
    senders: Arc<Mutex<HashMap<String, broadcast::Sender<AgentTaskEvent>>>>,
    run_slots: Arc<Semaphore>,
    config: AgentTaskConfig,
}

impl AgentTaskManager {
    fn new() -> Self {
        let config = AgentTaskConfig::from_env();
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

    async fn ensure_sender(&self, task_id: &str) -> broadcast::Sender<AgentTaskEvent> {
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
        let initial = AgentTaskEvent {
            task_id: task_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            message: Some("已加入多数据源执行队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            response: None,
            error: None,
        };
        let record = AgentTaskRecord {
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
                    "too many nl2sql agent tasks in memory (limit: {})",
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
            let evt = AgentTaskEvent {
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

    async fn publish_completed(&self, task_id: &str, response: &AgentExecuteResponse) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let response_json = serde_json::to_value(response).ok();
            let (status, stage, message, error) = if let Some(err) = response.error.clone() {
                (
                    "failed".to_string(),
                    "failed".to_string(),
                    "多数据源执行失败".to_string(),
                    Some(err),
                )
            } else {
                (
                    "completed".to_string(),
                    "done".to_string(),
                    "多数据源执行完成".to_string(),
                    None,
                )
            };

            let evt = AgentTaskEvent {
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
            let evt = AgentTaskEvent {
                task_id: task_id.to_string(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("多数据源执行失败".to_string()),
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

    async fn snapshot(
        &self,
        task_id: &str,
        tenant_id: &str,
        user_id: &str,
    ) -> Option<AgentTaskEvent> {
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
    ) -> Option<broadcast::Receiver<AgentTaskEvent>> {
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
                "too many concurrent nl2sql agent tasks (limit: {})",
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

fn task_manager() -> &'static AgentTaskManager {
    static MANAGER: OnceLock<AgentTaskManager> = OnceLock::new();
    MANAGER.get_or_init(AgentTaskManager::new)
}

pub(crate) async fn start_agent_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AgentExecuteRequest>,
) -> Result<Json<StartAgentTaskResponse>> {
    let task_id = format!("nl2sql-agent-task-{}", uuid::Uuid::new_v4());
    let manager = task_manager().clone();
    let run_slot = manager.try_acquire_run_slot()?;
    manager
        .create_task(&task_id, &claims.tenant_id, &claims.sub)
        .await?;

    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let hook_state = state.clone();
    let hook_tenant_id = claims.tenant_id.clone();
    let task_id_clone = task_id.clone();
    tokio::spawn(async move {
        let _run_slot = run_slot;
        let manager2 = task_manager().clone();

        manager2
            .publish_stage(&task_id_clone, "request_validation", "开始校验请求")
            .await;

        let task_id_for_stage = task_id_clone.clone();
        let stage_emitter: StageEmitter = Arc::new(move |signal: AgentStageSignal| {
            let stage_task_id = task_id_for_stage.clone();
            tokio::spawn(async move {
                let manager = task_manager().clone();
                manager
                    .publish_stage(&stage_task_id, &signal.stage, &signal.message)
                    .await;
            });
        });

        let result = with_agent_stage_emitter(stage_emitter, async move {
            agent_execute(State(state_clone), Extension(claims_clone), Json(req)).await
        })
        .await;

        match result {
            Ok(Json(resp)) => {
                manager2.publish_completed(&task_id_clone, &resp).await;
                match run_lifecycle_hooks(
                    &hook_state,
                    &hook_tenant_id,
                    "nl2sql",
                    HookEventType::TaskCompleted,
                    "nl2sql.agent_completed",
                    serde_json::json!({
                        "taskId": &task_id_clone,
                    }),
                    Some(serde_json::to_value(&resp).unwrap_or(serde_json::Value::Null)),
                    false,
                )
                .await
                {
                    Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
                        tracing::warn!(
                            tenant_id = %hook_tenant_id,
                            task_id = %task_id_clone,
                            "nl2sql agent task_completed hook completed with warning: {}",
                            hook_result.messages().join("\n")
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            tenant_id = %hook_tenant_id,
                            task_id = %task_id_clone,
                            error = %error,
                            "nl2sql agent task_completed hook failed to execute"
                        );
                    }
                }
            }
            Err(e) => {
                manager2.publish_failed(&task_id_clone, e.to_string()).await;
            }
        }
    });

    Ok(Json(StartAgentTaskResponse {
        task_id,
        status: "queued".to_string(),
    }))
}

pub(crate) async fn get_agent_task_status(
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<AgentTaskStatusResponse>> {
    let manager = task_manager();
    let snapshot = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("agent task not found".to_string()))?;

    Ok(Json(AgentTaskStatusResponse {
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

pub(crate) async fn stream_agent_task_events(
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<
    Sse<
        impl futures_util::stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>,
    >,
> {
    let manager = task_manager().clone();
    let snapshot = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("agent task not found".to_string()))?;
    let mut rx = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("agent task not found".to_string()))?;

    let stream = async_stream::stream! {
        let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        yield Ok(Event::default().event("task_event").data(snapshot_payload));

        while let Ok(evt) = rx.recv().await {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
            if evt.status == "completed" || evt.status == "failed" {
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
