use super::{
    augment_question_for_metric_hint, build_requirement_clarification_question,
    enforce_metric_hard_constraint_sql, extract_columns_from_sql, extract_schema_tables_and_fks,
    generate_sql, is_safe_sql, load_conversation_history, load_join_paths_for_datasource,
    load_manual_foreign_keys, map_update_err, matched_metric_names, now_ms, parse_metric_aliases,
    parse_requirements_from_question, require_admin, resolve_metric_hard_constraint,
    should_enable_domain_routing, should_enable_qu, validate_data_source_access, ClarifyRequest,
    EmbeddingConfigResponse, ForeignKeyPrompt, InputContentBlock, InputMessage, MessageRequest,
    MetricMatchCandidate, OutputContentBlock, RefreshSemanticsResponse, RefreshTaskStatusResponse,
    ReindexResponse, SuggestRequest, SuggestResponse, UpdateSemanticsResponse,
    UpdateTableSemanticsRequest,
};
use crate::auth::Claims;
use crate::error::{AppError, Result};
use crate::routes::hooks::{run_lifecycle_hooks, HookEventType};
use crate::routes::nl2sql::{ClarifyResponse, ClarifyResponseData};
use crate::state::AppState;
use axum::extract::{Extension, Json, Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, OwnedSemaphorePermit, Semaphore};

fn aggregate_embedding_usage(usages: &[api::Usage]) -> Option<api::Usage> {
    if usages.is_empty() {
        return None;
    }
    let mut agg = api::Usage::default();
    for usage in usages {
        agg.input_tokens = agg.input_tokens.saturating_add(usage.input_tokens);
        agg.output_tokens = agg.output_tokens.saturating_add(usage.output_tokens);
        agg.cache_creation_input_tokens = agg
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
        agg.cache_read_input_tokens = agg
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }
    Some(agg)
}

async fn persist_embedding_usage(
    usage_writer: Option<Arc<crate::routes::chat::TokenUsageWriter>>,
    tenant_id: &str,
    user_id: &str,
    datasource_id: &str,
    request_id: Option<&str>,
    model: &str,
    api_key_id: Option<String>,
    usage: Option<api::Usage>,
) {
    let (Some(writer), Some(usage)) = (usage_writer, usage) else {
        return;
    };
    let total_tokens = usage.total_tokens();
    if total_tokens == 0 {
        return;
    }
    let record = crate::routes::chat::TokenUsageRecord {
        tenant_id: tenant_id.to_string(),
        user_id: user_id.to_string(),
        session_id: format!("nl2sql:semantics:{datasource_id}"),
        request_id: request_id.map(std::string::ToString::to_string),
        model: model.to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        total_tokens,
        estimated_cost_usd: usage.estimated_cost_usd(model).total_cost_usd(),
        api_key_id,
        provider: "nl2sql_embedding".to_string(),
        created_at: chrono::Utc::now(),
    };
    if let Err(e) = writer.write(&record).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            user_id = %user_id,
            datasource_id = %datasource_id,
            error = %e,
            "failed to persist NL2SQL embedding token usage"
        );
    }
}

fn is_strict_mode(mode: &str) -> bool {
    mode.eq_ignore_ascii_case("strict")
}

fn normalize_domain_match_text(input: &str) -> String {
    nl2sql_domain::text::normalize_domain_match_text(input)
}

fn question_mentions_domain(question: &str, domain_name: &str) -> bool {
    nl2sql_domain::text::question_mentions_domain(question, domain_name)
}

fn strict_allowlist_for_question(
    domains: &[crate::nl2sql::routing::BusinessDomain],
    question: &str,
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut map: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for domain in domains {
        if !is_strict_mode(&domain.routing_mode) {
            continue;
        }
        let Some(ds_id) = domain.datasource_id.as_ref() else {
            continue;
        };
        if domain.domain_name.trim().is_empty() {
            continue;
        }
        if !question_mentions_domain(question, &domain.domain_name) {
            continue;
        }
        let entry = map.entry(ds_id.clone()).or_default();
        for table in &domain.tables {
            entry.insert(table.clone());
        }
    }
    map
}

fn route_result_from_explicit_business_domains(
    domains: &[crate::nl2sql::routing::BusinessDomain],
    question: &str,
    accessible_sources: &[(String, String, String)],
    requested_datasource_id: Option<&str>,
) -> Option<RouteResult> {
    let accessible_ids: std::collections::HashSet<&str> = accessible_sources
        .iter()
        .map(|(id, _, _)| id.as_str())
        .collect();
    let requested_datasource_id = requested_datasource_id.filter(|id| !id.trim().is_empty());
    let mut matches: std::collections::BTreeMap<
        String,
        (
            std::collections::BTreeSet<String>,
            std::collections::BTreeSet<String>,
            f32,
        ),
    > = std::collections::BTreeMap::new();

    for domain in domains {
        let Some(datasource_id) = domain.datasource_id.as_deref() else {
            continue;
        };
        if !accessible_ids.contains(datasource_id)
            || requested_datasource_id.is_some_and(|requested| requested != datasource_id)
            || domain.tables.is_empty()
            || !question_mentions_domain(question, &domain.domain_name)
        {
            continue;
        }
        let entry = matches.entry(datasource_id.to_string()).or_insert_with(|| {
            (
                std::collections::BTreeSet::new(),
                std::collections::BTreeSet::new(),
                0.0,
            )
        });
        entry.0.insert(domain.domain_name.clone());
        entry.1.extend(domain.tables.iter().cloned());
        entry.2 = entry.2.max(domain.confidence_score);
    }

    if matches.len() != 1 {
        return None;
    }
    let (datasource_id, (domain_names, tables, confidence)) = matches.into_iter().next()?;
    let domain_label = domain_names.into_iter().collect::<Vec<_>>().join(", ");
    let confidence = confidence.clamp(0.90, 0.99);
    Some(RouteResult {
        data_source_id: datasource_id.clone(),
        confidence,
        method: "business_domain".to_string(),
        matched_tables: tables
            .into_iter()
            .map(|table_name| MatchedTableInfo {
                data_source_id: datasource_id.clone(),
                table_name,
                best_column: String::new(),
                column_description: format!(
                    "explicitly matched configured business domain: {domain_label}"
                ),
                similarity_score: confidence,
            })
            .collect(),
        clarification_question: None,
    })
}

#[derive(Debug, Clone)]
struct SqlKnowledgeRouteCandidate {
    data_source_id: String,
    data_source_name: String,
    confidence: f32,
    score: f64,
    snippet_count: usize,
    schema_table_count: usize,
    filename: String,
    line_span: String,
    reason: String,
}

fn sql_knowledge_route_min_score() -> f64 {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_ROUTE_MIN_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(0.75)
}

fn sql_knowledge_route_strong_score() -> f64 {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_ROUTE_STRONG_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(2.5)
}

fn sql_knowledge_route_confidence(
    score: f64,
    snippet_count: usize,
    schema_table_count: usize,
) -> f32 {
    let mut confidence = (score / 5.5).clamp(0.50, 0.94) as f32;
    if snippet_count >= 2 {
        confidence = (confidence + 0.05).min(0.96);
    }
    if schema_table_count == 0 {
        confidence = (confidence + 0.08).min(0.97);
    }
    confidence
}

fn route_result_from_sql_knowledge(candidate: &SqlKnowledgeRouteCandidate) -> RouteResult {
    RouteResult {
        data_source_id: candidate.data_source_id.clone(),
        confidence: candidate.confidence,
        method: "sql_knowledge".to_string(),
        matched_tables: vec![MatchedTableInfo {
            data_source_id: candidate.data_source_id.clone(),
            table_name: format!("SQL知识库: {}", candidate.filename),
            best_column: candidate.line_span.clone(),
            column_description: format!(
                "{}；命中 {} 个知识片段；数据源：{}",
                candidate.reason, candidate.snippet_count, candidate.data_source_name
            ),
            similarity_score: candidate.confidence,
        }],
        clarification_question: None,
    }
}

fn should_prefer_sql_knowledge_route(
    candidate: &SqlKnowledgeRouteCandidate,
    current: Option<&RouteResult>,
) -> bool {
    if candidate.score < sql_knowledge_route_min_score() {
        return false;
    }
    let Some(current) = current else {
        return true;
    };
    if current.data_source_id == candidate.data_source_id {
        return false;
    }
    if candidate.schema_table_count == 0 && current.confidence < 0.95 {
        return true;
    }
    if current.confidence < 0.50 {
        return true;
    }
    candidate.score >= sql_knowledge_route_strong_score()
        && candidate.confidence >= current.confidence + 0.12
}

fn schema_table_count_from_info(schema_info: Option<serde_json::Value>) -> usize {
    let Some(schema_info) = schema_info else {
        return 0;
    };
    let (tables, _) = extract_schema_tables_and_fks(&schema_info);
    tables.as_array().map(|arr| arr.len()).unwrap_or(0)
}

async fn datasource_has_direct_sql_knowledge(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 \
         FROM nl2sql_reference_packs p \
         JOIN nl2sql_reference_files f ON f.tenant_id = p.tenant_id AND f.pack_id = p.id \
         WHERE p.tenant_id = ? AND p.enabled = 1 AND f.status = 'indexed' \
           AND (p.datasource_id = ? OR EXISTS (SELECT 1 FROM json_each(p.datasource_bindings_json) WHERE json_each.value = ?)) \
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .bind(datasource_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .is_some()
}

async fn best_sql_knowledge_route_candidate(
    state: &AppState,
    tenant_id: &str,
    question: &str,
    sources: &[(String, String, String)],
    schema_table_counts: &std::collections::HashMap<String, usize>,
) -> Option<SqlKnowledgeRouteCandidate> {
    let max_sources = std::env::var("NL2SQL_SQL_KNOWLEDGE_ROUTE_MAX_SOURCES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(48);
    let mut best: Option<SqlKnowledgeRouteCandidate> = None;
    for (datasource_id, name, _) in sources.iter().take(max_sources) {
        if !datasource_has_direct_sql_knowledge(&state.db, tenant_id, datasource_id).await {
            continue;
        }
        let snippets = match super::reference::resolve_auto_query_references(
            state,
            tenant_id,
            datasource_id,
            question,
            3,
        )
        .await
        {
            Ok(snippets) => snippets,
            Err(e) => {
                tracing::warn!(
                    tenant_id,
                    datasource_id,
                    error = %e,
                    "route: SQL knowledge lookup failed for datasource"
                );
                continue;
            }
        };
        let Some(top) = snippets
            .iter()
            .filter(|snippet| !snippet.stale)
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            continue;
        };
        let schema_table_count = *schema_table_counts.get(datasource_id).unwrap_or(&0);
        let candidate = SqlKnowledgeRouteCandidate {
            data_source_id: datasource_id.clone(),
            data_source_name: name.clone(),
            confidence: sql_knowledge_route_confidence(
                top.score,
                snippets.len(),
                schema_table_count,
            ),
            score: top.score,
            snippet_count: snippets.len(),
            schema_table_count,
            filename: top.filename.clone(),
            line_span: format!("{}-{}", top.start_line, top.end_line),
            reason: top.reason.clone(),
        };
        if best
            .as_ref()
            .map(|current| candidate.score > current.score)
            .unwrap_or(true)
        {
            best = Some(candidate);
        }
    }
    best
}

async fn persist_clarification_message(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    user_id: &str,
    conversation_id: &str,
    session_id: &str,
    turn: u32,
    original_question: &str,
    clarification_question: &str,
    user_input: &str,
    confirmed_requirements: &[String],
    missing_requirements: &[String],
) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let confirmed = serde_json::to_value(confirmed_requirements)
        .map_err(|e| AppError::Internal(format!("serialize confirmed requirements failed: {e}")))?;
    let missing = serde_json::to_value(missing_requirements)
        .map_err(|e| AppError::Internal(format!("serialize missing requirements failed: {e}")))?;
    sqlx::query(
        "INSERT INTO nl2sql_clarification_messages \
         (id, tenant_id, user_id, conversation_id, session_id, turn, original_question, clarification_question, user_input, confirmed_requirements, missing_requirements) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(conversation_id)
    .bind(session_id)
    .bind(turn)
    .bind(original_question)
    .bind(clarification_question)
    .bind(user_input)
    .bind(confirmed)
    .bind(missing)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RouteTaskEvent {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    pub response: Option<RouteResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartRouteTaskResponse {
    pub task_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteTaskStatusResponse {
    pub task_id: String,
    pub status: String,
    pub stage: Option<String>,
    pub message: Option<String>,
    pub elapsed_ms: u64,
    pub stage_elapsed_ms: Option<u64>,
    pub response: Option<RouteResponse>,
    pub error: Option<String>,
}

type RouteStageEmitter = Arc<dyn Fn(crate::nl2sql::routing::RouteStageSignal) + Send + Sync>;

tokio::task_local! {
    static ROUTE_STAGE_EMITTER: RouteStageEmitter;
}

pub(crate) async fn with_route_stage_emitter<F, T>(emitter: RouteStageEmitter, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    ROUTE_STAGE_EMITTER.scope(emitter, fut).await
}

pub(crate) fn emit_route_stage(
    stage: &str,
    message: &str,
    matched_tables: Option<Vec<crate::nl2sql::MatchedTable>>,
    route_confidence: Option<f32>,
    routing_method: Option<String>,
) {
    crate::nl2sql::routing::emit_route_stage(
        stage,
        message,
        matched_tables,
        route_confidence,
        routing_method,
    );
}

#[derive(Debug, Clone)]
struct RouteTaskRecord {
    tenant_id: String,
    user_id: String,
    created_at: Instant,
    completed_at: Option<Instant>,
    last_event: RouteTaskEvent,
    done: bool,
}

#[derive(Debug, Clone, Copy)]
struct RouteTaskConfig {
    max_concurrent_running: usize,
    max_tasks_in_memory: usize,
    task_ttl: Duration,
    cleanup_interval: Duration,
    task_hard_timeout: Duration,
}

impl RouteTaskConfig {
    fn from_env() -> Self {
        fn read_usize(name: &str, default: usize) -> usize {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }
        fn read_u64(name: &str, default: u64) -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(default)
        }
        Self {
            max_concurrent_running: read_usize("NL2SQL_ROUTE_TASK_MAX_CONCURRENT", 16),
            max_tasks_in_memory: read_usize("NL2SQL_ROUTE_TASK_MAX_IN_MEMORY", 2000),
            task_ttl: Duration::from_secs(read_u64("NL2SQL_ROUTE_TASK_TTL_SECS", 1800)),
            cleanup_interval: Duration::from_secs(read_u64(
                "NL2SQL_ROUTE_TASK_CLEANUP_INTERVAL_SECS",
                60,
            )),
            task_hard_timeout: Duration::from_secs(read_u64(
                "NL2SQL_ROUTE_TASK_HARD_TIMEOUT_SECS",
                420,
            )),
        }
    }
}

#[derive(Clone)]
struct RouteTaskManager {
    inner: Arc<Mutex<std::collections::HashMap<String, RouteTaskRecord>>>,
    senders: Arc<Mutex<std::collections::HashMap<String, broadcast::Sender<RouteTaskEvent>>>>,
    run_slots: Arc<Semaphore>,
    config: RouteTaskConfig,
}

fn route_task_terminal_fields(
    response: &RouteResponse,
) -> (&'static str, &'static str, &'static str) {
    let needs_clarification = response.result.as_ref().is_some_and(|result| {
        result.method == "clarification" && result.clarification_question.is_some()
    });
    if response.routed {
        ("completed", "done", "路由完成")
    } else if needs_clarification {
        (
            "clarification_needed",
            "clarification_needed",
            "需要补充路由信息",
        )
    } else {
        ("failed", "failed", "未找到匹配的数据表")
    }
}

impl RouteTaskManager {
    fn new() -> Self {
        let config = RouteTaskConfig::from_env();
        let manager = Self {
            inner: Arc::new(Mutex::new(std::collections::HashMap::new())),
            senders: Arc::new(Mutex::new(std::collections::HashMap::new())),
            run_slots: Arc::new(Semaphore::new(config.max_concurrent_running)),
            config,
        };
        tracing::info!(
            max_concurrent_running = config.max_concurrent_running,
            max_tasks_in_memory = config.max_tasks_in_memory,
            task_ttl_secs = config.task_ttl.as_secs(),
            cleanup_interval_secs = config.cleanup_interval.as_secs(),
            task_hard_timeout_secs = config.task_hard_timeout.as_secs(),
            "nl2sql route task manager initialized"
        );
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

    async fn ensure_sender(&self, task_id: &str) -> broadcast::Sender<RouteTaskEvent> {
        let mut map = self.senders.lock().await;
        if let Some(sender) = map.get(task_id) {
            return sender.clone();
        }
        let (tx, _) = broadcast::channel(256);
        map.insert(task_id.to_string(), tx.clone());
        tx
    }

    async fn create_task(&self, task_id: &str, tenant_id: &str, user_id: &str) -> Result<()> {
        self.cleanup_expired().await;
        let initial = RouteTaskEvent {
            task_id: task_id.to_string(),
            status: "queued".to_string(),
            stage: Some("queued".to_string()),
            message: Some("已加入路由队列".to_string()),
            elapsed_ms: 0,
            stage_elapsed_ms: Some(0),
            response: None,
            error: None,
        };
        let record = RouteTaskRecord {
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
                    "too many nl2sql route tasks in memory (limit: {})",
                    self.config.max_tasks_in_memory
                )));
            }
            guard.insert(task_id.to_string(), record);
        }
        let tx = self.ensure_sender(task_id).await;
        let _ = tx.send(initial);
        Ok(())
    }

    async fn publish_stage(
        &self,
        task_id: &str,
        stage: &str,
        message: &str,
        response: Option<&RouteResponse>,
    ) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let evt = RouteTaskEvent {
                task_id: task_id.to_string(),
                status: "running".to_string(),
                stage: Some(stage.to_string()),
                message: Some(message.to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                response: response.cloned(),
                error: None,
            };
            rec.last_event = evt.clone();
            drop(guard);
            let tx = self.ensure_sender(task_id).await;
            let _ = tx.send(evt);
        }
    }

    async fn publish_completed(&self, task_id: &str, response: &RouteResponse) {
        let mut guard = self.inner.lock().await;
        if let Some(rec) = guard.get_mut(task_id) {
            let now_elapsed = rec.created_at.elapsed().as_millis() as u64;
            let stage_elapsed = now_elapsed.saturating_sub(rec.last_event.elapsed_ms);
            let (status, stage, message) = route_task_terminal_fields(response);
            let evt = RouteTaskEvent {
                task_id: task_id.to_string(),
                status: status.to_string(),
                stage: Some(stage.to_string()),
                message: Some(message.to_string()),
                elapsed_ms: now_elapsed,
                stage_elapsed_ms: Some(stage_elapsed),
                response: Some(response.clone()),
                error: response.error.clone(),
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
            let evt = RouteTaskEvent {
                task_id: task_id.to_string(),
                status: "failed".to_string(),
                stage: Some("failed".to_string()),
                message: Some("路由失败".to_string()),
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
    ) -> Option<RouteTaskEvent> {
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
    ) -> Option<broadcast::Receiver<RouteTaskEvent>> {
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
                "too many concurrent nl2sql route tasks (limit: {})",
                self.config.max_concurrent_running
            ))
        })
    }

    async fn cleanup_expired(&self) -> usize {
        let now = Instant::now();
        let ttl = self.config.task_ttl;
        let mut expired_ids = Vec::new();
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

fn route_task_manager() -> &'static RouteTaskManager {
    static MANAGER: OnceLock<RouteTaskManager> = OnceLock::new();
    MANAGER.get_or_init(RouteTaskManager::new)
}

