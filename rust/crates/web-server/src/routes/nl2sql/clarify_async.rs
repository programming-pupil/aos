use super::query_async::{emit_stage, with_stage_emitter, QueryStageSignal};
use super::{ClarifyRequest, ClarifyResponse};
use crate::auth::Claims;
use crate::error::{AppError, Result};
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
pub(crate) struct ClarifyTaskEvent {
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
pub(crate) struct StartClarifyTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClarifyTaskStatusResponse {
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
struct ClarifyTaskRecord {
    tenant_id: String,
    user_id: String,
    created_at: Instant,
    completed_at: Option<Instant>,
    last_event: ClarifyTaskEvent,
    done: bool,
}

#[derive(Debug, Clone, Copy)]
struct ClarifyTaskConfig {
    max_concurrent_running: usize,
    max_tasks_in_memory: usize,
    task_ttl: Duration,
    cleanup_interval: Duration,
}

impl ClarifyTaskConfig {
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
            max_concurrent_running: read_usize("NL2SQL_CLARIFY_TASK_MAX_CONCURRENT", 8),
            max_tasks_in_memory: read_usize("NL2SQL_CLARIFY_TASK_MAX_IN_MEMORY", 2000),
            task_ttl: Duration::from_secs(read_u64("NL2SQL_CLARIFY_TASK_TTL_SECS", 1800)),
            cleanup_interval: Duration::from_secs(read_u64(
                "NL2SQL_CLARIFY_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
        }
    }
}

#[derive(Clone)]
struct ClarifyTaskManager {
    inner: Arc<Mutex<HashMap<String, ClarifyTaskRecord>>>,
    senders: Arc<Mutex<HashMap<String, broadcast::Sender<ClarifyTaskEvent>>>>,
    run_slots: Arc<Semaphore>,
    config: ClarifyTaskConfig,
}

impl ClarifyTaskManager {
    fn new() -> Self {
        let config = ClarifyTaskConfig::from_env();
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

    async fn ensure_sender(&self, task_id: &str) -> broadcast::Sender<ClarifyTaskEvent> {
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
        let initial = ClarifyTaskEvent {
            task_id: task_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            message: Some("已加入澄清处理队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            response: None,
            error: None,
        };
        let record = ClarifyTaskRecord {
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
                    "too many nl2sql clarify tasks in memory (limit: {})",
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
            let evt = ClarifyTaskEvent {
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

    async fn publish_completed(&self, task_id: &str, response: &ClarifyResponse) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let response_json = serde_json::to_value(response).ok();
            let (status, stage, message, error) = if let Some(err) = response.error.clone() {
                (
                    "failed".to_string(),
                    "failed".to_string(),
                    "澄清处理失败".to_string(),
                    Some(err),
                )
            } else if response.pending_clarification.is_some() {
                (
                    "clarification_needed".to_string(),
                    "clarification_gate".to_string(),
                    "仍需补充信息".to_string(),
                    None,
                )
            } else {
                (
                    "completed".to_string(),
                    "done".to_string(),
                    "澄清处理完成".to_string(),
                    None,
                )
            };
            let evt = ClarifyTaskEvent {
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
            let evt = ClarifyTaskEvent {
                task_id: task_id.to_string(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("澄清处理失败".to_string()),
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
    ) -> Option<ClarifyTaskEvent> {
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
    ) -> Option<broadcast::Receiver<ClarifyTaskEvent>> {
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
                "too many concurrent nl2sql clarify tasks (limit: {})",
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

fn task_manager() -> &'static ClarifyTaskManager {
    static MANAGER: OnceLock<ClarifyTaskManager> = OnceLock::new();
    MANAGER.get_or_init(ClarifyTaskManager::new)
}

async fn find_waiting_source_agent_task_id(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    source_query_task_id: &str,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar::<sqlx::Sqlite, String>(
        "SELECT links.task_id
         FROM agent_task_resource_links links
         JOIN agent_tasks tasks
           ON tasks.tenant_id = links.tenant_id AND tasks.id = links.task_id
         WHERE links.tenant_id = ?
           AND links.resource_type = 'nl2sql_async_query'
           AND links.resource_id = ?
           AND tasks.owner_user_id = ?
           AND tasks.status = 'waiting_input'
         ORDER BY links.created_at DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(source_query_task_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?)
}

async fn complete_source_query_after_clarification(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    source_query_task_id: &str,
    clarify_task_id: &str,
    response: &ClarifyResponse,
) -> Result<()> {
    if response.error.is_some() || response.pending_clarification.is_some() {
        return Ok(());
    }
    let Some(data) = response.data.as_ref().filter(|data| {
        data.sql
            .as_deref()
            .is_some_and(|sql| !sql.trim().is_empty())
    }) else {
        return Ok(());
    };
    let task_id = find_waiting_source_agent_task_id(
        state.control_db(),
        tenant_id,
        user_id,
        source_query_task_id,
    )
    .await?;
    let Some(task_id) = task_id else {
        return Ok(());
    };

    crate::routes::agent_ops::link_task_resource(
        state,
        tenant_id,
        &task_id,
        "nl2sql_agent_query",
        &data.query_id,
    )
    .await?;
    crate::routes::agent_ops::complete_task(
        state,
        tenant_id,
        &task_id,
        "NL2SQL 澄清已完成并生成 SQL",
        Some(serde_json::json!({
            "queryTaskId": source_query_task_id,
            "clarifyTaskId": clarify_task_id,
            "queryId": data.query_id,
            "conversationId": data.conversation_id,
        })),
    )
    .await
}

pub(crate) async fn start_clarify_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ClarifyRequest>,
) -> Result<Json<StartClarifyTaskResponse>> {
    let task_id = format!("nl2sql-clarify-task-{}", uuid::Uuid::new_v4());
    let manager = task_manager().clone();
    let run_slot = manager.try_acquire_run_slot()?;
    manager
        .create_task(&task_id, &claims.tenant_id, &claims.sub)
        .await?;

    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let req_clone = req.clone();
    let source_query_task_id = req.source_query_task_id.clone();
    let notify_state = state.clone();
    let notify_tenant_id = claims.tenant_id.clone();
    let notify_user_id = claims.sub.clone();
    let task_id_clone = task_id.clone();
    tokio::spawn(async move {
        let _run_slot = run_slot;
        let manager2 = task_manager().clone();

        manager2
            .publish_stage(&task_id_clone, "request_validation", "开始校验澄清请求")
            .await;

        let stage_task_id = task_id_clone.clone();
        let stage_emitter: Arc<dyn Fn(QueryStageSignal) + Send + Sync> = Arc::new(move |signal| {
            let task_id_inner = stage_task_id.clone();
            tokio::spawn(async move {
                task_manager()
                    .publish_stage(&task_id_inner, &signal.stage, &signal.message)
                    .await;
            });
        });

        let result = with_stage_emitter(stage_emitter, async move {
            emit_stage("clarification_gate", "正在处理补充条件");
            emit_stage("query_understanding", "正在做意图澄清");
            emit_stage("cache_lookup", "正在检查缓存");
            emit_stage("generate_sql", "正在生成 SQL");
            super::routing::clarify(State(state_clone), Extension(claims_clone), Json(req_clone))
                .await
        })
        .await;

        match result {
            Ok(Json(resp)) => {
                manager2.publish_completed(&task_id_clone, &resp).await;
                if let Some(source_query_task_id) = source_query_task_id.as_deref() {
                    if let Err(error) = complete_source_query_after_clarification(
                        &notify_state,
                        &notify_tenant_id,
                        &notify_user_id,
                        source_query_task_id,
                        &task_id_clone,
                        &resp,
                    )
                    .await
                    {
                        tracing::error!(
                            tenant_id = %notify_tenant_id,
                            user_id = %notify_user_id,
                            source_query_task_id,
                            clarify_task_id = %task_id_clone,
                            error = %error,
                            "failed to complete waiting NL2SQL AgentOps task after clarification"
                        );
                    }
                }
            }
            Err(e) => {
                manager2.publish_failed(&task_id_clone, e.to_string()).await;
            }
        }
    });

    Ok(Json(StartClarifyTaskResponse {
        task_id,
        status: "queued".to_string(),
    }))
}

pub(crate) async fn get_clarify_task_status(
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<ClarifyTaskStatusResponse>> {
    let manager = task_manager();
    let snapshot = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("clarify task not found".to_string()))?;

    Ok(Json(ClarifyTaskStatusResponse {
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

pub(crate) async fn stream_clarify_task_events(
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
        .ok_or_else(|| AppError::NotFound("clarify task not found".to_string()))?;
    let mut rx = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("clarify task not found".to_string()))?;

    let stream = async_stream::stream! {
        let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        yield Ok(Event::default().event("task_event").data(snapshot_payload));

        while let Ok(evt) = rx.recv().await {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
            if evt.status == "completed"
                || evt.status == "failed"
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

    #[tokio::test]
    async fn waiting_source_task_lookup_is_tenant_user_and_status_scoped() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("create test database");
        sqlx::query(
            "CREATE TABLE agent_tasks (
                id TEXT NOT NULL,
                tenant_id TEXT NOT NULL,
                owner_user_id TEXT NOT NULL,
                status TEXT NOT NULL,
                PRIMARY KEY (tenant_id, id)
            )",
        )
        .execute(&db)
        .await
        .expect("create tasks table");
        sqlx::query(
            "CREATE TABLE agent_task_resource_links (
                tenant_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                resource_type TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&db)
        .await
        .expect("create links table");

        for (task_id, tenant_id, owner_user_id, status, resource_id) in [
            ("right", "tenant-a", "user-a", "waiting_input", "query-a"),
            (
                "other-user",
                "tenant-a",
                "user-b",
                "waiting_input",
                "query-a",
            ),
            (
                "other-tenant",
                "tenant-b",
                "user-a",
                "waiting_input",
                "query-a",
            ),
            ("completed", "tenant-a", "user-a", "completed", "query-b"),
        ] {
            sqlx::query(
                "INSERT INTO agent_tasks (id, tenant_id, owner_user_id, status)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(task_id)
            .bind(tenant_id)
            .bind(owner_user_id)
            .bind(status)
            .execute(&db)
            .await
            .expect("insert task");
            sqlx::query(
                "INSERT INTO agent_task_resource_links
                 (tenant_id, task_id, resource_type, resource_id)
                 VALUES (?, ?, 'nl2sql_async_query', ?)",
            )
            .bind(tenant_id)
            .bind(task_id)
            .bind(resource_id)
            .execute(&db)
            .await
            .expect("insert resource link");
        }

        assert_eq!(
            find_waiting_source_agent_task_id(&db, "tenant-a", "user-a", "query-a")
                .await
                .expect("lookup task")
                .as_deref(),
            Some("right")
        );
        assert!(
            find_waiting_source_agent_task_id(&db, "tenant-a", "user-a", "query-b")
                .await
                .expect("lookup completed task")
                .is_none()
        );
        assert!(
            find_waiting_source_agent_task_id(&db, "tenant-a", "unknown", "query-a")
                .await
                .expect("lookup other user")
                .is_none()
        );
    }
}