/// GET /api/v1/nl2sql/suggest — AI-based data source suggestion from natural language.
pub(crate) async fn suggest(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SuggestRequest>,
) -> Result<Json<SuggestResponse>> {
    // Fetch all accessible data sources for this tenant
    let rows = sqlx::query(
        "SELECT id, name, description, db_type, schema_info FROM data_sources \
         WHERE tenant_id = ? AND (user_id = ? OR user_id IS NULL) AND enabled = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Err(AppError::NotFound(
            "No accessible data sources found".into(),
        ));
    }

    // Build a summary of all schemas for the LLM to pick the best source
    #[derive(serde::Serialize)]
    struct SourceSummary<'a> {
        id: &'a str,
        name: &'a str,
        description: Option<&'a str>,
        db_type: &'a str,
        tables: Vec<TableSummary>,
    }
    #[derive(serde::Serialize)]
    struct TableSummary {
        name: String,
        columns: Vec<String>,
    }

    let sources: Vec<SourceSummary> = rows
        .iter()
        .map(|row| {
            // schema_info is nullable for datasource records that rely on SQL Knowledge only.
            let schema: serde_json::Value = row
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or_else(super::empty_schema_info);
            let tables: Vec<TableSummary> = if let Some(arr) = schema.as_array() {
                arr.iter()
                    .filter_map(|v: &serde_json::Value| {
                        let tables_arr = v.get("tables")?.as_array()?;
                        Some(TableSummary {
                            name: v.get("table_name")?.as_str()?.to_string(),
                            columns: tables_arr
                                .iter()
                                .filter_map(|t: &serde_json::Value| {
                                    t.get("columns")?.as_array().map(|cs| {
                                        cs.iter()
                                            .filter_map(|c: &serde_json::Value| {
                                                c.get("name")?.as_str().map(|s| s.to_string())
                                            })
                                            .collect()
                                    })
                                })
                                .next()
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            SourceSummary {
                id: row.get("id"),
                name: row.get("name"),
                description: row.get("description"),
                db_type: row.get("db_type"),
                tables,
            }
        })
        .collect();

    let sources_json =
        serde_json::to_string_pretty(&sources).map_err(|e| AppError::Internal(e.to_string()))?;

    #[derive(serde::Serialize)]
    struct SuggestPrompt<'a> {
        question: &'a str,
        instruction: &'a str,
        sources: &'a str,
    }
    let _question_json =
        serde_json::to_string(&req.question).map_err(|e| AppError::Internal(e.to_string()))?;

    let prompt = serde_json::to_string(&SuggestPrompt {
        question: req.question.as_str(),
        instruction: "Choose the ONE most relevant data source (by id) for answering the question. Return JSON with: data_source_id (string or null), confidence (0.0-1.0), and reason (brief sentence).",
        sources: &sources_json,
    })
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to create LLM client: {e}")))?;

    let model = chat_cfg.model.clone();
    let client = chat_cfg.client;

    let request = MessageRequest {
        model: model.clone(),
        max_tokens: 256,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt }],
        }],
        system: Some(
            "You are a data source routing assistant. Given a user question and available \
             data sources in JSON format, choose the ONE most relevant data source (by id) for \
             answering the question. Return only valid JSON with: data_source_id (string or null), \
             confidence (0.0-1.0), and reason (brief sentence). No markdown, no explanation."
                .to_string(),
        ),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.1),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = client
        .send_message(&request)
        .await
        .map_err(|e| AppError::Internal(format!("LLM call failed: {e}")))?;

    let text = response
        .content
        .iter()
        .find_map(|block| match block {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let parsed: SuggestResponse = serde_json::from_str(text.trim().trim_start_matches('`').trim())
        .map_err(|_| AppError::Internal("Failed to parse LLM suggest response".into()))?;

    Ok(Json(parsed))
}

// ── SQL & Result Explanation ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExplainRequest {
    pub query_id: String,
    pub data_source_id: Option<String>,
    /// Optional SQL override if the caller wants to explain a different query.
    pub sql: Option<String>,
    /// Language for the explanation output. Defaults to "en-US".
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "en-US".to_string()
}

#[derive(Debug, Deserialize)]
pub(crate) struct QueryResultPageQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueryResultPageResponse {
    pub query_id: String,
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
    pub page: u32,
    pub per_page: u32,
    pub total_rows: usize,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplainResponse {
    pub explanation: String,
    pub summary: String,
    /// Key insights extracted from the data.
    pub insights: Vec<String>,
    /// Concrete next actions or follow-up analyses suggested by the model.
    pub actions: Vec<String>,
    /// Caveats, data-quality warnings, or limits of the current result.
    pub risks: Vec<String>,
    /// Suggested chart type or visualization approach for this result.
    pub chart_recommendation: Option<String>,
    /// Column-level observations.
    pub column_notes: Vec<ColumnNote>,
}

/// GET /api/v1/nl2sql/results/{query_id} — restore a cached single-source result page.
pub(crate) async fn get_query_result_page(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(query_id): Path<String>,
    Query(query): Query<QueryResultPageQuery>,
) -> Result<Json<QueryResultPageResponse>> {
    const DEFAULT_PAGE_SIZE: u32 = 10;
    const MAX_PAGE_SIZE: u32 = 200;

    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT data_source_id, generated_sql \
         FROM nl2sql_queries \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND deleted_at IS NULL \
         LIMIT 1",
    )
    .bind(&query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?;

    let Some((data_source_id, generated_sql)) = row else {
        return Err(AppError::NotFound("query not found".to_string()));
    };

    if let Some(ds_id) = data_source_id.as_deref() {
        validate_data_source_access(&state, &claims.tenant_id, &claims.sub, &claims.role, ds_id)
            .await?;
    }

    let cache_result =
        crate::nl2sql::result_cache::lookup_by_query_id(&state.db, &claims.tenant_id, &query_id)
            .await;
    let rows = match cache_result {
        crate::nl2sql::result_cache::CacheLookupResult::Hit(hit) => hit
            .result_snapshot
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default(),
        crate::nl2sql::result_cache::CacheLookupResult::Expired
        | crate::nl2sql::result_cache::CacheLookupResult::NotFound => {
            return Err(AppError::NotFound("result snapshot not found".to_string()));
        }
    };

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query
        .per_page
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let start = usize::try_from((page - 1) * per_page).unwrap_or(0);
    let page_size = usize::try_from(per_page).unwrap_or(DEFAULT_PAGE_SIZE as usize);
    let end = start.saturating_add(page_size);
    let page_rows = if start >= rows.len() {
        Vec::new()
    } else {
        rows[start..rows.len().min(end)].to_vec()
    };

    let columns = page_rows
        .first()
        .or_else(|| rows.first())
        .and_then(serde_json::Value::as_object)
        .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| {
            extract_columns_from_sql(generated_sql.as_deref().unwrap_or_default())
                .into_iter()
                .collect()
        });
    let total_rows = rows.len();
    let has_more = end < total_rows;

    Ok(Json(QueryResultPageResponse {
        query_id,
        columns,
        rows: page_rows,
        page,
        per_page,
        total_rows,
        has_more,
    }))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ColumnNote {
    pub column: String,
    pub observation: String,
}

#[derive(Debug, Clone)]
struct DataExplainResult {
    explanation: String,
    summary: String,
    insights: Vec<String>,
    actions: Vec<String>,
    risks: Vec<String>,
    chart_recommendation: Option<String>,
    column_notes: Vec<ColumnNote>,
}

/// POST /api/v1/nl2sql/explain — interpret SQL query results in natural language.
pub(crate) async fn explain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ExplainRequest>,
) -> Result<Json<ExplainResponse>> {
    // ── Step 1: fetch query record from nl2sql_queries ───────────────────────
    let (question, generated_sql, stored_data_source_id, rows_returned): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = sqlx::query_as(
        "SELECT question, generated_sql, data_source_id, CAST(rows_returned AS INTEGER) \
         FROM nl2sql_queries \
         WHERE id = ? AND tenant_id = ? AND user_id = ? AND deleted_at IS NULL",
    )
    .bind(&req.query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_optional(&state.db)
    .await?
    .map(|row: (Option<String>, Option<String>, Option<String>, Option<i64>)| row)
    .unwrap_or((None, None, None, None));

    let effective_data_source_id = req.data_source_id.or(stored_data_source_id);
    if let Some(ds_id) = effective_data_source_id.as_deref() {
        validate_data_source_access(&state, &claims.tenant_id, &claims.sub, &claims.role, ds_id)
            .await?;
    }

    let sql_to_explain = req
        .sql
        .or(generated_sql)
        .ok_or_else(|| AppError::NotFound("query not found and no SQL provided".into()))?;

    if !is_safe_sql(&sql_to_explain) {
        return Err(AppError::ValidationError(
            "Only SELECT statements can be explained".into(),
        ));
    }

    // ── Step 2: look up cached result data by query_id ───────────────────────
    let cache_result = crate::nl2sql::result_cache::lookup_by_query_id(
        &state.db,
        &claims.tenant_id,
        &req.query_id,
    )
    .await;

    let (mut result_snapshot, cache_status) = match cache_result {
        crate::nl2sql::result_cache::CacheLookupResult::Hit(hit) => (hit.result_snapshot, None),
        crate::nl2sql::result_cache::CacheLookupResult::Expired => (None, Some("expired")),
        crate::nl2sql::result_cache::CacheLookupResult::NotFound => (None, Some("not_found")),
    };
    if result_snapshot.is_none() && effective_data_source_id.is_none() {
        let agent_rows: Option<String> = sqlx::query_scalar(
            "SELECT rows_json \
             FROM nl2sql_agent_query_results \
             WHERE tenant_id = ? AND user_id = ? AND query_id = ? \
             LIMIT 1",
        )
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&req.query_id)
        .fetch_optional(&state.db)
        .await?
        .flatten();
        result_snapshot = agent_rows
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    }

    // If no cached data, gracefully degrade to SQL-only explanation instead of 410.
    let rows: Vec<serde_json::Value> = result_snapshot
        .as_ref()
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let snapshot_row_count = rows.len();
    let row_count = rows_returned
        .and_then(|value| usize::try_from(value.max(0)).ok())
        .unwrap_or(snapshot_row_count);
    let used_cache_fallback = result_snapshot.is_none();
    if used_cache_fallback {
        tracing::info!(
            tenant_id = %claims.tenant_id,
            query_id = %req.query_id,
            status = %cache_status.unwrap_or("not_found"),
            "nl2sql explain: result snapshot missing, fallback to SQL-only explanation"
        );
    }

    // ── Step 3: fetch schema ─────────────────────────────────────────────────
    let schema_info: serde_json::Value = {
        if let Some(ds_id) = effective_data_source_id.as_deref() {
            let row = sqlx::query("SELECT schema_info FROM data_sources WHERE id = ?")
                .bind(ds_id)
                .fetch_optional(&state.db)
                .await?;
            match row {
                Some(r) => r
                    .get::<Option<serde_json::Value>, _>("schema_info")
                    .unwrap_or_else(super::empty_schema_info),
                None => super::empty_schema_info(),
            }
        } else {
            serde_json::json!({
                "tables": [],
                "foreign_keys": [],
                "note": "multi-source agent result; schema is represented by the generated per-step SQL"
            })
        }
    };

    // ── Step 4: build data summary for LLM prompt ───────────────────────────
    let data_summary = nl2sql_domain::sql::build_data_summary_for_rows(&rows);

    // ── Step 5: call LLM to generate explanation + insights ─────────────────
    let language = &req.language;
    let explanation = explain_with_data(
        &state,
        &claims,
        question.as_deref().unwrap_or("N/A"),
        &sql_to_explain,
        &schema_info,
        &data_summary,
        row_count,
        snapshot_row_count,
        language,
    )
    .await
    .unwrap_or_else(|_e| {
        let fallback = if language == "zh-CN" {
            if used_cache_fallback {
                "结果缓存已过期或不存在，只能基于 SQL 进行有限说明。请重新执行查询后再查看数据洞察。"
            } else {
                "解释生成失败。请先查看 SQL 和查询结果。"
            }
        } else if used_cache_fallback {
            "The result cache is missing or expired, so only a limited SQL-only explanation is available. Re-run the query to generate data insights."
        } else {
            "Failed to generate explanation. Please review the SQL and results manually."
        };
        DataExplainResult {
            explanation: fallback.to_string(),
            summary: fallback.to_string(),
            insights: Vec::new(),
            actions: Vec::new(),
            risks: Vec::new(),
            chart_recommendation: None,
            column_notes: Vec::new(),
        }
    });

    Ok(Json(ExplainResponse {
        explanation: explanation.explanation,
        summary: explanation.summary,
        insights: explanation.insights,
        actions: explanation.actions,
        risks: explanation.risks,
        chart_recommendation: explanation.chart_recommendation,
        column_notes: explanation.column_notes,
    }))
}

/// Call LLM to explain SQL with actual result data.
async fn explain_with_data(
    state: &AppState,
    claims: &Claims,
    question: &str,
    sql: &str,
    schema: &serde_json::Value,
    data_summary: &str,
    row_count: usize,
    snapshot_row_count: usize,
    language: &str,
) -> anyhow::Result<DataExplainResult> {
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve chat config: {}", e))?;

    let (system_prompt, lang_label) = if language == "zh-CN" {
        ("你是一位面向业务负责人和数据分析师的数据洞察助手。用户提出了一个问题并执行了一条 SQL 查询，查询结果以 JSON 格式提供。\n请基于结果直接回答用户真正关心的业务问题，而不是只解释 SQL。\n要求：\n1. summary 用 1-2 句话给出可决策的核心结论。\n2. explanation 用简洁段落说明关键对比、占比、异常、趋势或排序原因；如果结果不足以支撑结论，要明确说明限制。\n3. insights 给出 2-5 条关键洞察，优先包含差异、百分比、异常值、贡献项和反直觉点。\n4. actions 给出 1-4 条下一步动作或继续分析建议，必须和当前结果相关。\n5. risks 给出 0-3 条数据口径、样本量、缺失字段、时间范围等注意事项。\n6. chart_recommendation 给出适合的图表类型和原因，例如柱状图/折线图/散点图/表格。\n7. column_notes 只说明核心字段，不要机械解释每一列。\n8. 如果 snapshot_row_count 小于 row_count，必须把当前结果视为样本/分页快照，不要声称已分析全量明细；但 SQL 聚合结果本身可以作为聚合结论。\n只返回 JSON，不要任何 JSON 外文字：\n{\"summary\":\"...\",\"explanation\":\"...\",\"insights\":[\"...\"],\"actions\":[\"...\"],\"risks\":[\"...\"],\"chart_recommendation\":\"...\",\"column_notes\":[{\"column\":\"列名\",\"observation\":\"说明\"}]}".to_string(), "中文")
    } else {
        ("You are a data insight assistant for business owners and analysts. A user asked a question and an SQL query was executed. Results are provided as JSON.\nAnswer the business question directly instead of merely explaining the SQL.\nRequirements:\n1. summary: 1-2 decision-oriented sentences.\n2. explanation: concise narrative covering key comparisons, shares, anomalies, trends, rankings, and limits.\n3. insights: 2-5 key insights, prioritizing deltas, percentages, outliers, contribution, and counter-intuitive findings.\n4. actions: 1-4 concrete next actions or follow-up analyses tied to the current result.\n5. risks: 0-3 caveats about metric definition, sample size, missing columns, timeframe, or data quality.\n6. chart_recommendation: suitable visualization and why.\n7. column_notes: explain only the most important fields.\n8. If snapshot_row_count is smaller than row_count, treat the visible rows as a sample/page snapshot and do not claim full-detail coverage; SQL aggregate rows can still support aggregate conclusions.\nReturn ONLY valid JSON, no markdown, no text outside JSON:\n{\"summary\":\"...\",\"explanation\":\"...\",\"insights\":[\"...\"],\"actions\":[\"...\"],\"risks\":[\"...\"],\"chart_recommendation\":\"...\",\"column_notes\":[{\"column\":\"col_name\",\"observation\":\"note\"}]}".to_string(), "English")
    };

    let schema_summary = serde_json::to_string_pretty(schema).unwrap_or_default();

    #[derive(serde::Serialize)]
    struct Prompt<'a> {
        question: &'a str,
        sql: &'a str,
        row_count: usize,
        snapshot_row_count: usize,
        column_schema: &'a str,
        query_results: &'a str,
        language: &'a str,
    }
    let prompt_json = serde_json::to_string(&Prompt {
        question,
        sql,
        row_count,
        snapshot_row_count,
        column_schema: &schema_summary,
        query_results: data_summary,
        language: lang_label,
    })
    .map_err(|e| anyhow::anyhow!("failed to serialize prompt: {}", e))?;

    let request = MessageRequest {
        model: chat_cfg.model.clone(),
        max_tokens: 2048,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt_json }],
        }],
        system: Some(system_prompt),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.3),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let client = chat_cfg.client;
    let response = client
        .send_message(&request)
        .await
        .map_err(|e| anyhow::anyhow!("LLM call failed: {}", e))?;

    let text = response
        .content
        .iter()
        .find_map(|block| match block {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let trimmed = text.trim().trim_start_matches('`').trim();

    #[derive(serde::Deserialize)]
    struct LlmResult {
        #[serde(default)]
        explanation: String,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        insights: Vec<String>,
        #[serde(default)]
        actions: Vec<String>,
        #[serde(default)]
        risks: Vec<String>,
        #[serde(default)]
        chart_recommendation: Option<String>,
        #[serde(default)]
        column_notes: Vec<ColumnNote>,
    }

    let cleaned = trimmed
        .strip_prefix("json")
        .unwrap_or(trimmed)
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed = match serde_json::from_str::<LlmResult>(cleaned) {
        Ok(value) => value,
        Err(first_err) => {
            let Some(start) = cleaned.find('{') else {
                return Err(anyhow::anyhow!(
                    "failed to parse explanation JSON: {}",
                    first_err
                ));
            };
            let Some(end) = cleaned.rfind('}') else {
                return Err(anyhow::anyhow!(
                    "failed to parse explanation JSON: {}",
                    first_err
                ));
            };
            serde_json::from_str::<LlmResult>(&cleaned[start..=end])
                .map_err(|e| anyhow::anyhow!("failed to parse explanation JSON: {}", e))?
        }
    };
    let summary = parsed.summary.trim().to_string();
    let explanation = parsed.explanation.trim().to_string();
    let fallback = if !summary.is_empty() {
        summary.clone()
    } else if !explanation.is_empty() {
        explanation.clone()
    } else if language == "zh-CN" {
        "查询已执行，但模型未返回有效解释。".to_string()
    } else {
        "The query was executed, but the model did not return a usable explanation.".to_string()
    };

    Ok(DataExplainResult {
        explanation: if explanation.is_empty() {
            fallback.clone()
        } else {
            explanation
        },
        summary: if summary.is_empty() {
            fallback
        } else {
            summary
        },
        insights: parsed
            .insights
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        actions: parsed
            .actions
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        risks: parsed
            .risks
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        chart_recommendation: parsed
            .chart_recommendation
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        column_notes: parsed
            .column_notes
            .into_iter()
            .filter(|note| !note.column.trim().is_empty() && !note.observation.trim().is_empty())
            .map(|note| ColumnNote {
                column: note.column.trim().to_string(),
                observation: note.observation.trim().to_string(),
            })
            .collect(),
    })
}

// ── Health check ──────────────────────────────────────────────────────────────

/// GET /api/v1/nl2sql/embedding-health — health check for the vector store.
pub(crate) async fn embedding_health(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.nl2sql_embedding_store.as_ref() {
        Some(store) => {
            let ann = store.ann_runtime_health();
            Json(serde_json::json!({
                "status": "ok",
                "total_vectors": store.len(),
                "ann": ann,
            }))
        }
        None => Json(serde_json::json!({
            "status": "unavailable",
            "reason": "bundled local vector store did not initialize; inspect the server startup log",
            "ann": {
                "state": "unavailable",
                "reason": "bundled local vector store did not initialize; inspect the server startup log",
                "loaded_in_memory": false,
                "base_points": 0,
                "overlay_points": 0,
                "stale_points": 0,
                "disk_artifacts_present": false
            }
        })),
    }
}

// ── Semantic Routing ─────────────────────────────────────────────────────────

/// POST /api/v1/nl2sql/route — semantic routing using 3-level embedding fusion.
pub(crate) async fn route(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RouteRequest>,
) -> Result<Json<RouteResponse>> {
    let route_started_at = std::time::Instant::now();
    // Fetch accessible data sources
    let rows = sqlx::query(
        "SELECT id, name, description, schema_info FROM data_sources \
         WHERE tenant_id = ? AND (user_id = ? OR user_id IS NULL) AND enabled = 1",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .fetch_all(&state.db)
    .await?;

    let sources: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| {
            (
                r.get("id"),
                r.get("name"),
                r.get::<Option<String>, _>("description")
                    .unwrap_or_default(),
            )
        })
        .collect();
    let schema_table_counts: std::collections::HashMap<String, usize> = rows
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("id"),
                schema_table_count_from_info(r.get::<Option<serde_json::Value>, _>("schema_info")),
            )
        })
        .collect();
    tracing::info!(
        tenant_id = %claims.tenant_id,
        user_id = %claims.sub,
        question_chars = req.question.chars().count(),
        source_count = sources.len(),
        "route: loaded accessible data sources"
    );

    // Explicit domain labels are deterministic routing metadata. Resolve them
    // before vector search so a configured domain remains usable even when its
    // human-facing name is not semantically similar to physical table names.
    let tenant_domains: Vec<crate::nl2sql::routing::BusinessDomain> =
        if should_enable_domain_routing() {
            match crate::nl2sql::routing::resolve_all_business_domains_for_tenant(
                &state.db,
                &claims.tenant_id,
            )
            .await
            {
                Ok(domains) => domains,
                Err(error) => {
                    tracing::warn!(
                        tenant_id = %claims.tenant_id,
                        error = %error,
                        "failed to load business domains for semantic routing"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
    if let Some(route_result) = route_result_from_explicit_business_domains(
        &tenant_domains,
        &req.question,
        &sources,
        req.data_source_id.as_deref(),
    ) {
        emit_route_stage(
            "route_selected",
            "已通过配置的业务域定位数据源",
            Some(
                route_result
                    .matched_tables
                    .iter()
                    .map(|table| crate::nl2sql::MatchedTable {
                        table_name: table.table_name.clone(),
                        best_column: table.best_column.clone(),
                        column_description: table.column_description.clone(),
                        similarity_score: table.similarity_score,
                    })
                    .collect(),
            ),
            Some(route_result.confidence),
            Some(route_result.method.clone()),
        );
        tracing::info!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            datasource_id = %route_result.data_source_id,
            matched_tables = route_result.matched_tables.len(),
            elapsed_ms = route_started_at.elapsed().as_millis() as u64,
            "route: resolved by explicit business-domain label"
        );
        return Ok(Json(RouteResponse {
            routed: true,
            result: Some(route_result),
            error: None,
        }));
    }

    // Vector routing is the fallback after deterministic domain routing.
    let embed_store = match state.nl2sql_embedding_store.as_ref() {
        Some(store) => store,
        None => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(
                    "semantic routing is temporarily unavailable because the local vector store \
                     did not initialize; inspect the server startup log and retry. A remote \
                     Embedding API is an optional retrieval-quality enhancement, not a prerequisite."
                        .to_string(),
                ),
            }));
        }
    };

    let _engine = match state.nl2sql_routing_engine.as_ref() {
        Some(engine) => engine,
        None => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(
                    "semantic routing engine is not available. \
                     Please ensure the embedding store is properly initialized."
                        .to_string(),
                ),
            }));
        }
    };

    emit_route_stage(
        "sql_knowledge_probe",
        "正在检查 SQL 知识库命中",
        None,
        None,
        None,
    );
    let sql_knowledge_route = best_sql_knowledge_route_candidate(
        &state,
        &claims.tenant_id,
        &req.question,
        &sources,
        &schema_table_counts,
    )
    .await;
    if let Some(candidate) = sql_knowledge_route.as_ref() {
        tracing::info!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            question_chars = req.question.chars().count(),
            datasource_id = %candidate.data_source_id,
            score = candidate.score,
            confidence = candidate.confidence,
            snippets = candidate.snippet_count,
            schema_tables = candidate.schema_table_count,
            "route: SQL knowledge route candidate found"
        );
    }

    let profiles =
        crate::nl2sql::embedding_profiles::reconcile_tenant_profiles(&state.db, &claims.tenant_id)
            .await
            .map_err(|error| {
                AppError::Internal(format!("failed to resolve embedding profiles: {error}"))
            })?;
    let datasource_ids: Vec<String> = sources.iter().map(|(id, _, _)| id.clone()).collect();

    let mut selected_profile = profiles.local.clone();
    let mut question_vec = None;
    if let Some(api_profile) = &profiles.api {
        let api_ready = crate::nl2sql::embedding_profiles::active_profile_ready_for_datasources(
            &state.db,
            &claims.tenant_id,
            api_profile,
            &datasource_ids,
        )
        .await
        .unwrap_or(false);
        let circuit_allows =
            crate::nl2sql::embedding_profiles::circuit_allows_request(&state.db, &api_profile.id)
                .await
                .unwrap_or(false);
        if api_ready && circuit_allows {
            let model = crate::nl2sql::embedding::EmbeddingModel::new_with_dimensions(
                &api_profile.config.model,
                api_profile.config.base_url.clone(),
                Some(api_profile.config.api_key.clone()),
                api_profile.config.dimensions,
            );
            match model.embed_batch(&[req.question.clone()]).await {
                Ok(mut vectors) => {
                    let vector = vectors.pop().unwrap_or_default();
                    if vectors.is_empty()
                        && vector.len() == api_profile.config.effective_dimensions()
                    {
                        question_vec = Some(vector);
                        selected_profile = api_profile.clone();
                        let _ = crate::nl2sql::embedding_profiles::record_profile_success(
                            &state.db,
                            &api_profile.id,
                        )
                        .await;
                    } else {
                        let error = format!(
                            "API query embedding returned an incompatible batch/dimension (remaining={}, dimensions={}, expected={})",
                            vectors.len(),
                            vector.len(),
                            api_profile.config.effective_dimensions()
                        );
                        let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                            &state.db,
                            &api_profile.id,
                            &error,
                        )
                        .await;
                        tracing::warn!(
                            tenant_id = %claims.tenant_id,
                            profile_id = %api_profile.id,
                            error = %error,
                            "API embedding query was incompatible; switching to local profile"
                        );
                    }
                }
                Err(error) => {
                    let _ = crate::nl2sql::embedding_profiles::record_profile_failure(
                        &state.db,
                        &api_profile.id,
                        &error.to_string(),
                    )
                    .await;
                    tracing::warn!(
                        tenant_id = %claims.tenant_id,
                        profile_id = %api_profile.id,
                        error = %error,
                        "API embedding query failed; switching to local profile"
                    );
                }
            }
        }
    }

    if question_vec.is_none() {
        let local_model = crate::nl2sql::embedding::EmbeddingModel::new_with_dimensions(
            &profiles.local.config.model,
            profiles.local.config.base_url.clone(),
            None,
            profiles.local.config.dimensions,
        );
        let mut local_vectors = local_model
            .embed_batch(&[req.question.clone()])
            .await
            .map_err(|error| {
                AppError::Internal(format!("bundled local embedding failed: {error}"))
            })?;
        let local_vector = local_vectors.pop().unwrap_or_default();
        if !local_vectors.is_empty()
            || local_vector.len() != profiles.local.config.effective_dimensions()
        {
            return Err(AppError::Internal(format!(
                "bundled local embedding returned an incompatible batch/dimension (remaining={}, dimensions={}, expected={})",
                local_vectors.len(),
                local_vector.len(),
                profiles.local.config.effective_dimensions()
            )));
        }
        question_vec = Some(local_vector);
        selected_profile = profiles.local.clone();
        let _ = crate::nl2sql::embedding_profiles::record_profile_success(
            &state.db,
            &profiles.local.id,
        )
        .await;
    }
    let question_vec = question_vec.unwrap_or_default();
    let selected_store = embed_store
        .profile_store(
            &claims.tenant_id,
            &selected_profile.id,
            &selected_profile.config.model,
            selected_profile.config.base_url.clone(),
        )
        .map_err(|error| {
            AppError::Internal(format!("failed to open embedding profile: {error}"))
        })?;
    let embed_api_key = selected_profile
        .config
        .key_id
        .as_ref()
        .map(|_| selected_profile.config.api_key.clone());

    emit_route_stage("vector_matching", "向量相似度匹配中", None, None, None);
    emit_route_stage("search_candidates", "检索候选表", None, None, None);
    // ── P0-1: Hybrid routing — embedding coarse filter + LLM tool-calling ──────
    let use_ann = std::env::var("NL2SQL_USE_ANN_INDEX")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);

    let embed_store_for_coarse = Arc::clone(&selected_store);
    let req_question = req.question.clone();
    let sources_clone = sources.clone();
    let embed_api_key_clone = embed_api_key.clone();
    let question_vec_clone = question_vec.clone();
    let candidates_job = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Handle::current();
        runtime.block_on(async move {
            crate::nl2sql::routing::global_coarse_search(
                &embed_store_for_coarse,
                &req_question,
                Some(question_vec_clone.as_slice()),
                embed_api_key_clone,
                use_ann,
                Some(&sources_clone),
            )
            .await
        })
    });
    let (candidates, allowed_ds) = match candidates_job.await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(format!(
                    "global table search failed: {}. Falling back to legacy routing.",
                    e
                )),
            }));
        }
        Err(e) => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(format!(
                    "global table search task failed: {}. Falling back to legacy routing.",
                    e
                )),
            }));
        }
    };
    tracing::info!(
        tenant_id = %claims.tenant_id,
        user_id = %claims.sub,
        question_chars = req.question.chars().count(),
        candidate_count = candidates.len(),
        allowed_ds_count = allowed_ds.as_ref().map(|s| s.len()).unwrap_or(0),
        elapsed_ms = route_started_at.elapsed().as_millis() as u64,
        "route: coarse search completed"
    );

    if candidates.is_empty() {
        if let Some(candidate) = sql_knowledge_route
            .as_ref()
            .filter(|candidate| should_prefer_sql_knowledge_route(candidate, None))
        {
            let route_result = route_result_from_sql_knowledge(candidate);
            emit_route_stage(
                "route_selected",
                &format!(
                    "已通过 SQL 知识库找到数据源 {}",
                    route_result.data_source_id
                ),
                Some(
                    route_result
                        .matched_tables
                        .iter()
                        .map(|t| crate::nl2sql::MatchedTable {
                            table_name: t.table_name.clone(),
                            best_column: t.best_column.clone(),
                            column_description: t.column_description.clone(),
                            similarity_score: t.similarity_score,
                        })
                        .collect(),
                ),
                Some(route_result.confidence),
                Some(route_result.method.clone()),
            );
            tracing::info!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                question_chars = req.question.chars().count(),
                datasource_id = %route_result.data_source_id,
                confidence = route_result.confidence,
                elapsed_ms = route_started_at.elapsed().as_millis() as u64,
                "route: resolved by SQL knowledge because schema candidates were empty"
            );
            return Ok(Json(RouteResponse {
                routed: true,
                result: Some(route_result),
                error: None,
            }));
        }
        let source_ids: Vec<String> = sources.iter().map(|(id, _, _)| id.clone()).collect();
        let total_col_embeddings = selected_store.col_len();
        let scoped_col_embeddings = selected_store.col_len_for_datasources(&source_ids);
        let prefiltered_datasources = allowed_ds.as_ref().map(|s| s.len()).unwrap_or(0);
        if total_col_embeddings == 0 || scoped_col_embeddings == 0 {
            tracing::warn!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                question_chars = req.question.chars().count(),
                total_accessible_datasources = sources.len(),
                prefiltered_datasources,
                total_col_embeddings,
                scoped_col_embeddings,
                min_table_sim = crate::nl2sql::min_table_sim_threshold(),
                "route empty candidates reason=no_embeddings_or_not_refreshed"
            );
        } else {
            tracing::info!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                question_chars = req.question.chars().count(),
                total_accessible_datasources = sources.len(),
                prefiltered_datasources,
                total_col_embeddings,
                scoped_col_embeddings,
                min_table_sim = crate::nl2sql::min_table_sim_threshold(),
                ds_prefilter_enabled = std::env::var("NL2SQL_DS_EMBED_PRE_FILTER")
                    .ok()
                    .and_then(|v| v.parse::<bool>().ok())
                    .unwrap_or(true),
                "route empty candidates reason=all_filtered_by_similarity_or_prefilter"
            );
        }
        return Ok(Json(RouteResponse {
            routed: false,
            result: None,
            error: Some(
                "No candidate tables were found from the embedding store. \
                 Please refresh the schema first, or manually select a data source \
                 if this question is unrelated to the indexed tables."
                    .to_string(),
            ),
        }));
    }

    emit_route_stage(
        "ai_confirming",
        "AI 正在确认最合适的数据源",
        None,
        None,
        None,
    );

    // ── P1-1: Expand question with synonyms for richer routing coverage ───────────
    let (question_for_routing, _synonym_terms) = if let Some(ds_id) = &req.data_source_id {
        match crate::nl2sql::routing::expand_question_with_synonyms(
            &req.question,
            ds_id,
            &claims.tenant_id,
            &state.db,
        )
        .await
        {
            Ok(expanded) => {
                let synonym_terms: Vec<String> = vec![];
                (expanded, synonym_terms)
            }
            Err(_) => (req.question.clone(), vec![]),
        }
    } else {
        (req.question.clone(), vec![])
    };

    // ── P1-1: Load synonym terms for BM25 text matching (Path 2) ─────────────
    let synonym_terms_for_rrfs: Vec<String> = if let Some(ds_id) = &req.data_source_id {
        match sqlx::query_as::<_, (String,)>(
            "SELECT term FROM nl2sql_synonyms WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
        )
        .bind(&claims.tenant_id)
        .bind(ds_id)
        .fetch_all(&state.db)
        .await
        {
            Ok(rows) => rows.into_iter().map(|(t,)| t).collect(),
            Err(_) => vec![],
        }
    } else {
        vec![]
    };

    // ── RRFS multi-path routing (default: true) ──
    let use_rrfs = std::env::var("NL2SQL_USE_RRFS")
        .ok()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true); // was: false

    // Build domain table filter from loaded tenant domains (P0-2).
    let domain_table_filter: Option<std::collections::HashMap<(String, String), f32>> =
        if !tenant_domains.is_empty() {
            if let Some(ds_id) = req.data_source_id.as_deref().filter(|s| !s.is_empty()) {
                let mut filter = std::collections::HashMap::new();
                for domain in &tenant_domains {
                    if domain.datasource_id.as_deref() == Some(ds_id) {
                        for table in &domain.tables {
                            let boost = if is_strict_mode(&domain.routing_mode) {
                                domain.confidence_score.max(0.8)
                            } else {
                                domain.confidence_score
                            };
                            filter.insert((ds_id.to_string(), table.clone()), boost);
                        }
                    }
                }
                if filter.is_empty() {
                    None
                } else {
                    Some(filter)
                }
            } else {
                None
            }
        } else {
            None
        };

    // Per-domain strict policy: strict applies when the question explicitly references
    // a strict domain name. This keeps strict behavior precise and predictable.
    // It is domain-level governance (enterprise-grade), not a global toggle.
    let strict_allowlist: Option<
        std::collections::HashMap<String, std::collections::HashSet<String>>,
    > = if tenant_domains.is_empty() {
        None
    } else {
        let map = strict_allowlist_for_question(&tenant_domains, &req.question);
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    };

    if use_rrfs {
        emit_route_stage("rrfs_ranking", "RRFS 融合排序中", None, None, None);
        let ds_ids: Vec<String> = sources.iter().map(|(id, _, _)| id.clone()).collect();
        let rrfs_matches = match crate::nl2sql::routing::route_rrfs(
            &question_for_routing,
            &selected_store,
            Some(question_vec.as_slice()),
            Some(candidates.as_slice()),
            &ds_ids,
            embed_api_key.clone(),
            use_ann,
            &state.db,
            allowed_ds.as_ref(),
            domain_table_filter.as_ref(),
            Some(&synonym_terms_for_rrfs),
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "route_rrfs failed, falling back to hybrid");
                Vec::new()
            }
        };

        if !rrfs_matches.is_empty() {
            tracing::debug!(count = rrfs_matches.len(), "RRFS returned ranked tables");

            let best = rrfs_matches.first();
            let route_result = match best {
                Some(top) => {
                    let ds_id = &top.datasource_id;
                    let matched_tables: Vec<MatchedTableInfo> = rrfs_matches
                        .iter()
                        .filter(|m| m.datasource_id == *ds_id)
                        .filter(|m| {
                            strict_allowlist
                                .as_ref()
                                .and_then(|modes| modes.get(ds_id))
                                .map(|allow| allow.contains(&m.table_name))
                                .unwrap_or(true)
                        })
                        .take(10)
                        .map(|m| MatchedTableInfo {
                            data_source_id: m.datasource_id.clone(),
                            table_name: m.table_name.clone(),
                            best_column: String::new(),
                            column_description: String::new(),
                            similarity_score: (m.embed_similarity + 1.0) / 2.0,
                        })
                        .collect();

                    let confidence = (top.rrf_score / 0.1_f32).min(1.0);
                    if matched_tables.is_empty() {
                        return Ok(Json(RouteResponse {
                            routed: false,
                            result: None,
                            error: Some(
                                "No matched tables remain after applying strict business-domain policy."
                                    .to_string(),
                            ),
                        }));
                    }

                    RouteResult {
                        data_source_id: ds_id.clone(),
                        confidence,
                        method: "rrfs".to_string(),
                        matched_tables,
                        clarification_question: None,
                    }
                }
                None => {
                    return Ok(Json(RouteResponse {
                        routed: false,
                        result: None,
                        error: Some("RRFS returned no results.".to_string()),
                    }));
                }
            };
            let route_result = if let Some(candidate) =
                sql_knowledge_route.as_ref().filter(|candidate| {
                    should_prefer_sql_knowledge_route(candidate, Some(&route_result))
                }) {
                let knowledge_route = route_result_from_sql_knowledge(candidate);
                tracing::info!(
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    question_chars = req.question.chars().count(),
                    schema_datasource_id = %route_result.data_source_id,
                    schema_confidence = route_result.confidence,
                    knowledge_datasource_id = %knowledge_route.data_source_id,
                    knowledge_confidence = knowledge_route.confidence,
                    knowledge_score = candidate.score,
                    "route: SQL knowledge candidate replaced RRFS route"
                );
                knowledge_route
            } else {
                route_result
            };

            emit_route_stage(
                "route_selected",
                &format!("已找到数据源 {}", route_result.data_source_id),
                None,
                Some(route_result.confidence),
                Some(route_result.method.clone()),
            );
            tracing::info!(
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                question_chars = req.question.chars().count(),
                datasource_id = %route_result.data_source_id,
                method = %route_result.method,
                confidence = route_result.confidence,
                matched_tables = route_result.matched_tables.len(),
                elapsed_ms = route_started_at.elapsed().as_millis() as u64,
                "route: resolved by RRFS"
            );

            return Ok(Json(RouteResponse {
                routed: true,
                result: Some(route_result),
                error: None,
            }));
        } else {
            tracing::warn!(
                question_chars = req.question.chars().count(),
                datasource_count = sources.len(),
                "RRFS returned no matches; falling back to hybrid LLM routing"
            );
        }
    }

    // ── LLM routing (route_hybrid) ────────────────────────────────────────────
    // Resolve LLM client for routing using per-tenant config.
    let llm_model_default = std::env::var("NL2SQL_ROUTING_LLM_MODEL")
        .ok()
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    let (llm_client, llm_model, llm_meta) = match crate::nl2sql::routing::resolve_routing_llm(
        &state.config_registry,
        &claims.tenant_id,
        &claims.sub,
        &llm_model_default,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(format!("LLM client resolution failed: {}", e)),
            }));
        }
    };

    // Load column descriptions for the candidate datasources
    let candidate_ds_ids: Vec<String> =
        candidates.iter().map(|c| c.datasource_id.clone()).collect();
    let col_descriptions: std::collections::HashMap<(String, String), String> = selected_store
        .get_column_descriptions_for_datasources(&state.db, &candidate_ds_ids)
        .await;

    // Enrich candidates with column descriptions
    // ── P0-2: LLM domain classification → inject into LLM routing prompt ──────────
    // Classify the question into business domains using the routing LLM client.
    // This overrides the static DB-based domain context with dynamic LLM analysis.
    let domain_context_str: Option<String> = if !tenant_domains.is_empty() {
        emit_route_stage("domain_classifying", "业务域分类中", None, None, None);
        // P0-2: LLM-based classification (after llm_client is resolved)
        match crate::nl2sql::routing::classify_question_to_domains(
            &req.question,
            &tenant_domains,
            &llm_client,
            &llm_model,
            &llm_meta,
        )
        .await
        {
            Ok(classified) if !classified.is_empty() => {
                tracing::debug!(
                    question_chars = req.question.chars().count(),
                    domains = ?classified,
                    "P0-2: classified into {} relevant domains",
                    classified.len()
                );
                // Filter domains to only those classified
                let relevant: Vec<_> = tenant_domains
                    .iter()
                    .filter(|d| classified.iter().any(|(n, _)| n == &d.domain_name))
                    .cloned()
                    .collect();
                Some(crate::nl2sql::routing::build_domain_context(
                    &relevant,
                    &req.question,
                ))
            }
            Ok(_) | Err(_) => {
                // No confident domain match or classification failed — use static context
                Some(crate::nl2sql::routing::build_domain_context(
                    &tenant_domains,
                    &req.question,
                ))
            }
        }
    } else {
        None
    };

    let candidates = candidates
        .into_iter()
        .map(|mut c| {
            if let Some(desc) = col_descriptions.get(&(c.table_name.clone(), c.best_column.clone()))
            {
                c.column_description = desc.clone();
            }
            c
        })
        .filter(|c| {
            strict_allowlist
                .as_ref()
                .and_then(|modes| modes.get(&c.datasource_id))
                .map(|allow| allow.contains(&c.table_name))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Json(RouteResponse {
            routed: false,
            result: None,
            error: Some(
                "No candidate tables remain after applying strict business-domain policy."
                    .to_string(),
            ),
        }));
    }

    emit_route_stage("llm_routing", "LLM 路由决策中", None, None, None);
    let routing_decision = match crate::nl2sql::routing::route_hybrid(
        &selected_store,
        &question_for_routing,
        candidates,
        &sources,
        &llm_client,
        &llm_model,
        &llm_meta,
        Some(&col_descriptions),
        domain_context_str.as_deref(),
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some(format!(
                    "LLM routing failed: {}. Falling back to best embedding match.",
                    e
                )),
            }));
        }
    };

    // Convert LlmRoutingDecision → Option<RoutingResult>
    let result: Option<crate::nl2sql::RoutingResult> = match routing_decision {
        crate::nl2sql::LlmRoutingDecision::HighConfidence(r) => Some(r),
        crate::nl2sql::LlmRoutingDecision::NeedsClarification {
            clarification_question,
            options,
            domain_context: _,
        } => {
            emit_route_stage(
                "manual_continue",
                "当前问题需要补充信息，等待手动继续",
                Some(
                    options
                        .iter()
                        .map(|o| crate::nl2sql::MatchedTable {
                            // Backward compatibility: old clients may send no
                            // `business_meaning`, only `reason`.
                            column_description: if o.business_meaning.trim().is_empty() {
                                o.reason.clone()
                            } else {
                                o.business_meaning.clone()
                            },
                            table_name: o.table_name.clone(),
                            best_column: o.column_name.clone(),
                            similarity_score: o.sim_score,
                        })
                        .collect(),
                ),
                None,
                None,
            );
            // Return clarification as part of the route response.
            return Ok(Json(RouteResponse {
                routed: false,
                result: Some(RouteResult {
                    data_source_id: String::new(),
                    confidence: 0.0,
                    method: "clarification".to_string(),
                    matched_tables: options
                        .into_iter()
                        .map(|o| {
                            let column_description = if o.business_meaning.trim().is_empty() {
                                o.reason.clone()
                            } else {
                                o.business_meaning.clone()
                            };
                            MatchedTableInfo {
                                data_source_id: o.data_source_id.clone(),
                                table_name: o.table_name,
                                best_column: o.column_name,
                                column_description,
                                similarity_score: (o.sim_score + 1.0) / 2.0,
                            }
                        })
                        .collect(),
                    clarification_question: Some(clarification_question),
                }),
                error: None,
            }));
        }
        crate::nl2sql::LlmRoutingDecision::LowConfidence(r) => Some(r),
        crate::nl2sql::LlmRoutingDecision::CrossDatasource {
            datasources,
            reason,
        } => {
            emit_route_stage(
                "ready",
                "检测到跨数据源路径",
                None,
                Some(0.85),
                Some("cross-datasource".to_string()),
            );
            return Ok(Json(RouteResponse {
                routed: false,
                result: Some(RouteResult {
                    data_source_id: "multi-datasource".to_string(),
                    confidence: 0.85,
                    method: "cross-datasource".to_string(),
                    matched_tables: datasources
                        .iter()
                        .flat_map(|ds| {
                            ds.matched_tables.iter().map(|t| MatchedTableInfo {
                                data_source_id: ds.data_source_id.clone(),
                                table_name: t.table_name.clone(),
                                best_column: t.best_column.clone(),
                                column_description: t.column_description.clone(),
                                similarity_score: t.similarity_score,
                            })
                        })
                        .collect(),
                    clarification_question: Some(
                        "This query requires data from multiple datasources. \
                         Please use the multi-datasource query feature to execute it."
                            .to_string(),
                    ),
                }),
                error: Some(format!(
                    "cross_datasource: {} (datasources: {})",
                    reason,
                    datasources
                        .iter()
                        .map(|ds| ds.data_source_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }));
        }
    };

    let route_result = match result {
        Some(r) => {
            // Load column descriptions from nl2sql_table_semantics for enriched UI display.
            let col_descriptions: std::collections::HashMap<(String, String), String> = {
                let rows = sqlx::query(
                    "SELECT table_name, column_name, semantic_description \
                     FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
                )
                .bind(&r.data_source_id)
                .fetch_all(&state.db)
                .await?;
                rows.iter()
                    .filter_map(|row| {
                        let table: String = row.get("table_name");
                        let col: String = row.get("column_name");
                        let desc: String = row.get("semantic_description");
                        if desc.is_empty() {
                            None
                        } else {
                            Some(((table, col), desc))
                        }
                    })
                    .collect()
            };

            // Use the already-computed question_vec for detailed scoring.
            let matched_tables = match crate::nl2sql::routing::score_tables_detailed(
                selected_store.as_ref(),
                &question_vec,
                &r.data_source_id,
                &col_descriptions,
            )
            .await
            {
                Ok(scores) => scores
                    .into_iter()
                    .map(|s| MatchedTableInfo {
                        data_source_id: r.data_source_id.clone(),
                        table_name: s.table_name,
                        best_column: s.best_column,
                        column_description: s.column_description,
                        similarity_score: s.fused_score,
                    })
                    .collect(),
                Err(_) => r
                    .matched_tables
                    .into_iter()
                    .map(|t| MatchedTableInfo {
                        data_source_id: r.data_source_id.clone(),
                        table_name: t.table_name,
                        best_column: t.best_column,
                        column_description: String::new(),
                        similarity_score: t.similarity_score,
                    })
                    .collect(),
            };

            RouteResult {
                data_source_id: r.data_source_id,
                confidence: r.confidence,
                method: r.method.to_string(),
                matched_tables,
                clarification_question: None,
            }
        }
        None => {
            return Ok(Json(RouteResponse {
                routed: false,
                result: None,
                error: Some("No confident routing result. Fall back to manual selection.".into()),
            }));
        }
    };

    let route_result = if let Some(candidate) = sql_knowledge_route
        .as_ref()
        .filter(|candidate| should_prefer_sql_knowledge_route(candidate, Some(&route_result)))
    {
        let knowledge_route = route_result_from_sql_knowledge(candidate);
        tracing::info!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            question_chars = req.question.chars().count(),
            schema_datasource_id = %route_result.data_source_id,
            schema_confidence = route_result.confidence,
            knowledge_datasource_id = %knowledge_route.data_source_id,
            knowledge_confidence = knowledge_route.confidence,
            knowledge_score = candidate.score,
            "route: SQL knowledge candidate replaced final schema route"
        );
        knowledge_route
    } else {
        route_result
    };

    emit_route_stage(
        "route_selected",
        &format!("已找到数据源 {}", route_result.data_source_id),
        Some(
            route_result
                .matched_tables
                .iter()
                .map(|t| crate::nl2sql::MatchedTable {
                    table_name: t.table_name.clone(),
                    best_column: t.best_column.clone(),
                    column_description: t.column_description.clone(),
                    similarity_score: t.similarity_score,
                })
                .collect(),
        ),
        Some(route_result.confidence),
        Some(route_result.method.clone()),
    );
    tracing::info!(
        tenant_id = %claims.tenant_id,
        user_id = %claims.sub,
        question_chars = req.question.chars().count(),
        datasource_id = %route_result.data_source_id,
        method = %route_result.method,
        confidence = route_result.confidence,
        matched_tables = route_result.matched_tables.len(),
        elapsed_ms = route_started_at.elapsed().as_millis() as u64,
        "route: final route resolved"
    );

    Ok(Json(RouteResponse {
        routed: true,
        result: Some(route_result),
        error: None,
    }))
}

pub(crate) async fn start_route_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<RouteRequest>,
) -> Result<Json<StartRouteTaskResponse>> {
    let task_id = format!("nl2sql-route-task-{}", uuid::Uuid::new_v4());
    let manager = route_task_manager().clone();
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
        let manager2 = route_task_manager().clone();
        let route_task_hard_timeout = manager2.config.task_hard_timeout;
        let manager2_for_fut = manager2.clone();
        let task_id_for_fut = task_id_clone.clone();
        manager2
            .publish_stage(&task_id_clone, "request_validation", "开始识别数据源", None)
            .await;

        let task_id_for_stage = task_id_clone.clone();
        let stage_emitter: RouteStageEmitter = Arc::new(move |signal| {
            let task_id_inner = task_id_for_stage.clone();
            tokio::spawn(async move {
                let manager = route_task_manager().clone();
                manager
                    .publish_stage(&task_id_inner, &signal.stage, &signal.message, None)
                    .await;
            });
        });

        match run_lifecycle_hooks(
            &hook_state,
            &hook_tenant_id,
            "nl2sql",
            HookEventType::BeforeRoute,
            "nl2sql.route",
            serde_json::json!({
                "taskId": &task_id_clone,
            }),
            None,
            false,
        )
        .await
        {
            Ok(hook_result) if hook_result.is_denied() => {
                manager2
                    .publish_failed(
                        &task_id_clone,
                        format!(
                            "before_route hook denied route task: {}",
                            hook_result.messages().join("\n")
                        ),
                    )
                    .await;
                return;
            }
            Ok(hook_result) if hook_result.is_failed() || hook_result.is_cancelled() => {
                manager2
                    .publish_failed(
                        &task_id_clone,
                        format!(
                            "before_route hook failed route task: {}",
                            hook_result.messages().join("\n")
                        ),
                    )
                    .await;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                manager2
                    .publish_failed(
                        &task_id_clone,
                        format!("before_route hook failed to execute: {error}"),
                    )
                    .await;
                return;
            }
        }

        let route_fut =
            crate::nl2sql::routing::with_route_stage_emitter(stage_emitter, async move {
                manager2_for_fut
                    .publish_stage(&task_id_for_fut, "search_candidates", "检索候选表", None)
                    .await;
                route(State(state_clone), Extension(claims_clone), Json(req)).await
            });
        let result: Result<Json<RouteResponse>> =
            match tokio::time::timeout(route_task_hard_timeout, route_fut).await {
                Ok(inner) => inner,
                Err(_) => {
                    tracing::warn!(
                        task_id = %task_id_clone,
                        timeout_secs = route_task_hard_timeout.as_secs(),
                        "route task hard timeout reached; failing task to prevent slot starvation"
                    );
                    manager2
                        .publish_failed(
                            &task_id_clone,
                            format!(
                                "route task hard timeout (server {}s)",
                                route_task_hard_timeout.as_secs()
                            ),
                        )
                        .await;
                    return;
                }
            };

        match result {
            Ok(Json(resp)) => {
                match run_lifecycle_hooks(
                    &hook_state,
                    &hook_tenant_id,
                    "nl2sql",
                    HookEventType::AfterRoute,
                    "nl2sql.route",
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
                            "after_route hook completed with warning: {}",
                            hook_result.messages().join("\n")
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            tenant_id = %hook_tenant_id,
                            task_id = %task_id_clone,
                            error = %error,
                            "after_route hook failed to execute"
                        );
                    }
                }
                manager2.publish_completed(&task_id_clone, &resp).await;
                match run_lifecycle_hooks(
                    &hook_state,
                    &hook_tenant_id,
                    "nl2sql",
                    HookEventType::TaskCompleted,
                    "nl2sql.route_completed",
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
                            "nl2sql route task_completed hook completed with warning: {}",
                            hook_result.messages().join("\n")
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            tenant_id = %hook_tenant_id,
                            task_id = %task_id_clone,
                            error = %error,
                            "nl2sql route task_completed hook failed to execute"
                        );
                    }
                }
            }
            Err(e) => {
                manager2.publish_failed(&task_id_clone, e.to_string()).await;
            }
        }
    });

    Ok(Json(StartRouteTaskResponse {
        task_id,
        status: "queued".to_string(),
    }))
}

pub(crate) async fn get_route_task_status(
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<RouteTaskStatusResponse>> {
    let manager = route_task_manager();
    let snapshot = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("route task not found".to_string()))?;
    Ok(Json(RouteTaskStatusResponse {
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

pub(crate) async fn stream_route_task_events(
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<
    Sse<
        impl futures_util::stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>,
    >,
> {
    let manager = route_task_manager().clone();
    let snapshot = manager
        .snapshot(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("route task not found".to_string()))?;
    let mut rx = manager
        .subscribe(&task_id, &claims.tenant_id, &claims.sub)
        .await
        .ok_or_else(|| AppError::NotFound("route task not found".to_string()))?;

    let stream = async_stream::stream! {
        let snapshot_payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        yield Ok(Event::default().event("task_event").data(snapshot_payload));

        while let Ok(evt) = rx.recv().await {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            yield Ok(Event::default().event("task_event").data(payload));
            if evt.status == "completed" || evt.status == "failed" || evt.status == "clarification_needed" {
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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RouteRequest {
    pub question: String,
    pub data_source_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteResponse {
    pub routed: bool,
    pub result: Option<RouteResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteResult {
    pub data_source_id: String,
    pub confidence: f32,
    pub method: String,
    pub matched_tables: Vec<MatchedTableInfo>,
    /// Present when the routing LLM detected ambiguity and returned a clarification question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MatchedTableInfo {
    pub data_source_id: String,
    pub table_name: String,
    pub best_column: String,
    /// Semantic description of the best-matching column (AI + user combined).
    pub column_description: String,
    /// Final fused similarity score [0, 1].
    pub similarity_score: f32,
}

// ── Semantics Management ────────────────────────────────────────────────────

/// POST /api/v1/nl2sql/semantics/:datasource_id/refresh — regenerate all embeddings for a datasource.
async fn refresh_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<RefreshSemanticsResponse>> {
    // Validate access
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }
    require_admin(&claims)?;

    // Resolve per-tenant embedding config from api_keys, fall back to env vars.
    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_model_for_usage = embed_cfg
        .as_ref()
        .map(|cfg| cfg.model.clone())
        .unwrap_or_else(|| "text-embedding-3-small".to_string());
    let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;

    // Resolve per-tenant chat LLM config (DB key with failover, then env fallback).
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let result = describer
        .refresh_datasource(&claims.tenant_id, &datasource_id)
        .await
        .map_err(|e| AppError::Internal(format!("refresh failed: {e}")))?;
    persist_embedding_usage(
        state.usage_writer.clone(),
        &claims.tenant_id,
        &claims.sub,
        &datasource_id,
        None,
        &embed_model_for_usage,
        embed_api_key_for_usage,
        aggregate_embedding_usage(&result.embedding_usage),
    )
    .await;

    Ok(Json(RefreshSemanticsResponse {
        tables_processed: result.tables_processed,
        columns_processed: result.columns_processed,
        failed_tables: result.failed_tables,
    }))
}

/// Request body is optional. Supply `{ "tables": ["t1", "t2"] }` to refresh
/// only a subset — used by the frontend's "retry failed tables" button.
pub(crate) async fn refresh_semantics_async(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    body: Option<Json<RefreshAsyncRequest>>,
) -> Result<Json<RefreshTaskCreatedResponse>> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    // Validate access
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }
    require_admin(&claims)?;

    // Check for an in-flight refresh on this datasource before enqueuing
    // a new task. Two concurrent `refresh-async` calls on the same ds
    // would race to upsert into `nl2sql_*_semantics` and produce
    // duplicated LLM calls with an indeterminate final state.
    let in_flight: Option<String> = sqlx::query_scalar(
        "SELECT task_id FROM nl2sql_refresh_tasks \
         WHERE datasource_id = ? AND status IN ('pending', 'running') \
         LIMIT 1",
    )
    .bind(&datasource_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(existing) = in_flight {
        return Err(AppError::Conflict(format!(
            "a refresh is already in progress for this data source (task_id={existing})"
        )));
    }

    let task_id = uuid::Uuid::new_v4().to_string();

    // Count total tables to set `total_tables` in the task record.
    // Manual tables are excluded because `refresh_datasource` skips them,
    // so counting them would make the progress percentage plateau before
    // reaching 100%.
    let total_tables: i32 = {
        let schema_info: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT schema_info FROM data_sources WHERE id = ?")
                .bind(&datasource_id)
                .fetch_optional(&state.db)
                .await?
                .flatten();
        let count = schema_info
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|t| {
                        !t.get("is_manual")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        i32::try_from(count).unwrap_or(i32::MAX)
    };

    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks \
         (task_id, tenant_id, trigger_source, datasource_id, status, total_tables) \
         VALUES (?, ?, 'user', ?, 'pending', ?)",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(total_tables)
    .execute(&state.db)
    .await?;

    // Clone everything needed for the background task
    let tenant_id = claims.tenant_id.clone();
    let datasource_id_inner = datasource_id.clone();
    let task_id_inner = task_id.clone();
    let db = state.db.clone();
    let embed_store = state.nl2sql_embedding_store.clone();
    let default_model = state.default_model.clone();
    let config_registry = state.config_registry.clone();
    let usage_writer = state.usage_writer.clone();
    let user_id = claims.sub.clone();
    let only_tables = req.tables.clone();

    tokio::spawn(async move {
        // Take the per-datasource advisory lock to serialise against the
        // periodic scheduler. If we can't get it quickly, surface a
        // timeout so the task row doesn't stay "pending" forever when the
        // scheduler's cycle is unexpectedly long-running.
        let lock_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let _lock = loop {
            match crate::nl2sql::refresh_lock::RefreshLock::try_acquire(&db, &datasource_id_inner)
                .await
            {
                Ok(Some(guard)) => break guard,
                Ok(None) => {
                    if std::time::Instant::now() >= lock_deadline {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = 'timed out waiting for another refresh to finish (60s); try again later' \
                             WHERE task_id = ?",
                        )
                        .bind(&task_id_inner)
                        .execute(&db)
                        .await;
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    let _ = sqlx::query(
                        "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                         error_message = ? WHERE task_id = ?",
                    )
                    .bind(format!("failed to acquire refresh lock: {e}"))
                    .bind(&task_id_inner)
                    .execute(&db)
                    .await;
                    return;
                }
            }
        };

        // Mark task as running
        let _ = sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'running' WHERE task_id = ?")
            .bind(&task_id_inner)
            .execute(&db)
            .await;

        let embed_cfg =
            crate::nl2sql::resolve_embedding_config(&db, &tenant_id, Some("nl2sql")).await;
        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());
        let embed_store = match embed_store {
            Some(s) => s,
            None => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'embedding store not initialized' WHERE task_id = ?",
                )
                .bind(&task_id_inner)
                .execute(&db)
                .await;
                return;
            }
        };

        let chat_cfg = match config_registry.as_ref() {
            Some(registry) => {
                match crate::nl2sql::resolve_chat_config(
                    registry,
                    &tenant_id,
                    &tenant_id,
                    &default_model,
                    Some("nl2sql"),
                )
                .await
                {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        let _ = sqlx::query(
                            "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                             error_message = ? WHERE task_id = ?",
                        )
                        .bind(format!("failed to resolve chat config: {}", e))
                        .bind(&task_id_inner)
                        .execute(&db)
                        .await;
                        return;
                    }
                }
            }
            None => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'config registry not available' WHERE task_id = ?",
                )
                .bind(&task_id_inner)
                .execute(&db)
                .await;
                return;
            }
        };

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            std::sync::Arc::clone(&embed_store),
            embed_cfg,
            Some(chat_cfg),
        );

        // Push live progress into `nl2sql_refresh_tasks` as tables finish.
        struct DbProgress {
            db: sqlx::SqlitePool,
            task_id: String,
        }
        #[async_trait::async_trait]
        impl crate::nl2sql::schema_describer::ProgressReporter for DbProgress {
            async fn report(&self, percent: u32, processed_tables: u32) {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET progress = ?, processed_tables = ? WHERE task_id = ?",
                )
                .bind(percent)
                .bind(processed_tables)
                .bind(&self.task_id)
                .execute(&self.db)
                .await;
            }
        }

        let reporter = DbProgress {
            db: db.clone(),
            task_id: task_id_inner.clone(),
        };

        let result = match only_tables {
            Some(ref list) if !list.is_empty() => {
                describer
                    .refresh_tables(&tenant_id, &datasource_id_inner, list, reporter)
                    .await
            }
            _ => {
                describer
                    .refresh_datasource_with_progress(&tenant_id, &datasource_id_inner, reporter)
                    .await
            }
        };

        match result {
            Ok(r) => {
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant_id,
                    &user_id,
                    &datasource_id_inner,
                    Some(&task_id_inner),
                    &embed_model_for_usage,
                    embed_api_key_for_usage,
                    aggregate_embedding_usage(&r.embedding_usage),
                )
                .await;
                // Partial-success policy: if every table failed we mark
                // the task failed, otherwise we record it as completed
                // and attach the failed table list for operators to act on.
                let failed_json = if r.failed_tables.is_empty() {
                    None
                } else {
                    serde_json::to_value(
                        r.failed_tables
                            .iter()
                            .map(|(name, err)| serde_json::json!({ "table": name, "error": err }))
                            .collect::<Vec<_>>(),
                    )
                    .ok()
                };

                let all_failed = r.tables_processed == 0 && !r.failed_tables.is_empty();
                let status = if all_failed { "failed" } else { "completed" };

                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks \
                     SET status = ?, progress = 100, \
                         processed_tables = ?, failed_tables = ?, \
                         completed_at = CURRENT_TIMESTAMP \
                     WHERE task_id = ?",
                )
                .bind(status)
                .bind(i32::try_from(r.tables_processed).unwrap_or(i32::MAX))
                .bind(failed_json)
                .bind(&task_id_inner)
                .execute(&db)
                .await;
            }
            Err(e) => {
                let _ = sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = ?, completed_at = CURRENT_TIMESTAMP WHERE task_id = ?",
                )
                .bind(e.to_string())
                .bind(&task_id_inner)
                .execute(&db)
                .await;
            }
        }
    });

    Ok(Json(RefreshTaskCreatedResponse {
        task_id,
        status: "pending".to_string(),
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskCreatedResponse {
    pub task_id: String,
    pub status: String,
}

/// Body for `POST /nl2sql/semantics/:id/refresh-async`. All fields optional:
/// an empty body refreshes the whole datasource.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RefreshAsyncRequest {
    /// When set, only these tables are re-indexed. Used by the frontend's
    /// "retry failed tables" workflow.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

/// GET /api/v1/nl2sql/semantics-tasks/:task_id
/// Returns the current status and progress of an async refresh task.
async fn get_refresh_task_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(task_id): Path<String>,
) -> Result<Json<RefreshTaskStatusResponse>> {
    type RefreshTaskRow = (
        String,
        String,
        String,
        i64,
        i64,
        Option<String>,
        Option<serde_json::Value>,
        Option<String>,
    );
    let row: Option<RefreshTaskRow> = sqlx::query_as(
        "SELECT task_id, datasource_id, status, \
         CAST(progress AS INTEGER) AS progress, \
         CAST(processed_tables AS INTEGER) AS processed_tables, \
         error_message, failed_tables, \
         strftime('%Y-%m-%dT%H:%M:%SZ', completed_at) AS completed_at \
         FROM nl2sql_refresh_tasks \
         WHERE task_id = ? AND tenant_id = ?",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some((
            task_id,
            datasource_id,
            status,
            progress,
            processed_tables,
            error_message,
            failed_tables,
            completed_at,
        )) => {
            let progress = u32::try_from(progress).unwrap_or(u32::MAX);
            let processed_tables = u32::try_from(processed_tables).unwrap_or(u32::MAX);
            Ok(Json(RefreshTaskStatusResponse {
                task_id,
                datasource_id,
                status,
                progress,
                processed_tables,
                error_message,
                failed_tables,
                completed_at,
            }))
        }
        None => Err(AppError::NotFound("refresh task not found".into())),
    }
}

/// P2-2: POST /api/v1/nl2sql/datasource/{id}/reindex
/// Manually triggers a full re-index of a datasource's embedding vectors.
/// Clears the embedding store and re-runs the full refresh cycle.
#[derive(Debug, Deserialize)]
pub(crate) struct ReindexRequest {
    /// Optional: switch to a different embedding model during re-index.
    /// If omitted, re-index uses the currently configured model.
    pub new_model: Option<String>,
}

async fn reindex_datasource(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<ReindexRequest>,
) -> Result<Json<ReindexResponse>> {
    // Validate access
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let _embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::ValidationError("embedding store not initialized".to_string()))?;

    // Keep the active profile intact while the replacement is built. Profile
    // activation happens only after the complete replacement index is ready.

    // Step 1: Update the tracking columns
    if let Some(ref new_model) = req.new_model {
        sqlx::query(
            "UPDATE data_sources SET embedding_model = ?, embedding_dimensions = ?, embedding_needs_reindex = 0 WHERE id = ?",
        )
        .bind(new_model)
        .bind(dimensions_for_model(new_model) as i64)
        .bind(&datasource_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    } else {
        sqlx::query("UPDATE data_sources SET embedding_needs_reindex = 0 WHERE id = ?")
            .bind(&datasource_id)
            .execute(&state.db)
            .await
            .ok();
    }

    // Step 3: Clear AI-generated column semantics (user descriptions preserved)
    sqlx::query(
        "UPDATE nl2sql_table_semantics SET semantic_description = '', embedding_model = '', is_indexed = 0 WHERE datasource_id = ?",
    )
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    sqlx::query(
        "UPDATE nl2sql_table_desc_semantics SET ai_description = '', embedding_model = '' WHERE datasource_id = ?",
    )
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    // Step 4: Trigger full schema refresh
    let task_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO nl2sql_refresh_tasks (task_id, tenant_id, datasource_id, status, progress) VALUES (?, ?, ?, 'pending', 0)",
    )
    .bind(&task_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await
    .ok();

    // Spawn background refresh task
    let db = state.db.clone();
    let config_registry = state.config_registry.clone();
    let default_model = state.default_model.clone();
    let embed_store = state.nl2sql_embedding_store.clone();
    let tenant_id = claims.tenant_id.clone();
    let user_id = claims.sub.clone();
    let datasource_id2 = datasource_id.clone();
    let task_id2 = task_id.clone();
    let usage_writer = state.usage_writer.clone();

    tokio::spawn(async move {
        let embed_cfg =
            crate::nl2sql::resolve_embedding_config(&db, &tenant_id, Some("nl2sql")).await;
        let embed_model_for_usage = embed_cfg
            .as_ref()
            .map(|cfg| cfg.model.clone())
            .unwrap_or_else(|| "text-embedding-3-small".to_string());
        let embed_api_key_for_usage = embed_cfg.as_ref().and_then(|cfg| cfg.key_id.clone());

        let chat_cfg = match config_registry.as_ref() {
            Some(registry) => match crate::nl2sql::resolve_chat_config(
                registry.as_ref(),
                &tenant_id,
                &tenant_id,
                &default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("reindex: failed to resolve chat config: {}", e);
                    sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = ? WHERE task_id = ?")
                        .bind(e.to_string())
                        .bind(&task_id2)
                        .execute(&db)
                        .await
                        .ok();
                    return;
                }
            },
            None => {
                tracing::error!("reindex: config registry not available");
                sqlx::query(
                    "UPDATE nl2sql_refresh_tasks SET status = 'failed', \
                     error_message = 'config registry not available' WHERE task_id = ?",
                )
                .bind(&task_id2)
                .execute(&db)
                .await
                .ok();
                return;
            }
        };

        let store = match embed_store.as_ref() {
            Some(s) => std::sync::Arc::clone(s),
            None => {
                tracing::error!("reindex: embedding store not available");
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = 'embedding store not initialized' WHERE task_id = ?")
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
                return;
            }
        };

        let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
            db.clone(),
            store,
            embed_cfg,
            Some(chat_cfg),
        );

        let result = describer
            .refresh_datasource(&tenant_id, &datasource_id2)
            .await;
        match result {
            Ok(r) => {
                persist_embedding_usage(
                    usage_writer.clone(),
                    &tenant_id,
                    &user_id,
                    &datasource_id2,
                    Some(&task_id2),
                    &embed_model_for_usage,
                    embed_api_key_for_usage,
                    aggregate_embedding_usage(&r.embedding_usage),
                )
                .await;
                tracing::info!(
                    "reindex completed: {} tables, {} columns",
                    r.tables_processed,
                    r.columns_processed
                );
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'completed', progress = 100, processed_tables = ? WHERE task_id = ?")
                    .bind(r.tables_processed as i32)
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
            }
            Err(e) => {
                tracing::error!("reindex failed: {}", e);
                sqlx::query("UPDATE nl2sql_refresh_tasks SET status = 'failed', error_message = ? WHERE task_id = ?")
                    .bind(e.to_string())
                    .bind(&task_id2)
                    .execute(&db)
                    .await
                    .ok();
            }
        }
    });

    Ok(Json(ReindexResponse {
        status: "reindexing".to_string(),
        task_id: Some(task_id),
        message: "Re-index started. Use the task endpoint to monitor progress.".to_string(),
    }))
}

/// Returns the expected embedding dimensions for a model name.
fn dimensions_for_model(model: &str) -> usize {
    nl2sql_domain::config::dimensions_for_model(model)
}

/// GET /api/v1/nl2sql/embedding/config — returns current embedding configuration for the tenant.
async fn get_embedding_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<EmbeddingConfigResponse>> {
    let cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    match cfg {
        Some(cfg) => Ok(Json(EmbeddingConfigResponse {
            available: true,
            model: Some(cfg.model),
            base_url: cfg.base_url,
            configured_via: cfg.configured_via,
            dimensions: cfg.dimensions,
            api_configured: cfg.profile_kind == crate::nl2sql::EmbeddingProfileKind::Api,
            local_model: crate::nl2sql::LOCAL_EMBEDDING_MODEL.to_string(),
            profiles: Vec::new(),
        })),
        None => Ok(Json(EmbeddingConfigResponse {
            available: false,
            model: None,
            base_url: None,
            configured_via: "none",
            dimensions: None,
            api_configured: false,
            local_model: crate::nl2sql::LOCAL_EMBEDDING_MODEL.to_string(),
            profiles: Vec::new(),
        })),
    }
}

/// PATCH /api/v1/nl2sql/semantics/:datasource_id/tables/:table_name
async fn update_table_semantics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, table_name)): Path<(String, String)>,
    Json(req): Json<UpdateTableSemanticsRequest>,
) -> Result<Json<UpdateSemanticsResponse>> {
    let ds_tenant: Option<String> = sqlx::query("SELECT tenant_id FROM data_sources WHERE id = ?")
        .bind(&datasource_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("tenant_id"));

    if ds_tenant.as_ref() != Some(&claims.tenant_id) {
        return Err(AppError::Forbidden);
    }
    require_admin(&claims)?;

    let embed_cfg =
        crate::nl2sql::resolve_embedding_config(&state.db, &claims.tenant_id, Some("nl2sql")).await;
    let embed_store = state
        .nl2sql_embedding_store
        .as_ref()
        .ok_or_else(|| AppError::Internal("embedding store not initialized".into()))?;
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(e))?;

    let describer = crate::nl2sql::schema_describer::SchemaDescriber::new(
        state.db.clone(),
        std::sync::Arc::clone(embed_store),
        embed_cfg,
        Some(chat_cfg),
    );

    let outcome = describer
        .update_table_description(
            &claims.tenant_id,
            &datasource_id,
            &table_name,
            &req.user_description,
        )
        .await
        .map_err(map_update_err)?;

    Ok(Json(UpdateSemanticsResponse {
        success: true,
        indexed: outcome.indexed,
        index_error: outcome.index_error,
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateDatasourceSemanticsRequest {
    pub user_description: String,
}

// ── Manual Foreign Keys CRUD ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ForeignKeyResponse {
    pub id: String,
    pub datasource_id: String,
    pub source_table: String,
    pub source_column: String,
    pub source_type: String,
    pub target_table: String,
    pub target_column: String,
    pub target_type: String,
    pub updated_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
struct ForeignKeyListResponse {
    pub foreign_keys: Vec<ForeignKeyResponse>,
}

#[derive(Debug, Deserialize)]
struct CreateForeignKeyRequest {
    pub source_table: String,
    pub source_column: String,
    pub source_type: String,
    pub target_table: String,
    pub target_column: String,
    pub target_type: String,
}

/// GET /api/v1/nl2sql/foreign-keys/:datasource_id — list all manual FKs for a datasource.
async fn list_foreign_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
) -> Result<Json<ForeignKeyListResponse>> {
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    >(
        "SELECT id, source_table, source_column, source_type, target_table, \
             target_column, target_type, created_by, updated_by, created_at \
             FROM nl2sql_foreign_keys \
             WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
    )
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .fetch_all(&state.db)
    .await?;

    let foreign_keys: Vec<ForeignKeyResponse> = rows
        .into_iter()
        .map(
            |(
                id,
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
                _created_by,
                updated_by,
                created_at,
            )| ForeignKeyResponse {
                id,
                datasource_id: datasource_id.clone(),
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
                updated_by,
                created_at,
            },
        )
        .collect();

    Ok(Json(ForeignKeyListResponse { foreign_keys }))
}

/// POST /api/v1/nl2sql/foreign-keys/:datasource_id — create a new manual FK definition.
async fn create_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(datasource_id): Path<String>,
    Json(req): Json<CreateForeignKeyRequest>,
) -> Result<Json<ForeignKeyResponse>> {
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO nl2sql_foreign_keys \
         (id, tenant_id, datasource_id, source_table, source_column, source_type, \
          target_table, target_column, target_type, created_by, updated_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .bind(&req.source_table)
    .bind(&req.source_column)
    .bind(&req.source_type)
    .bind(&req.target_table)
    .bind(&req.target_column)
    .bind(&req.target_type)
    .bind(&claims.sub)
    .bind(&claims.sub)
    .execute(&state.db)
    .await?;

    // Fetch the created record to return the full response including created_at.
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT id, source_table, source_column, source_type, target_table, \
             target_column, target_type, created_by, updated_by, created_at \
             FROM nl2sql_foreign_keys WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    let (
        id,
        source_table,
        source_column,
        source_type,
        target_table,
        target_column,
        target_type,
        _created_by,
        updated_by,
        created_at,
    ) = row;

    Ok(Json(ForeignKeyResponse {
        id,
        datasource_id,
        source_table,
        source_column,
        source_type,
        target_table,
        target_column,
        target_type,
        updated_by,
        created_at,
    }))
}

/// PATCH /api/v1/nl2sql/foreign-keys/:datasource_id/:fk_id — update a manual FK.
#[derive(Debug, Deserialize)]
struct UpdateForeignKeyRequest {
    pub source_table: Option<String>,
    pub source_column: Option<String>,
    pub source_type: Option<String>,
    pub target_table: Option<String>,
    pub target_column: Option<String>,
    pub target_type: Option<String>,
}

async fn update_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, fk_id)): Path<(String, String)>,
    Json(req): Json<UpdateForeignKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let _ = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let mut query = String::from(
        "UPDATE nl2sql_foreign_keys SET updated_at = CURRENT_TIMESTAMP, updated_by = ?",
    );
    let mut bindings: Vec<String> = vec![claims.sub.clone()];

    if let Some(ref v) = req.source_table {
        query.push_str(", source_table = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.source_column {
        query.push_str(", source_column = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.source_type {
        query.push_str(", source_type = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_table {
        query.push_str(", target_table = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_column {
        query.push_str(", target_column = ?");
        bindings.push(v.clone());
    }
    if let Some(ref v) = req.target_type {
        query.push_str(", target_type = ?");
        bindings.push(v.clone());
    }

    query.push_str(" WHERE id = ? AND tenant_id = ? AND datasource_id = ?");

    let mut q = sqlx::query(sqlx::AssertSqlSafe(query));
    for binding in &bindings {
        q = q.bind(binding);
    }
    q = q.bind(&fk_id).bind(&claims.tenant_id).bind(&datasource_id);

    let result = q.execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Foreign key not found".into()));
    }

    Ok(Json(
        serde_json::json!({ "id": fk_id, "updated_by": &claims.sub }),
    ))
}

/// DELETE /api/v1/nl2sql/foreign-keys/:datasource_id/:fk_id — delete a manual FK.
async fn delete_foreign_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((datasource_id, fk_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    // Validate access and ensure the datasource belongs to the tenant.
    let _db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &datasource_id,
    )
    .await?;

    let result = sqlx::query(
        "DELETE FROM nl2sql_foreign_keys \
         WHERE id = ? AND tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(&fk_id)
    .bind(&claims.tenant_id)
    .bind(&datasource_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("foreign key not found".into()));
    }

    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// Generate a natural language explanation of the given SQL query.
/// The `language` parameter controls the output language ("zh-CN" or "en-US").
pub(crate) async fn explain_sql(
    state: &AppState,
    claims: &Claims,
    sql: &str,
    schema: &serde_json::Value,
    language: &str,
) -> anyhow::Result<String> {
    let chat_cfg = crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to resolve chat config: {}", e))?;

    let (system_prompt, lang_label) = if language == "zh-CN" {
        ("你是一位 SQL 专家。根据给定的数据库 schema 和 SQL 查询，用 1-2 句话简洁地解释这条查询的作用。直接返回解释文字，不需要任何格式或标记。".to_string(), "中文")
    } else {
        ("You are a SQL expert. Given a database schema and SQL query, explain what the query does in 1-2 sentences. Return your explanation as plain text with no formatting.".to_string(), "English")
    };

    #[derive(serde::Serialize)]
    struct ExplainSqlPrompt {
        schema_json: String,
        sql: String,
        language: String,
    }
    let prompt_json = serde_json::to_string(&ExplainSqlPrompt {
        schema_json: serde_json::to_string_pretty(schema).unwrap_or_default(),
        sql: sql.to_string(),
        language: lang_label.to_string(),
    })
    .map_err(|e| anyhow::anyhow!("failed to serialize prompt: {}", e))?;

    let request = MessageRequest {
        model: chat_cfg.model,
        max_tokens: 256,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt_json }],
        }],
        system: Some(system_prompt),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.3),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = chat_cfg
        .client
        .send_message(&request)
        .await
        .map_err(|e| anyhow::anyhow!("LLM explanation call failed: {}", e))?;

    let text = response
        .content
        .iter()
        .find_map(|b| match b {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(text.trim().to_string())
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn clarify(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ClarifyRequest>,
) -> Result<Json<ClarifyResponse>> {
    super::query_async::emit_stage("request_validation", "开始校验澄清请求");
    let tenant_id = &claims.tenant_id;
    let user_id = &claims.sub;
    let session_id = &req.session_id;
    let ts = now_ms();
    let mut applied_rules: Vec<super::AppliedRuleHit> = Vec::new();

    let ClarifyRequest {
        session_id: _,
        conversation_id,
        question: _,
        clarification_context,
        selected_option,
        free_text,
        route_confidence,
        routing_method,
        semantic_context,
        ..
    } = &req;
    let mut route_confidence = route_confidence.map(|c| c.clamp(0.0, 1.0));
    let mut routing_method = routing_method
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let mut semantic_context = semantic_context.clone();

    let user_input = selected_option
        .as_ref()
        .and_then(|opt| {
            clarification_context
                .as_ref()
                .and_then(|ctx| ctx.options.get(opt.option_index))
                .map(|c| format!("选择候选：{}.{}", c.table_name, c.column_name))
        })
        .or_else(|| free_text.as_ref().map(|t| t.trim().to_string()))
        .unwrap_or_default();

    let (data_source_id, question_text) = if let Some(opt) = selected_option {
        let ctx = clarification_context.as_ref().ok_or_else(|| {
            AppError::ValidationError(
                "clarification_context is required when selecting an option".into(),
            )
        })?;
        let chosen = ctx.options.get(opt.option_index).ok_or_else(|| {
            AppError::ValidationError(format!("invalid option index: {}", opt.option_index))
        })?;
        let enriched_question = format!(
            "{} (use table: {})",
            ctx.original_question, chosen.table_name
        );
        (chosen.data_source_id.clone(), enriched_question)
    } else if let Some(text) = free_text {
        if text.trim().is_empty() {
            return Err(AppError::ValidationError(
                "free_text cannot be empty".into(),
            ));
        }
        let ctx = clarification_context.as_ref().ok_or_else(|| {
            AppError::ValidationError(
                "clarification_context is required when providing free_text".into(),
            )
        })?;
        // Keep the original intent + user补充条件 together; otherwise a short reply
        // like "最近7天" is treated as a brand new question and loses context.
        let enriched_question = format!("{}\n补充条件：{}", ctx.original_question, text.trim());

        let reroute = route(
            State(state.clone()),
            Extension(claims.clone()),
            Json(RouteRequest {
                question: enriched_question.clone(),
                data_source_id: None,
            }),
        )
        .await;

        match reroute {
            Ok(Json(RouteResponse {
                routed: true,
                result: Some(result),
                error: _,
            })) if !result.data_source_id.trim().is_empty()
                && result.data_source_id != "multi-datasource" =>
            {
                tracing::info!(
                    tenant_id,
                    user_id,
                    session_id,
                    original_question_chars = ctx.original_question.chars().count(),
                    free_text_chars = text.trim().chars().count(),
                    datasource_id = %result.data_source_id,
                    confidence = result.confidence,
                    method = %result.method,
                    option_count = ctx.options.len(),
                    "clarify free_text rerouted datasource"
                );
                route_confidence = Some(result.confidence.clamp(0.0, 1.0));
                routing_method = Some(format!("clarification_free_text:{}", result.method));
                semantic_context = serde_json::to_value(&result).ok();
                (result.data_source_id, enriched_question)
            }
            Ok(Json(RouteResponse {
                result: Some(result),
                error,
                ..
            })) => {
                let mut history = ctx.clarification_history.clone();
                history.push(crate::nl2sql::ClarificationHistoryItem {
                    round: ctx.turn,
                    user_input: text.trim().to_string(),
                    missing_after: None,
                });
                let mut options: Vec<crate::nl2sql::ClarificationOption> = result
                    .matched_tables
                    .into_iter()
                    .enumerate()
                    .take(5)
                    .map(|(idx, table)| crate::nl2sql::ClarificationOption {
                        option_index: idx,
                        data_source_id: table.data_source_id,
                        table_name: table.table_name,
                        column_name: table.best_column,
                        reason: "根据补充内容重新检索后的候选".to_string(),
                        sim_score: table.similarity_score,
                        business_meaning: table.column_description,
                    })
                    .collect();
                if options.is_empty() {
                    options = ctx.options.clone();
                }
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question: result.clarification_question.unwrap_or_else(|| {
                        error.unwrap_or_else(|| {
                            "补充后仍未能确定唯一数据源，请选择最匹配的数据源或继续补充。"
                                .to_string()
                        })
                    }),
                    options,
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: history,
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
            Ok(Json(RouteResponse { error, .. })) => {
                let mut history = ctx.clarification_history.clone();
                history.push(crate::nl2sql::ClarificationHistoryItem {
                    round: ctx.turn,
                    user_input: text.trim().to_string(),
                    missing_after: None,
                });
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question: error.unwrap_or_else(|| {
                        "补充后仍未能确定唯一数据源，请选择最匹配的数据源或继续补充。".to_string()
                    }),
                    options: ctx.options.clone(),
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: history,
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    tenant_id,
                    user_id,
                    session_id,
                    original_question_chars = ctx.original_question.chars().count(),
                    free_text_chars = text.trim().chars().count(),
                    error = %e,
                    "clarify free_text reroute failed"
                );
                let mut history = ctx.clarification_history.clone();
                history.push(crate::nl2sql::ClarificationHistoryItem {
                    round: ctx.turn,
                    user_input: text.trim().to_string(),
                    missing_after: None,
                });
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: enriched_question.clone(),
                    clarification_question:
                        "补充后仍未能可靠确定数据源，请选择最匹配的数据源或继续补充。".to_string(),
                    options: ctx.options.clone(),
                    confirmed_requirements: ctx.confirmed_requirements.clone(),
                    missing_requirements: ctx.missing_requirements.clone(),
                    missing_requirement_reasons: ctx.missing_requirement_reasons.clone(),
                    clarification_history: history,
                    turn: ctx.turn.saturating_add(1),
                    conversation_id: ctx.conversation_id.clone(),
                };
                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
        }
    } else {
        return Err(AppError::ValidationError(
            "either selected_option or free_text must be provided".into(),
        ));
    };

    // Soft-limit strategy:
    // Allow up to MAX_CLARIFICATION_TURNS user补充轮次.
    // On round N+1, don't reject; apply default grain and continue generation.
    let max_turns = crate::nl2sql::max_clarification_turns();
    let soft_fallback_applied = clarification_context
        .as_ref()
        .map(|ctx| ctx.turn > max_turns)
        .unwrap_or(false);

    if let Some(ctx) = clarification_context {
        let ctx_record = super::chat::SessionMessageRecord {
            role: "clarification".to_string(),
            content: serde_json::json!(ctx),
            timestamp_ms: ts,
        };
        // B-01: Log (non-fatal) if session persistence fails.
        if let Err(e) = super::chat::append_message(
            &state.data_dir,
            tenant_id,
            user_id,
            session_id,
            &ctx_record,
        ) {
            tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist clarification context to session");
        }
    }

    let conv_id = conversation_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some(ctx) = clarification_context {
        if !user_input.is_empty() {
            if let Err(e) = persist_clarification_message(
                &state.db,
                tenant_id,
                user_id,
                &conv_id,
                session_id,
                ctx.turn,
                &ctx.original_question,
                &ctx.clarification_question,
                &user_input,
                &ctx.confirmed_requirements,
                &ctx.missing_requirements,
            )
            .await
            {
                tracing::warn!(
                    error = %e,
                    tenant_id,
                    user_id,
                    session_id,
                    conversation_id = %conv_id,
                    "failed to persist clarification message to db"
                );
            }
        }
    }

    let db_type =
        validate_data_source_access(&state, tenant_id, user_id, &claims.role, &data_source_id)
            .await?;
    super::query_async::emit_stage("request_validation", "澄清请求校验通过");

    if !matches!(
        db_type.as_str(),
        "mysql" | "tidb" | "postgres" | "clickhouse" | "presto" | "trino" | "mongodb"
    ) {
        return Err(AppError::ValidationError(format!(
            "NL2SQL is not supported for db_type: {db_type}. \
             Pick a supported data source (mysql, tidb, postgres, clickhouse, presto, trino, mongodb)."
        )));
    }

    let schema_info: serde_json::Value = {
        let row = sqlx::query("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(&data_source_id)
            .fetch_optional(&state.db)
            .await?;
        match row {
            Some(r) => r
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or(serde_json::json!({"tables": [], "foreign_keys": []})),
            None => return Err(AppError::NotFound("data source not found".into())),
        }
    };

    let (schema_tables, foreign_keys) = extract_schema_tables_and_fks(&schema_info);
    super::query_async::emit_stage("load_schema", "Schema 加载完成");
    let mut foreign_key_prompts: Vec<ForeignKeyPrompt> = foreign_keys
        .into_iter()
        .map(|fk| ForeignKeyPrompt {
            source_table: fk.source_table,
            source_column: fk.source_column,
            source_type: fk.source_column_type,
            target_table: fk.target_table,
            target_column: fk.target_column,
            target_type: fk.target_column_type,
        })
        .collect();

    let manual_fks = load_manual_foreign_keys(&state.db, tenant_id, &data_source_id).await;
    let manual_fk_count = manual_fks.len();
    foreign_key_prompts.extend(manual_fks);
    if manual_fk_count > 0 {
        super::push_rule_hit(
            &mut applied_rules,
            "manual_foreign_keys_loaded",
            "Manual Foreign Keys",
            Some(format!("{manual_fk_count} user-defined FK(s) loaded")),
        );
    }

    let history = load_conversation_history(&state.db, tenant_id, &conv_id, 8).await;
    super::query_async::emit_stage("load_context", "上下文加载完成");

    // A clarification continuation must preserve the same business-domain
    // contract as the initial query. Domain labels select schema; they are not
    // entity values and must never become invented row predicates.
    let datasource_domains =
        match crate::nl2sql::routing::resolve_business_domains(&state.db, Some(&data_source_id))
            .await
        {
            Ok(domains) => domains,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    datasource_id = %data_source_id,
                    "failed to resolve business domains for clarification"
                );
                Vec::new()
            }
        };
    let matched_business_domain = super::business_domain_context_for_question(
        &datasource_domains,
        &data_source_id,
        &question_text,
    );
    let business_domain_context = matched_business_domain
        .as_ref()
        .map(super::BusinessDomainQuestionContext::system_prompt);
    let semantic_question = matched_business_domain.as_ref().map_or_else(
        || question_text.clone(),
        |context| context.semantic_question(&question_text),
    );
    let mut schema_tables = schema_tables;
    if let Some(domain_match) = matched_business_domain.as_ref() {
        super::push_rule_hit(
            &mut applied_rules,
            "business_domain_resolved",
            "Business Domain Resolution",
            Some(format!(
                "matched domains: {}; mapped tables: {}",
                domain_match.matched_domains.join(", "),
                domain_match.mapped_tables.join(", ")
            )),
        );
    }
    if !datasource_domains.is_empty() {
        let strict_match = super::strict_domain_tables_for_question(
            &datasource_domains,
            &data_source_id,
            &question_text,
        );
        if !strict_match.allowed_tables.is_empty() {
            let before_count = schema_tables
                .as_array()
                .map(|tables| tables.len())
                .unwrap_or(0);
            schema_tables = super::filter_schema_tables_by_allowlist(
                &schema_tables,
                &strict_match.allowed_tables,
            );
            let after_count = schema_tables
                .as_array()
                .map(|tables| tables.len())
                .unwrap_or(0);
            super::push_rule_hit(
                &mut applied_rules,
                "strict_domain_filter",
                "Strict Business Domain Filter",
                Some(format!(
                    "domains={}; restricted schema tables: {} -> {}",
                    strict_match.matched_domains.join(","),
                    before_count,
                    after_count
                )),
            );
        }
    }

    // ── Query Understanding for agent execute ───────────────────────────────────
    let mut qu_result: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult> =
        if should_enable_qu() {
            super::query_async::emit_stage("query_understanding", "正在做意图澄清");
            let chat_cfg = match crate::nl2sql::resolve_chat_config(
                state.config_registry(),
                tenant_id,
                user_id,
                &state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "agent_execute QU: failed to resolve chat config, skipping");
                    None
                }
            };
            if let Some(cfg) = chat_cfg {
                let qu = crate::nl2sql::query_understanding::QueryUnderstanding::new(
                    state.db.clone(),
                    cfg,
                );
                let schema_for_qu = serde_json::json!(schema_tables);
                match qu
                    .understand_with_context(
                        &semantic_question,
                        &data_source_id,
                        tenant_id,
                        &schema_for_qu,
                        &history.messages,
                    )
                    .await
                {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "agent_execute QU: understand() failed, skipping");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
    if let (Some(qu), Some(domain_match)) = (qu_result.as_mut(), matched_business_domain.as_ref()) {
        let removed = super::remove_business_domain_derived_filters(qu, domain_match);
        if removed > 0 {
            super::push_rule_hit(
                &mut applied_rules,
                "business_domain_filter_sanitized",
                "Business Domain Query Understanding Guard",
                Some(format!(
                    "removed {removed} filter(s) derived from routing labels"
                )),
            );
        }
    }
    if let Some(qu) = qu_result.as_ref() {
        super::push_rule_hit(
            &mut applied_rules,
            "query_understanding",
            "Query Understanding",
            Some(format!(
                "intent={}, confidence={:.2}",
                qu.intent, qu.confidence
            )),
        );
        if let Some(time) = qu.entities.time.as_ref() {
            super::push_rule_hit(
                &mut applied_rules,
                "time_pattern_resolved",
                "Time Pattern Resolution",
                Some(format!(
                    "type={}, granularity={}, ranges={}",
                    time.resolved_type,
                    time.granularity,
                    time.ranges.len()
                )),
            );
        }
    } else if should_enable_qu() {
        super::push_rule_hit(
            &mut applied_rules,
            "query_understanding_enabled",
            "Query Understanding",
            Some("enabled".to_string()),
        );
    }
    super::query_async::emit_stage("query_understanding", "意图澄清完成");

    // Load pre-computed JOIN paths for multi-table queries.
    let join_paths = load_join_paths_for_datasource(&state.db, &data_source_id).await;
    if !join_paths.is_empty() {
        super::push_rule_hit(
            &mut applied_rules,
            "join_paths_loaded",
            "Join Path Modeling",
            Some(format!("{} join path(s) available", join_paths.len())),
        );
    }

    // P1-2: Load business metrics for SQL generation prompt injection.
    let metric_candidates: Vec<MetricMatchCandidate> = {
        let rows: Vec<(
            String,
            Option<serde_json::Value>,
            Option<String>,
            Option<serde_json::Value>,
        )> = sqlx::query_as(
            "SELECT metric_name, metric_aliases, expression, filter_conditions FROM nl2sql_metrics \
             WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(&data_source_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        rows.into_iter()
            .map(
                |(name, aliases, expression, filter_conditions)| MetricMatchCandidate {
                    name,
                    aliases: parse_metric_aliases(aliases.as_ref()),
                    expression,
                    filter_conditions,
                },
            )
            .collect()
    };
    let metrics: Vec<(String, String, Option<String>)> = {
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT metric_name, expression, filter_conditions FROM nl2sql_metrics \
             WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(&data_source_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        rows.into_iter()
            .map(|(name, expr, filters)| {
                let filter_str = filters;
                (name, expr, filter_str)
            })
            .collect()
    };
    let matched_metrics = matched_metric_names(&semantic_question, &metric_candidates);
    let metric_hard_constraint =
        resolve_metric_hard_constraint(&matched_metrics, &metric_candidates);
    if !matched_metrics.is_empty() {
        super::push_rule_hit(
            &mut applied_rules,
            "metric_resolved",
            "Metric Resolution",
            Some(format!("matched metrics: {}", matched_metrics.join(", "))),
        );
    }
    let synonym_hits =
        super::detect_synonym_hits(&state.db, tenant_id, &data_source_id, &semantic_question).await;
    if !synonym_hits.is_empty() {
        let preview = synonym_hits
            .iter()
            .take(3)
            .map(|(term, table, col)| format!("{term}->{table}.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        super::push_rule_hit(
            &mut applied_rules,
            "synonym_resolved",
            "Synonym Resolution",
            Some(format!(
                "matched {} synonym(s): {}",
                synonym_hits.len(),
                preview
            )),
        );
    }

    // Stage-3: Clarify branch requirement completeness gate.
    // Even after one clarification answer, if requirements are still incomplete,
    // keep asking instead of guessing SQL.
    super::query_async::emit_stage("clarification_gate", "正在检查是否仍需澄清");
    let requirement_question =
        augment_question_for_metric_hint(&semantic_question, &matched_metrics);
    let req_check = parse_requirements_from_question(
        &requirement_question,
        qu_result.as_ref(),
        &schema_tables,
        &metrics,
    );
    if !req_check.missing.is_empty() && !soft_fallback_applied && matched_business_domain.is_none()
    {
        super::push_rule_hit(
            &mut applied_rules,
            "clarification_required",
            "Requirement Clarification Gate",
            Some(format!(
                "{} unresolved requirement(s)",
                req_check.missing.len()
            )),
        );
        let mut history = clarification_context
            .as_ref()
            .map(|ctx| ctx.clarification_history.clone())
            .unwrap_or_default();
        if !user_input.is_empty() {
            history.push(crate::nl2sql::ClarificationHistoryItem {
                round: clarification_context
                    .as_ref()
                    .map(|ctx| ctx.turn)
                    .unwrap_or(1),
                user_input,
                missing_after: Some(req_check.missing.clone()),
            });
        }

        let next_turn = clarification_context
            .as_ref()
            .map(|ctx| ctx.turn.saturating_add(1))
            .unwrap_or(1);
        let new_ctx = crate::nl2sql::ClarificationContext {
            // Keep a rolling enriched question so the yellow context panel
            // reflects the latest clarified intent, not only the first round.
            original_question: question_text.clone(),
            clarification_question: build_requirement_clarification_question(&req_check.missing),
            options: vec![crate::nl2sql::ClarificationOption {
                option_index: 0,
                data_source_id: data_source_id.clone(),
                table_name: "当前数据源".to_string(),
                column_name: "补充需求".to_string(),
                reason: "仍有关键约束缺失，需要继续澄清".to_string(),
                sim_score: 1.0,
                business_meaning: "请继续补充缺失条件".to_string(),
            }],
            confirmed_requirements: req_check.confirmed.clone(),
            missing_requirements: req_check.missing.clone(),
            missing_requirement_reasons: req_check.missing_reasons.clone(),
            clarification_history: history,
            turn: next_turn,
            conversation_id: conv_id.clone(),
        };

        let ctx_record = super::chat::SessionMessageRecord {
            role: "clarification".to_string(),
            content: serde_json::json!(&new_ctx),
            timestamp_ms: ts + 1,
        };
        if let Err(e) = super::chat::append_message(
            &state.data_dir,
            tenant_id,
            user_id,
            session_id,
            &ctx_record,
        ) {
            tracing::warn!(
                error = %e,
                tenant_id,
                user_id,
                session_id,
                "failed to persist chained clarification context"
            );
        }

        return Ok(Json(ClarifyResponse {
            data: None,
            pending_clarification: Some(new_ctx),
            error: None,
        }));
    }

    let fallback_granularity = crate::nl2sql::soft_fallback_granularity();
    let generation_question = if soft_fallback_applied {
        super::push_rule_hit(
            &mut applied_rules,
            "clarification_soft_fallback",
            "Clarification Soft Fallback",
            Some(format!("default granularity: {fallback_granularity}")),
        );
        format!(
            "{}\n系统兜底：统计粒度默认按{}（{}）。",
            semantic_question,
            match fallback_granularity.as_str() {
                "weekly" => "周",
                "monthly" => "月",
                "quarterly" => "季度",
                "yearly" => "年",
                _ => "天",
            },
            fallback_granularity
        )
    } else {
        semantic_question
    };
    let query_id = uuid::Uuid::new_v4().to_string();
    let evidence_columns = synonym_hits
        .iter()
        .map(|(_, _, column)| column.clone())
        .collect::<Vec<_>>();
    let durable_intent = super::semantic_audit::compile_bind_and_persist_intent(
        &state.db,
        tenant_id,
        &data_source_id,
        &conv_id,
        &query_id,
        &generation_question,
        &matched_metrics,
        &schema_tables,
        &evidence_columns,
        qu_result.as_ref(),
    )
    .await
    .map_err(|error| {
        AppError::Internal(format!(
            "failed to persist bound analytic intent before clarification SQL generation: {error}"
        ))
    })?;
    let semantic_intent_json = durable_intent.intent_json().map_err(|error| {
        AppError::Internal(format!(
            "failed to serialize clarification analytic intent: {error}"
        ))
    })?;
    let planning_start = std::time::Instant::now();
    super::query_async::emit_stage("cache_lookup", "正在检查缓存");
    super::query_async::emit_stage("generate_sql", "正在生成 SQL");
    let sql_result = generate_sql(
        &state,
        &claims,
        Some(&data_source_id),
        &generation_question,
        &schema_tables,
        &foreign_key_prompts,
        &join_paths,
        history,
        req.clarification_context.as_ref(),
        qu_result.as_ref(),
        &db_type,
        false, // P1-4: agent path uses full schema
        &metrics
            .iter()
            .map(|(n, e, f)| (n.clone(), e.clone(), f.as_deref()))
            .collect::<Vec<_>>(),
        &matched_metrics,
        &[],
        business_domain_context.as_deref(),
        None,
        true,
        &semantic_intent_json,
    )
    .await;
    super::query_async::emit_stage("generate_sql", "SQL 生成完成");

    let planning_ms = planning_start.elapsed().as_millis() as i64;
    let (sql, _err): (Option<String>, Option<String>) = match &sql_result {
        Ok(r) => {
            if let (Some(usage), Some(model)) = (r.usage.as_ref(), r.model.as_deref()) {
                super::record_nl2sql_token_usage(
                    &state,
                    &claims,
                    &conv_id,
                    Some(&query_id),
                    usage,
                    model,
                    r.api_key_id.clone(),
                    r.provider.clone(),
                )
                .await;
            }
            // generate_sql can still request another clarification round
            // (CLARIFICATION_NEEDED) even after requirement-gate checks.
            // In that case we must return pending_clarification instead of
            // falling through as an empty SQL response, otherwise frontend
            // loses the active clarification input card.
            if let Some(cq) = &r.clarification_question {
                super::push_rule_hit(
                    &mut applied_rules,
                    "model_clarification_requested",
                    "Model Clarification",
                    Some("model asked for extra constraints".to_string()),
                );
                tracing::info!(
                    tenant_id,
                    user_id,
                    conversation_id = %conv_id,
                    session_id,
                    "clarify: model requested chained clarification, returning pending_clarification"
                );
                let mut history = clarification_context
                    .as_ref()
                    .map(|ctx| ctx.clarification_history.clone())
                    .unwrap_or_default();
                if !user_input.is_empty() {
                    history.push(crate::nl2sql::ClarificationHistoryItem {
                        round: clarification_context
                            .as_ref()
                            .map(|ctx| ctx.turn)
                            .unwrap_or(1),
                        user_input: user_input.clone(),
                        missing_after: None,
                    });
                }

                let next_turn = clarification_context
                    .as_ref()
                    .map(|ctx| ctx.turn.saturating_add(1))
                    .unwrap_or(1);
                let new_ctx = crate::nl2sql::ClarificationContext {
                    original_question: question_text.clone(),
                    clarification_question: cq.clone(),
                    options: vec![crate::nl2sql::ClarificationOption {
                        option_index: 0,
                        data_source_id: data_source_id.clone(),
                        table_name: "当前数据源".to_string(),
                        column_name: "补充需求".to_string(),
                        reason: "仍需进一步澄清才能准确生成 SQL".to_string(),
                        sim_score: 1.0,
                        business_meaning: "请继续补充缺失条件".to_string(),
                    }],
                    confirmed_requirements: req_check.confirmed.clone(),
                    missing_requirements: req_check.missing.clone(),
                    missing_requirement_reasons: req_check.missing_reasons.clone(),
                    clarification_history: history,
                    turn: next_turn,
                    conversation_id: conv_id.clone(),
                };

                let ctx_record = super::chat::SessionMessageRecord {
                    role: "clarification".to_string(),
                    content: serde_json::json!(&new_ctx),
                    timestamp_ms: ts + 1,
                };
                if let Err(e) = super::chat::append_message(
                    &state.data_dir,
                    tenant_id,
                    user_id,
                    session_id,
                    &ctx_record,
                ) {
                    tracing::warn!(
                        error = %e,
                        tenant_id,
                        user_id,
                        session_id,
                        "failed to persist model-driven chained clarification context"
                    );
                }

                return Ok(Json(ClarifyResponse {
                    data: None,
                    pending_clarification: Some(new_ctx),
                    error: None,
                }));
            }
            (Some(r.sql.clone()), None)
        }
        Err(e) => {
            sqlx::query(
                "INSERT INTO nl2sql_queries \
                 (id, tenant_id, user_id, data_source_id, conversation_id, question, \
                  generated_sql, executed, error_message, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&query_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(&data_source_id)
            .bind(&conv_id)
            .bind(&question_text)
            .bind(e.to_string())
            .bind(planning_ms)
            .bind(route_confidence)
            .bind(routing_method.as_deref())
            .bind(semantic_context.clone())
            .bind(super::applied_rules_json_value(&applied_rules))
            .execute(&state.db)
            .await?;

            let summary_version = super::fetch_summary_version_i32(&state.db, &conv_id).await;

            let resp_data = ClarifyResponseData {
                data_source_id: data_source_id.clone(),
                question: question_text.clone(),
                sql: None,
                explanation: None,
                error: Some(e.to_string()),
                query_id: query_id.clone(),
                conversation_id: Some(conv_id.clone()),
                clarification_context: clarification_context.clone(),
                fallback_mode: if soft_fallback_applied {
                    Some(format!("{}_granularity", fallback_granularity))
                } else {
                    None
                },
                summary_version,
                applied_rules: applied_rules.clone(),
            };
            let record = super::chat::SessionMessageRecord {
                role: "assistant".to_string(),
                content: serde_json::json!(&resp_data),
                timestamp_ms: ts + 1,
            };
            // B-01: Log (non-fatal) if session persistence fails.
            if let Err(e) = super::chat::append_message(
                &state.data_dir,
                tenant_id,
                user_id,
                session_id,
                &record,
            ) {
                tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist assistant clarification response to session");
            }
            return Ok(Json(ClarifyResponse {
                data: Some(resp_data),
                pending_clarification: None,
                error: None,
            }));
        }
    };

    let mut sql = if let (Some(raw_sql), Some(constraint)) =
        (sql.as_ref(), metric_hard_constraint.as_ref())
    {
        if let Some(rewritten) = enforce_metric_hard_constraint_sql(raw_sql, constraint) {
            super::push_rule_hit(
                &mut applied_rules,
                "metric_hard_enforced",
                "Metric Hard Constraint",
                Some(format!("enforced metric: {}", constraint.metric_name)),
            );
            Some(rewritten)
        } else {
            tracing::warn!(
                metric_name = %constraint.metric_name,
                "clarify metric hard-constraint rewrite skipped due to parse/shape mismatch"
            );
            Some(raw_sql.clone())
        }
    } else {
        sql
    };

    if let (Some(raw_sql), Some(domain_context)) = (sql.as_ref(), matched_business_domain.as_ref())
    {
        let suspicious = super::business_domain_derived_sql_literals(
            raw_sql,
            &generation_question,
            &domain_context.matched_domains,
        );
        if !suspicious.is_empty() {
            let guard_error = format!(
                "Business-domain labels are routing metadata, but the generated SQL used domain-derived literal predicates: {}. Regenerate without those predicates unless the value is independently present in the user's semantic request.",
                suspicious.join(", ")
            );
            let mut correction_context = super::SelfCorrectContext::default();
            let repaired = super::correct_sql(
                &state,
                &claims,
                raw_sql,
                &guard_error,
                &generation_question,
                &schema_tables,
                &foreign_key_prompts,
                &join_paths,
                &conv_id,
                &mut correction_context,
                req.clarification_context.as_ref(),
                &db_type,
                &data_source_id,
                None,
                false,
            )
            .await;
            let repaired = super::extract_sql_from_llm_output(&repaired);
            let remaining = super::business_domain_derived_sql_literals(
                &repaired,
                &generation_question,
                &domain_context.matched_domains,
            );
            if repaired.is_empty() || !remaining.is_empty() {
                super::push_rule_hit(
                    &mut applied_rules,
                    "business_domain_literal_blocked",
                    "Business Domain Literal Guard",
                    Some(format!(
                        "blocked domain-derived literals after clarification: {}",
                        suspicious.join(", ")
                    )),
                );
                return Err(AppError::ValidationError(
                    "Generated SQL incorrectly used a business-domain label as a row filter. AOS blocked the query instead of executing invented conditions; please retry."
                        .to_string(),
                ));
            }
            sql = Some(repaired);
            super::push_rule_hit(
                &mut applied_rules,
                "business_domain_literal_repaired",
                "Business Domain Literal Guard",
                Some(format!(
                    "removed domain-derived literals after clarification: {}",
                    suspicious.join(", ")
                )),
            );
        }
    }

    let final_sql = sql.as_deref().ok_or_else(|| {
        AppError::ValidationError(
            "clarification completed without a SQL candidate; retry the request".to_string(),
        )
    })?;
    let audit = super::semantic_audit::compile_canonical_intent_with_contracts_and_joins(
        &durable_intent.intent,
        final_sql,
        &durable_intent.metric_contracts,
        &durable_intent.join_contracts,
    )
    .ok_or_else(|| {
        AppError::ValidationError(
            "clarification SQL could not be verified against the canonical analytic intent"
                .to_string(),
        )
    })?;
    let release_decision = serde_json::to_string(&audit.verification.release_decision)
        .unwrap_or_else(|_| "\"NeedsClarification\"".to_string())
        .trim_matches('"')
        .to_string();
    crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
        &state.db,
        tenant_id,
        &data_source_id,
        &conv_id,
        &query_id,
        &super::semantic_audit::intent_json(&audit),
        &super::semantic_audit::verification_json(&audit),
        &release_decision,
        f64::from(audit.verification.confidence_basis.calibrated_score),
    )
    .await
    .map_err(|error| {
        AppError::Internal(format!(
            "failed to persist clarification semantic verification: {error}"
        ))
    })?;
    super::semantic_audit::require_execution_validation_decision(&release_decision).map_err(
        |reason| {
            AppError::ValidationError(format!(
            "{reason}. Resolve the metric, grain, population, time or join ambiguity and retry."
        ))
        },
    )?;

    sqlx::query(
        "INSERT INTO nl2sql_queries \
         (id, tenant_id, user_id, data_source_id, conversation_id, question, \
          generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?)",
    )
    .bind(&query_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&data_source_id)
    .bind(&conv_id)
    .bind(&question_text)
    .bind(&sql)
    .bind(planning_ms)
    .bind(route_confidence)
    .bind(routing_method.as_deref())
    .bind(semantic_context.clone())
    .bind(super::applied_rules_json_value(&applied_rules))
    .execute(&state.db)
    .await?;

    let explanation = match explain_sql(
        &state,
        &claims,
        sql.as_deref().unwrap_or(""),
        &schema_info,
        "en-US",
    )
    .await
    {
        Ok(e) if !e.is_empty() => Some(e),
        _ => None,
    };

    let summary_version = super::fetch_summary_version_i32(&state.db, &conv_id).await;

    let resp_data = ClarifyResponseData {
        data_source_id: data_source_id.clone(),
        question: question_text.clone(),
        sql: sql.clone(),
        explanation: explanation.clone(),
        error: None,
        query_id: query_id.clone(),
        conversation_id: Some(conv_id.clone()),
        clarification_context: clarification_context.clone(),
        fallback_mode: if soft_fallback_applied {
            Some(format!("{}_granularity", fallback_granularity))
        } else {
            None
        },
        summary_version,
        applied_rules: applied_rules.clone(),
    };

    let record = super::chat::SessionMessageRecord {
        role: "assistant".to_string(),
        content: serde_json::json!(&resp_data),
        timestamp_ms: ts + 1,
    };
    // B-01: Log (non-fatal) if session persistence fails.
    if let Err(e) =
        super::chat::append_message(&state.data_dir, tenant_id, user_id, session_id, &record)
    {
        tracing::warn!(error = %e, tenant_id, user_id, session_id, "failed to persist final assistant response to session");
    }
    super::foreign_keys::append_clarification_closed(
        &state,
        tenant_id,
        user_id,
        session_id,
        ts + 2,
    );

    Ok(Json(ClarifyResponse {
        data: Some(ClarifyResponseData {
            data_source_id,
            question: question_text,
            sql,
            explanation,
            error: None,
            query_id,
            conversation_id: Some(conv_id),
            clarification_context: clarification_context.clone(),
            fallback_mode: if soft_fallback_applied {
                Some(format!("{}_granularity", fallback_granularity))
            } else {
                None
            },
            summary_version,
            applied_rules,
        }),
        pending_clarification: None,
        error: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knowledge_candidate(schema_table_count: usize, score: f64) -> SqlKnowledgeRouteCandidate {
        SqlKnowledgeRouteCandidate {
            data_source_id: "knowledge-ds".to_string(),
            data_source_name: "知识库数据源".to_string(),
            confidence: sql_knowledge_route_confidence(score, 3, schema_table_count),
            score,
            snippet_count: 3,
            schema_table_count,
            filename: "ecpm.sql".to_string(),
            line_span: "1-20".to_string(),
            reason: "matched: ecpm".to_string(),
        }
    }

    fn schema_route(confidence: f32) -> RouteResult {
        RouteResult {
            data_source_id: "schema-ds".to_string(),
            confidence,
            method: "rrfs".to_string(),
            matched_tables: vec![MatchedTableInfo {
                data_source_id: "schema-ds".to_string(),
                table_name: "weak_match".to_string(),
                best_column: String::new(),
                column_description: String::new(),
                similarity_score: confidence,
            }],
            clarification_question: None,
        }
    }

    #[test]
    fn sql_knowledge_empty_schema_can_replace_weak_schema_route() {
        let candidate = knowledge_candidate(0, 1.6);
        let current = schema_route(0.42);

        assert!(should_prefer_sql_knowledge_route(
            &candidate,
            Some(&current)
        ));
    }

    #[test]
    fn sql_knowledge_does_not_replace_strong_schema_route_without_strong_score() {
        let candidate = knowledge_candidate(0, 1.6);
        let current = schema_route(0.97);

        assert!(!should_prefer_sql_knowledge_route(
            &candidate,
            Some(&current)
        ));
    }

    #[test]
    fn sql_knowledge_can_route_when_schema_candidates_are_empty() {
        let candidate = knowledge_candidate(0, 1.0);

        assert!(should_prefer_sql_knowledge_route(&candidate, None));
    }

    #[test]
    fn explicit_business_domain_routes_without_embedding_candidates() {
        let domains = vec![crate::nl2sql::routing::BusinessDomain {
            domain_name: "设备运维域".to_string(),
            domain_description: "设备状态".to_string(),
            tables: vec!["task_dispatch_device".to_string()],
            confidence_score: 0.95,
            datasource_id: Some("ds-1".to_string()),
            routing_mode: "assist".to_string(),
        }];
        let sources = vec![("ds-1".to_string(), "local".to_string(), String::new())];

        let result = route_result_from_explicit_business_domains(
            &domains,
            "查一下设备运维域",
            &sources,
            None,
        )
        .expect("explicit domain should route deterministically");

        assert_eq!(result.data_source_id, "ds-1");
        assert_eq!(result.method, "business_domain");
        assert_eq!(result.matched_tables[0].table_name, "task_dispatch_device");
    }

    #[test]
    fn explicit_domains_across_datasources_remain_ambiguous() {
        let domains = vec![
            crate::nl2sql::routing::BusinessDomain {
                domain_name: "订单域".to_string(),
                domain_description: String::new(),
                tables: vec!["orders".to_string()],
                confidence_score: 0.9,
                datasource_id: Some("ds-1".to_string()),
                routing_mode: "assist".to_string(),
            },
            crate::nl2sql::routing::BusinessDomain {
                domain_name: "库存域".to_string(),
                domain_description: String::new(),
                tables: vec!["inventory".to_string()],
                confidence_score: 0.9,
                datasource_id: Some("ds-2".to_string()),
                routing_mode: "assist".to_string(),
            },
        ];
        let sources = vec![
            ("ds-1".to_string(), "sales".to_string(), String::new()),
            ("ds-2".to_string(), "warehouse".to_string(), String::new()),
        ];

        assert!(route_result_from_explicit_business_domains(
            &domains,
            "比较订单域和库存域",
            &sources,
            None,
        )
        .is_none());
    }

    #[test]
    fn route_task_terminal_status_distinguishes_clarification_from_no_candidates() {
        let no_candidates = RouteResponse {
            routed: false,
            result: None,
            error: Some("no candidates".to_string()),
        };
        assert_eq!(route_task_terminal_fields(&no_candidates).0, "failed");

        let clarification = RouteResponse {
            routed: false,
            result: Some(RouteResult {
                data_source_id: String::new(),
                confidence: 0.3,
                method: "clarification".to_string(),
                matched_tables: Vec::new(),
                clarification_question: Some("请选择数据源".to_string()),
            }),
            error: None,
        };
        assert_eq!(
            route_task_terminal_fields(&clarification).0,
            "clarification_needed"
        );

        let routed = RouteResponse {
            routed: true,
            result: Some(schema_route(0.9)),
            error: None,
        };
        assert_eq!(route_task_terminal_fields(&routed).0, "completed");
    }
}
