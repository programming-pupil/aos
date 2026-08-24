//! NL2SQL API — natural language to SQL conversion and execution.
#![allow(dead_code, private_interfaces)]

use anyhow::Context;
use axum::{
    extract::{Extension, State},
    routing::{
        delete as routing_delete, get as routing_get, patch as routing_patch, post as routing_post,
        put as routing_put,
    },
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use std::sync::{Arc, Mutex, OnceLock};

use crate::auth::Claims;
use crate::error::{AppError, Result};
pub(crate) use crate::nl2sql::ForeignKeyPrompt;
pub(crate) use crate::routes::chat;
pub(crate) use crate::routes::PaginationParams;
use crate::state::AppState;
pub(crate) use api::{
    InputContentBlock, InputMessage, MessageRequest, OutputContentBlock, ToolChoice, ToolDefinition,
};

pub(crate) fn collect_output_text(blocks: &[OutputContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            OutputContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Default)]
struct Nl2sqlCandidateHealth {
    suppressed_until: Option<std::time::Instant>,
    healthy_until: Option<std::time::Instant>,
    failure_generation: u64,
}

struct Nl2sqlCandidateAttempt {
    failure_generation: u64,
    _probe_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

fn nl2sql_candidate_health(
) -> &'static Mutex<std::collections::HashMap<String, Nl2sqlCandidateHealth>> {
    static HEALTH: OnceLock<Mutex<std::collections::HashMap<String, Nl2sqlCandidateHealth>>> =
        OnceLock::new();
    HEALTH.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn nl2sql_candidate_probe_gates(
) -> &'static Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static GATES: OnceLock<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    GATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn nl2sql_candidate_suppression_secs() -> u64 {
    std::env::var("NL2SQL_UNUSABLE_CANDIDATE_COOLDOWN_SECS")
        .or_else(|_| std::env::var("NL2SQL_THINKING_ONLY_CANDIDATE_COOLDOWN_SECS"))
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 30)
        .unwrap_or(600)
}

fn nl2sql_candidate_healthy_secs() -> u64 {
    std::env::var("NL2SQL_CANDIDATE_HEALTHY_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 10)
        .unwrap_or(120)
}

fn nl2sql_candidate_suppression_key(
    tenant_id: &str,
    config: &crate::nl2sql::ChatTenantConfig,
) -> String {
    format!(
        "{}:{}:{}:{}",
        tenant_id,
        config.key_id.as_deref().unwrap_or("env-fallback"),
        config.provider,
        config.model
    )
}

fn candidate_health_snapshot(key: &str) -> (bool, bool, u64) {
    let now = std::time::Instant::now();
    let mut health = nl2sql_candidate_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = health.entry(key.to_string()).or_default();
    if entry.suppressed_until.is_some_and(|until| until <= now) {
        entry.suppressed_until = None;
    }
    if entry.healthy_until.is_some_and(|until| until <= now) {
        entry.healthy_until = None;
    }
    (
        entry.suppressed_until.is_some(),
        entry.healthy_until.is_some(),
        entry.failure_generation,
    )
}

async fn acquire_nl2sql_candidate_attempt(
    tenant_id: &str,
    config: &crate::nl2sql::ChatTenantConfig,
    respect_suppression: bool,
) -> Option<Nl2sqlCandidateAttempt> {
    let key = nl2sql_candidate_suppression_key(tenant_id, config);
    acquire_nl2sql_candidate_attempt_by_key(&key, respect_suppression).await
}

async fn acquire_nl2sql_candidate_attempt_by_key(
    key: &str,
    respect_suppression: bool,
) -> Option<Nl2sqlCandidateAttempt> {
    let (suppressed, healthy, failure_generation) = candidate_health_snapshot(key);
    if suppressed && respect_suppression {
        return None;
    }
    if healthy && !suppressed {
        return Some(Nl2sqlCandidateAttempt {
            failure_generation,
            _probe_guard: None,
        });
    }

    let gate = nl2sql_candidate_probe_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let probe_guard = gate.lock_owned().await;
    let (suppressed, healthy, failure_generation) = candidate_health_snapshot(key);
    if suppressed && respect_suppression {
        return None;
    }
    Some(Nl2sqlCandidateAttempt {
        failure_generation,
        _probe_guard: (!healthy || suppressed).then_some(probe_guard),
    })
}

fn mark_nl2sql_candidate_success(
    tenant_id: &str,
    config: &crate::nl2sql::ChatTenantConfig,
    observed_failure_generation: u64,
) {
    let key = nl2sql_candidate_suppression_key(tenant_id, config);
    mark_nl2sql_candidate_success_by_key(&key, observed_failure_generation);
}

fn mark_nl2sql_candidate_success_by_key(key: &str, observed_failure_generation: u64) {
    let mut health = nl2sql_candidate_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = health.entry(key.to_string()).or_default();
    if entry.failure_generation != observed_failure_generation {
        return;
    }
    entry.suppressed_until = None;
    entry.healthy_until = Some(
        std::time::Instant::now() + std::time::Duration::from_secs(nl2sql_candidate_healthy_secs()),
    );
}

pub(crate) fn nl2sql_candidate_is_suppressed(
    tenant_id: &str,
    config: &crate::nl2sql::ChatTenantConfig,
) -> bool {
    candidate_health_snapshot(&nl2sql_candidate_suppression_key(tenant_id, config)).0
}

pub(crate) fn suppress_nl2sql_candidate(tenant_id: &str, config: &crate::nl2sql::ChatTenantConfig) {
    let key = nl2sql_candidate_suppression_key(tenant_id, config);
    suppress_nl2sql_candidate_by_key(&key);
}

fn suppress_nl2sql_candidate_by_key(key: &str) {
    let until = std::time::Instant::now()
        + std::time::Duration::from_secs(nl2sql_candidate_suppression_secs());
    let mut health = nl2sql_candidate_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = health.entry(key.to_string()).or_default();
    entry.failure_generation = entry.failure_generation.wrapping_add(1);
    entry.suppressed_until = Some(until);
    entry.healthy_until = None;
}

pub(crate) fn clear_nl2sql_candidate_suppression(
    tenant_id: &str,
    config: &crate::nl2sql::ChatTenantConfig,
) {
    let key = nl2sql_candidate_suppression_key(tenant_id, config);
    let now = std::time::Instant::now();
    let mut health = nl2sql_candidate_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = health.entry(key).or_default();
    if entry.suppressed_until.is_some_and(|until| until > now) {
        return;
    }
    entry.suppressed_until = None;
    entry.healthy_until =
        Some(now + std::time::Duration::from_secs(nl2sql_candidate_healthy_secs()));
}

pub(crate) fn is_thinking_only_length_response(
    response: &api::MessageResponse,
    collected_text: &str,
) -> bool {
    let stopped_for_length = response.stop_reason.as_deref().is_some_and(|reason| {
        reason.eq_ignore_ascii_case("length") || reason.eq_ignore_ascii_case("max_tokens")
    });
    let has_thinking = response.content.iter().any(|block| {
        matches!(
            block,
            OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. }
        )
    });
    stopped_for_length
        && collected_text.trim().is_empty()
        && has_thinking
        && response.content.iter().all(|block| match block {
            OutputContentBlock::Thinking { .. } | OutputContentBlock::RedactedThinking { .. } => {
                true
            }
            OutputContentBlock::Text { text } => text.trim().is_empty(),
            OutputContentBlock::ToolUse { .. } => false,
        })
}

pub(crate) use self::queries::generate_conversation_summary;
pub(crate) use self::queries::{correct_sql, upsert_nl2sql_conversation};
pub(crate) use self::queries::{
    enforce_query_policy, extract_columns_from_sql, extract_tables_from_sql,
    query_policy_denial_message,
};
pub(crate) use self::query as query_request;
pub(crate) use self::routing::RouteRequest as RoutingRouteRequest;
pub(crate) use self::routing::{clarify as clarify_request, route as route_request};

/// Standard paginated response wrapper used by all list endpoints.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

impl<T: Serialize> PaginatedResponse<T> {
    pub fn new(data: Vec<T>, total: i64, page: u32, per_page: u32) -> Self {
        let per_page = per_page.max(1);
        let total_pages = ((total as u64 + per_page as u64 - 1) / per_page as u64) as u32;
        Self {
            data,
            total,
            page,
            per_page,
            total_pages,
        }
    }
}

pub mod agent;
pub mod agent_async;
pub mod agent_executor;
pub mod agent_planning;
pub mod analytics;
pub mod attribution;
pub mod auth;
pub mod clarify_async;
pub mod conversations;
pub mod cross_ds;
pub mod domains;
pub mod execution_support;
pub mod feedback;
pub mod foreign_keys;
pub mod golden_cases;
pub mod join_paths;
pub mod masking_rules;
pub mod merge_strategy;
pub mod metrics;
pub(crate) mod mongodb_query;
pub mod prompts;
pub mod queries;
pub mod query_async;
pub mod query_cancel;
pub mod query_policies;
pub mod query_understanding;
pub mod reference;
pub mod result_masking;
pub mod routing;
pub mod schema_changes;
pub mod semantic_audit;
pub mod semantics;
pub mod stream_results;
pub mod synonyms;
pub mod time_conversion;
pub mod time_patterns;
pub mod validation_rules;
pub mod views;

use self::auth::require_admin;
// Re-export SQL safety items so `super::is_safe_sql` / `super::classify_sql` /
// `super::SqlSafetyResult` keep resolving for sibling modules after the extraction.
#[allow(unused_imports)]
pub(crate) use nl2sql_core::sql_safety;
#[allow(unused_imports)]
pub(crate) use nl2sql_core::sql_safety::{classify_sql, is_safe_sql, SqlSafetyResult};
// Re-export masking items so sibling route modules keep their `super::...` imports.
#[allow(unused_imports)]
pub(crate) use self::result_masking::{
    apply_datasource_masking, apply_row_masking, is_datasource_sensitive, is_sensitive_column,
    load_datasource_sensitive_columns, mask_sensitive_value,
};
// Re-export cell decoders so sibling route modules keep their `super::...` imports.
#[allow(unused_imports)]
pub(crate) use nl2sql_core::cell_decoder::{
    decode_mysql_cell, decode_pg_cell, decode_postgres_cell,
};
// Re-export merge-strategy helpers used by the multi-step agent orchestrator.
#[allow(unused_imports)]
pub(crate) use self::merge_strategy::{
    cross_join, fill_null_columns, full_outer_join, hash_join, join_key_has_null, join_key_str,
    join_key_values, json_value_key, merge_rows_fn, right_join, union_all, union_distinct,
};
// Re-export query-policy DTOs and impl handlers so the routes() wiring and any sibling
// modules that referenced them via `super::...` keep compiling after the extraction.
#[allow(unused_imports)]
pub(crate) use self::query_policies::{
    create_query_policy, delete_query_policy, list_query_policies, update_query_policy,
    CreateQueryPolicyRequest, QueryPolicyListResponse, QueryPolicyRecord, UpdateQueryPolicyRequest,
};
// Re-export prompt builders for any sibling module that imported them via `super::...`.
#[allow(unused_imports)]
pub(crate) use self::prompts::{
    build_nl2sql_prompt, build_schema_overview_prompt, dialect_specific_rules,
    extract_sql_from_llm_output,
};
pub(crate) use self::reference::{
    load_query_reference_usages, persist_query_reference_usages,
    persist_sql_knowledge_usage_events, resolve_query_references, ReferenceBindingRequest,
    ReferencePromptSnippet, ReferenceUsageDto,
};
pub(crate) use self::time_conversion::normalize_generated_time_conversions;
// Re-export pure requirement-gate and metric-constraint rules from nl2sql-core.
#[allow(unused_imports)]
pub(crate) use nl2sql_core::requirements::{
    augment_follow_up_requirement_context, augment_question_for_metric_generation,
    augment_question_for_metric_hint, build_requirement_clarification_question,
    enforce_metric_hard_constraint_sql, llm_clarification_reasks_metric, matched_metric_names,
    normalize_domain_match_text, normalize_sql_time_filters_with_qu, parse_metric_aliases,
    parse_requirements_from_question, resolve_metric_hard_constraint, MetricHardConstraint,
    MetricMatchCandidate, RequirementCheckResult,
};
// Re-export multi-step agent planning DTOs and helpers.
#[allow(unused_imports)]
pub(crate) use self::agent_planning::{
    build_agent_planning_prompt, cross_ds_relations_summary, load_cross_domain_clusters_summary,
    parse_merge_strategy, parse_multi_step_plan, ColumnPrompt, CrossDatasourceRelation,
    DatasourceSchemaInfo, TableForeignKey, TablePrompt,
};
// Re-export multi-datasource Agent executor DTOs so existing route modules keep stable paths.
#[allow(unused_imports)]
pub(crate) use self::agent::execute_agent_request;
#[allow(unused_imports)]
pub(crate) use self::agent_executor::{
    AgentExecuteRequest, AgentExecuteResponse, FinalAgentResult, Nl2SqlAgent, StepExecutionDetail,
};
// Re-export execution support models shared by query execution, masking, and agent execution.
#[allow(unused_imports)]
pub(crate) use self::execution_support::{
    ColumnInfo, ExecuteRequest, ExecuteResponse, SelfCorrectContext, SqlExecErrorKind,
    SqlRepairDecision,
};

// ── Shared constants ─────────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) const SQL_MAX_ROWS: usize = 10_000;
pub const DEFAULT_MAX_AGENT_STEPS: usize = 10;
pub const DEFAULT_MAX_CROSS_DS_TABLES: usize = 4;
pub const DEFAULT_MAX_CROSS_DS_ROWS: usize = 10_000;
pub const DEFAULT_MAX_ROWS_PER_STEP: usize = 10_000;
pub const DEFAULT_MAX_AGENT_RESPONSE_ROWS: usize = 300;

pub(crate) const NL2SQL_EMBEDDING_REQUIRED_MESSAGE: &str =
    "本地语义模型暂未就绪，请检查内置模型文件；也可配置 embedding API 作为增强模型。";

pub(crate) fn empty_schema_info() -> serde_json::Value {
    json!({"tables": [], "foreign_keys": []})
}

pub(crate) async fn require_nl2sql_embedding_config(
    state: &AppState,
    tenant_id: &str,
) -> Result<()> {
    if crate::nl2sql::resolve_embedding_config(&state.db, tenant_id, Some("nl2sql"))
        .await
        .is_some()
    {
        Ok(())
    } else {
        Err(AppError::ValidationError(
            NL2SQL_EMBEDDING_REQUIRED_MESSAGE.to_string(),
        ))
    }
}

pub(crate) fn is_false(v: &bool) -> bool {
    !v
}

fn push_rule_hit(
    target: &mut Vec<AppliedRuleHit>,
    rule_key: &str,
    rule_name: &str,
    detail: Option<String>,
) {
    target.push(AppliedRuleHit {
        rule_key: rule_key.to_string(),
        rule_name: rule_name.to_string(),
        detail,
    });
}

pub(crate) fn applied_rules_json_value(applied_rules: &[AppliedRuleHit]) -> serde_json::Value {
    serde_json::to_value(applied_rules).unwrap_or_else(|_| serde_json::json!([]))
}

async fn persist_reference_usages_for_query(
    state: &AppState,
    claims: &Claims,
    query_id: &str,
    datasource_id: &str,
    question: &str,
    reference_snippets: &[ReferencePromptSnippet],
) {
    persist_query_reference_usages(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        query_id,
        datasource_id,
        reference_snippets,
    )
    .await;
    persist_sql_knowledge_usage_events(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        Some(datasource_id),
        "query_use",
        Some(question),
        Some(query_id),
        reference_snippets,
    )
    .await;
}

// ── DTOs ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct QueryRequest {
    pub data_source_id: String,
    pub question: String,
    /// Optional conversation ID for multi-turn context.
    /// If omitted on the first query of a conversation, a new ID is generated.
    pub conversation_id: Option<String>,
    /// Optional routing confidence (0.0-1.0) from `/nl2sql/route`.
    pub route_confidence: Option<f32>,
    /// Optional routing method from `/nl2sql/route` (e.g. rrfs / llm / manual).
    pub routing_method: Option<String>,
    /// Optional semantic context snapshot from `/nl2sql/route` matched tables.
    pub semantic_context: Option<serde_json::Value>,
    /// Optional reusable query references selected by the user.
    pub reference_bindings: Option<ReferenceBindingRequest>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponse {
    pub sql: Option<String>,
    pub explanation: Option<String>,
    pub error: Option<String>,
    pub query_id: String,
    pub conversation_id: Option<String>,
    pub summary_version: Option<i32>,
    /// When the LLM determines the question is too vague or unrelated, it returns
    /// a clarification question instead of SQL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
    /// 已确认约束（用于澄清阶段展示）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_requirements: Option<Vec<String>>,
    /// 缺失约束（用于澄清阶段展示）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_requirements: Option<Vec<String>>,
    /// Query Understanding enrichment: intent, entities, rewritten question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_understanding: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult>,
    /// Detected query intent (aggregate, compare, select, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// True when the SQL was served from the result cache (no LLM call made).
    #[serde(skip_serializing_if = "crate::routes::nl2sql::is_false")]
    pub cache_hit: bool,
    /// Enterprise traceability: which NL2SQL rules/guards were triggered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub applied_rules: Vec<AppliedRuleHit>,
    /// Reference snippets that influenced this SQL generation.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub used_references: Vec<ReferenceUsageDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedRuleHit {
    /// Stable key, for frontend/UI logic.
    #[serde(alias = "rule_key")]
    pub rule_key: String,
    /// Human-readable label for auditing and UX.
    #[serde(alias = "rule_name")]
    pub rule_name: String,
    /// Why this rule was considered "hit" for this answer.
    #[serde(alias = "detail_text")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QueryHistoryItem {
    pub id: String,
    pub data_source_id: Option<String>,
    pub question: String,
    pub generated_sql: Option<String>,
    pub executed: bool,
    /// i64 (widened from the DB's INT UNSIGNED) so the JSON output can
    /// represent the full value range without overflow.
    pub rows_returned: i64,
    pub planning_ms: i64,
    pub execution_ms: i64,
    pub error_message: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct QueryHistoryResponse {
    pub queries: Vec<QueryHistoryItem>,
    pub total: usize,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct SaveViewRequest {
    pub query_id: String,
    pub name: String,
    pub description: Option<String>,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SavedView {
    pub query_id: String,
    pub data_source_id: Option<String>,
    pub conversation_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub sql: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct SavedViewsResponse {
    pub views: Vec<SavedView>,
}

// ── P3-2: Conversation Summary REST API ──────────────────────────────────────

/// A single message in a conversation thread.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ConversationMessage {
    pub message_type: String,
    pub query_id: String,
    pub data_source_id: Option<String>,
    pub question: String,
    pub generated_sql: Option<String>,
    pub rows_returned: Option<i64>,
    pub execution_ms: Option<i64>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_turn: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_requirements: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_requirements: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_rules: Option<Vec<AppliedRuleHit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_references: Option<Vec<ReferenceUsageDto>>,
}

/// A conversation thread, possibly with an LLM-generated summary.
#[derive(Debug, Serialize)]
pub struct ConversationItem {
    pub id: String,
    pub message_count: i64,
    pub summary: Option<String>,
    pub last_question: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationItem>,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct ConversationDetailResponse {
    pub id: String,
    pub message_count: i64,
    pub total_messages: i64,
    pub page: u32,
    pub per_page: u32,
    pub has_more: bool,
    pub summary: Option<String>,
    pub last_question: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub created_at: String,
    pub updated_at: String,
}

// ── P0-2: Multi-datasource Agent ─────────────────────────────────────────────

// ── Agent config helpers ───────────────────────────────────────────────────────

#[allow(dead_code)]
fn max_cross_ds_tables() -> usize {
    nl2sql_domain::config::max_cross_ds_tables()
}

#[allow(dead_code)]
pub(crate) fn max_cross_ds_rows() -> usize {
    nl2sql_domain::config::max_cross_ds_rows()
}

#[allow(dead_code)]
async fn do_record_routing_features(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    tables: &[String],
) -> anyhow::Result<()> {
    let _placeholders: Vec<&str> = tables.iter().map(|_| "?").collect();
    let sql = format!(
        "INSERT INTO nl2sql_table_routing_features \
         (tenant_id, datasource_id, table_name, query_count, last_query_at) \
         VALUES {} \
         ON CONFLICT DO UPDATE SET \
         query_count = query_count + 1, \
         last_query_at = CURRENT_TIMESTAMP",
        tables
            .iter()
            .map(|_| "(?, ?, ?, 1, CURRENT_TIMESTAMP)".to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Generated VALUES entries contain placeholders only.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for table in tables {
        query = query.bind(tenant_id).bind(datasource_id).bind(table);
    }
    query.execute(db).await?;
    Ok(())
}

pub(crate) async fn record_routing_features(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    tables: &[String],
) {
    if let Err(e) = do_record_routing_features(db, tenant_id, datasource_id, tables).await {
        tracing::warn!(error = %e, tenant_id, datasource_id, "record_routing_features failed");
    }
}

// Multi-step agent planning helpers moved to routes/nl2sql/agent_planning.rs.

// decode_mysql_cell moved to nl2sql-core.

// decode_postgres_cell moved to nl2sql-core.

// Sensitive-column masking moved to routes/nl2sql/result_masking.rs.

// SQL safety classifier moved to nl2sql-core.

pub(crate) async fn validate_data_source_access(
    state: &AppState,
    tenant_id: &str,
    user_id: &str,
    role: &str,
    data_source_id: &str,
) -> Result<String> {
    let row = sqlx::query(
        "SELECT tenant_id, user_id, db_type, visibility FROM data_sources WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(data_source_id)
    .fetch_optional(&state.db)
    .await?;

    let (ds_tenant_id, user_id_col, db_type, visibility): (String, Option<String>, String, String) =
        match row {
            Some(r) => (
                r.get("tenant_id"),
                r.get("user_id"),
                r.get("db_type"),
                r.get("visibility"),
            ),
            None => return Err(AppError::NotFound("data source not found".into())),
        };

    if ds_tenant_id != tenant_id {
        return Err(AppError::Forbidden);
    }
    let is_admin = role == "admin" || role == "superadmin";
    let is_tenant_visible = visibility == "tenant";
    if !is_tenant_visible && user_id_col.as_deref() != Some(user_id) && !is_admin {
        return Err(AppError::Forbidden);
    }

    Ok(db_type)
}

// ── LLM integration ──────────────────────────────────────────────────────────

/// Result of SQL generation: the generated SQL string (or error through Err).
#[allow(dead_code)]
#[derive(Debug)]
struct GenerateSqlResult {
    sql: String,
    clarification_question: Option<String>,
    usage: Option<api::Usage>,
    model: Option<String>,
    api_key_id: Option<String>,
    provider: Option<String>,
    tool_reference_snippets: Vec<ReferencePromptSnippet>,
}

pub(crate) async fn record_nl2sql_token_usage(
    state: &AppState,
    claims: &Claims,
    conversation_id: &str,
    request_id: Option<&str>,
    usage: &api::Usage,
    model: &str,
    api_key_id: Option<String>,
    provider: Option<String>,
) {
    let Some(writer) = state.usage_writer.as_ref() else {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            conversation_id,
            "usage_writer is not initialized; skipping NL2SQL token usage persistence"
        );
        return;
    };
    let total_tokens = usage.total_tokens();
    let cost = usage.estimated_cost_usd(model).total_cost_usd();
    let record = chat::TokenUsageRecord {
        tenant_id: claims.tenant_id.clone(),
        user_id: claims.sub.clone(),
        session_id: format!("nl2sql:{conversation_id}"),
        request_id: request_id.map(std::string::ToString::to_string),
        model: model.to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        total_tokens,
        estimated_cost_usd: cost,
        api_key_id,
        provider: provider.unwrap_or_else(|| "nl2sql".to_string()),
        created_at: Utc::now(),
    };
    if let Err(e) = writer.write(&record).await {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            conversation_id,
            error = %e,
            "failed to persist NL2SQL token usage"
        );
    }
}

/// Holds loaded conversation history including messages and summary.
#[allow(dead_code)]
#[derive(Debug)]
struct ConversationHistory {
    /// Question → SQL pairs, in chronological order.
    messages: Vec<(String, String)>,
    /// Conversation summary from `nl2sql_conversations.summary`, if set.
    summary: Option<String>,
}

impl ConversationHistory {
    /// Build an LLM prompt prefix that augments the conversation summary with
    /// coreference resolution against the most recent turn. Returns a String
    /// containing the original summary (if any) and an additional section
    /// describing follow-up intent — pronouns ("那"), inherited time ranges
    /// ("上月呢"), exclusions ("排除退货") and scope additions ("只看 VIP").
    ///
    /// Returns `None` when there is neither a summary nor a useful coreference
    /// resolution, so callers can pass `None` through to the prompt builder
    /// unchanged.
    pub(crate) fn enriched_summary(&self, current_question: &str) -> Option<String> {
        // The newest entry is at index 0 because the SQL orders by
        // `created_at DESC`. Map it to the coreference PrevContext.
        let mut tables_storage: Vec<String> = Vec::new();
        let prev_owned: Option<(String, String, Vec<String>)> =
            self.messages.first().map(|(q, sql)| {
                tables_storage = extract_top_level_tables(sql);
                (q.clone(), sql.clone(), tables_storage.clone())
            });

        let resolved = if let Some((q, sql, tables)) = &prev_owned {
            let table_refs: Vec<&str> = tables.iter().map(|s| s.as_str()).collect();
            let prev_ctx = crate::nl2sql::coreference::PrevContext {
                question: q,
                sql,
                time_range: None,
                tables: &table_refs,
                filters: &[],
            };
            crate::nl2sql::coreference::resolve(current_question, Some(&prev_ctx))
        } else {
            crate::nl2sql::coreference::resolve(current_question, None)
        };

        let coreference_section = if resolved.is_empty() {
            String::new()
        } else {
            resolved.to_prompt_context()
        };

        match (&self.summary, coreference_section.is_empty()) {
            (Some(s), false) => Some(format!("{s}\n{coreference_section}")),
            (Some(s), true) => Some(s.clone()),
            (None, false) => Some(coreference_section),
            (None, true) => None,
        }
    }
}

/// Cheap regex pass that pulls top-level table identifiers out of a SQL
/// statement for use in coreference resolution and masking. We use a
/// lightweight regex rather than `sqlparser` because (a) this runs on
/// previously-emitted SQL which is well-formed, and (b) any failure here
/// just degrades downstream context — it cannot cause a wrong answer.
pub(crate) fn extract_top_level_tables(sql: &str) -> Vec<String> {
    nl2sql_domain::sql::extract_top_level_tables(sql)
}

/// Fill missing cached schema entries from physical tables referenced by the
/// selected SQL knowledge snippets. This is deliberately narrower than a full
/// datasource scan: it keeps the request bounded, uses the already-authorized
/// datasource configuration on the server, and never sends credentials to the
/// model or stores them in the knowledge context.
pub(crate) async fn discover_knowledge_schema_tables(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
    db_type: &str,
    encrypted_config: &serde_json::Value,
    existing_schema: &serde_json::Value,
    references: &[ReferencePromptSnippet],
    network_budget: Option<Arc<agent_executor::DatasourceRequestBudget>>,
) -> serde_json::Value {
    const DEFAULT_MAX_TABLES: usize = 16;
    const DEFAULT_TABLE_TIMEOUT_SECS: u64 = 8;
    let max_tables = std::env::var("NL2SQL_ON_DEMAND_SCHEMA_MAX_TABLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_TABLES)
        .min(64);
    let timeout_secs = std::env::var("NL2SQL_ON_DEMAND_SCHEMA_TABLE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 1)
        .unwrap_or(DEFAULT_TABLE_TIMEOUT_SECS)
        .min(30);

    let mut known_aliases = std::collections::HashSet::new();
    let existing_tables = existing_schema.as_array().or_else(|| {
        existing_schema
            .get("tables")
            .and_then(|value| value.as_array())
    });
    if let Some(existing_tables) = existing_tables {
        for table in existing_tables {
            insert_schema_table_aliases(&mut known_aliases, table);
        }
    }

    let mut extracted_table_names = Vec::new();
    for reference in references {
        if reference.stale {
            continue;
        }
        for table in knowledge_physical_table_names(&reference.content) {
            let table = table.trim().to_string();
            if table.is_empty()
                || table_name_aliases(&table)
                    .iter()
                    .any(|name| known_aliases.contains(name))
            {
                continue;
            }
            extracted_table_names.push(table);
        }
    }
    let table_names =
        nl2sql_domain::sql::prioritize_schema_discovery_tables(extracted_table_names, max_tables);
    if table_names.is_empty() {
        return serde_json::json!([]);
    }

    let config = match crate::routes::data_sources::decrypt_config(
        encrypted_config,
        &state.data_dir,
        &claims.tenant_id,
        datasource_id,
    ) {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(
                datasource_id,
                error = %error,
                "on-demand SQL knowledge schema hydration could not decrypt datasource config"
            );
            return serde_json::json!([]);
        }
    };
    let discovery = crate::nl2sql::schema_discovery::SchemaDiscovery::new();
    let db_type = db_type.to_string();
    let _trino_permit = if matches!(db_type.as_str(), "presto" | "trino") {
        match agent_executor::acquire_trino_user_permit(&claims.tenant_id, &claims.sub).await {
            Ok(permit) => Some(permit),
            Err(error) => {
                tracing::warn!(datasource_id, error = %error, "on-demand schema hydration stopped by user concurrency limit");
                return serde_json::json!([]);
            }
        }
    } else {
        None
    };
    // Schema hydration is datasource traffic too. Keep it serial inside the
    // request and share the concurrency gate with SQL execution. Trino also
    // acquires the tenant-and-user scoped gate above, so concurrent tasks from
    // one user cannot fan out beyond the global limit of three.
    let budget = network_budget.unwrap_or_else(|| agent_executor::DatasourceRequestBudget::new(3));
    let mut tables = Vec::new();
    for table_name in table_names {
        let permit = match budget.acquire("Trino schema discovery").await {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(datasource_id, error = %error, "on-demand schema hydration stopped by request budget");
                break;
            }
        };
        let result = if matches!(db_type.as_str(), "presto" | "trino") {
            discovery
                .discover_table(&db_type, &config, &table_name)
                .await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                discovery.discover_table(&db_type, &config, &table_name),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "schema table discovery timed out after {timeout_secs}s"
                )),
            }
        };
        drop(permit);
        match result {
            Ok(Some(table)) => tables.push(table),
            Ok(None) => {}
            Err(error) => {
                if matches!(db_type.as_str(), "presto" | "trino")
                    && nl2sql_core::schema_discovery::trino_remote_state_is_uncertain(&error)
                {
                    tracing::warn!(
                        datasource_id,
                        table = %table_name,
                        error = %error,
                        "stopping on-demand Trino schema hydration because remote query state is uncertain"
                    );
                    break;
                }
                tracing::debug!(table = %table_name, error = %error, "on-demand schema table was not discoverable")
            }
        }
    }
    tracing::info!(
        datasource_id,
        requested_tables = references
            .iter()
            .flat_map(|reference| knowledge_physical_table_names(&reference.content))
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        discovered_tables = tables.len(),
        "on-demand SQL knowledge schema hydration completed"
    );
    serde_json::Value::Array(tables)
}

/// Extract physical table references from SQL knowledge without trying to
/// introspect CTE aliases or system catalogs. The SQL parser is intentionally
/// best-effort here; the database remains the authority during discovery and
/// execution.
fn knowledge_physical_table_names(sql: &str) -> Vec<String> {
    let cte_names =
        regex::Regex::new(r#"(?im)(?:\bwith\s+|,)\s*([A-Za-z_][A-Za-z0-9_$]*)\s+as\s*\("#)
            .ok()
            .map(|regex| {
                regex
                    .captures_iter(sql)
                    .filter_map(|capture| {
                        capture
                            .get(1)
                            .map(|value| value.as_str().to_ascii_lowercase())
                    })
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();

    extract_top_level_tables(sql)
        .into_iter()
        .filter(|table| {
            let bare = table
                .rsplit('.')
                .next()
                .unwrap_or(table)
                .to_ascii_lowercase();
            !cte_names.contains(&bare)
                && !table.eq_ignore_ascii_case("dual")
                && !table
                    .to_ascii_lowercase()
                    .starts_with("information_schema.")
                && !table.to_ascii_lowercase().starts_with("pg_catalog.")
        })
        .collect()
}

fn merge_schema_tables(
    existing: &serde_json::Value,
    discovered: &serde_json::Value,
) -> serde_json::Value {
    let Some(existing_tables) = existing.as_array() else {
        return discovered.clone();
    };
    let Some(discovered_tables) = discovered.as_array() else {
        return existing.clone();
    };
    let mut merged = existing_tables.clone();
    let mut aliases = std::collections::HashSet::new();
    for table in existing_tables {
        insert_schema_table_aliases(&mut aliases, table);
    }
    for table in discovered_tables {
        let mut table_aliases = std::collections::HashSet::new();
        insert_schema_table_aliases(&mut table_aliases, table);
        if table_aliases.is_empty() || table_aliases.is_disjoint(&aliases) {
            merged.push(table.clone());
            aliases.extend(table_aliases);
        }
    }
    serde_json::Value::Array(merged)
}

pub(crate) fn normalize_table_identifier(raw: &str) -> String {
    raw.trim()
        .trim_end_matches(';')
        .trim_end_matches(',')
        .split('.')
        .filter_map(|part| {
            let cleaned = part
                .trim()
                .trim_matches('`')
                .trim_matches('"')
                .trim_matches('[')
                .trim_matches(']')
                .trim();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned.to_ascii_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

pub(crate) fn insert_table_name_aliases(out: &mut std::collections::HashSet<String>, raw: &str) {
    let normalized = normalize_table_identifier(raw);
    if normalized.is_empty() {
        return;
    }
    let parts: Vec<&str> = normalized.split('.').filter(|p| !p.is_empty()).collect();
    for start in 0..parts.len() {
        out.insert(parts[start..].join("."));
    }
}

pub(crate) fn table_name_aliases(raw: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    insert_table_name_aliases(&mut out, raw);
    out
}

pub(crate) fn insert_schema_table_aliases(
    out: &mut std::collections::HashSet<String>,
    table: &serde_json::Value,
) {
    for key in [
        "fully_qualified_name",
        "table_name",
        "qualified_name",
        "physical_table_name",
        "name",
    ] {
        if let Some(name) = table.get(key).and_then(|v| v.as_str()) {
            insert_table_name_aliases(out, name);
        }
    }

    let catalog = table.get("catalog").and_then(|v| v.as_str());
    let schema = table.get("schema").and_then(|v| v.as_str());
    let physical = table
        .get("physical_table_name")
        .or_else(|| table.get("name"))
        .and_then(|v| v.as_str());
    if let (Some(schema), Some(physical)) = (schema, physical) {
        insert_table_name_aliases(out, &format!("{schema}.{physical}"));
        if let Some(catalog) = catalog {
            insert_table_name_aliases(out, &format!("{catalog}.{schema}.{physical}"));
        }
    }
}

pub(crate) fn table_ref_matches_set(
    table_ref: &str,
    table_set: &std::collections::HashSet<String>,
) -> bool {
    table_name_aliases(table_ref)
        .iter()
        .any(|alias| table_set.contains(alias))
}

/// Loads up to `limit` most recent Q&A pairs for a conversation and
/// fetches the conversation-level summary.
pub(crate) async fn load_conversation_history(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    conversation_id: &str,
    limit: usize,
) -> ConversationHistory {
    let limit_i32 = i32::try_from(limit).unwrap_or(i32::MAX);
    let messages: Vec<(String, String)> = sqlx::query_as::<_, (String, String)>(
        "SELECT question, generated_sql FROM nl2sql_queries \
         WHERE tenant_id = ? AND conversation_id = ? AND generated_sql IS NOT NULL AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT ?",
    )
    .bind(tenant_id)
    .bind(conversation_id)
    .bind(limit_i32)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let summary: Option<String> = sqlx::query_as::<_, (Option<String>,)>(
        "SELECT summary FROM nl2sql_conversations WHERE id = ? AND tenant_id = ? AND deleted_at IS NULL",
    )
    .bind(conversation_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .next()
    .map(|(s,)| s)
    .flatten();

    ConversationHistory { messages, summary }
}

/// Load all manual foreign key definitions for a datasource, scoped to a tenant.
pub(crate) async fn load_manual_foreign_keys(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Vec<ForeignKeyPrompt> {
    let rows: Vec<(String, String, String, String, String, String)> =
        sqlx::query_as::<_, (String, String, String, String, String, String)>(
            "SELECT source_table, source_column, source_type, target_table, target_column, target_type \
             FROM nl2sql_foreign_keys \
             WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

    rows.into_iter()
        .map(
            |(
                source_table,
                source_column,
                source_type,
                target_table,
                target_column,
                target_type,
            )| {
                ForeignKeyPrompt {
                    source_table,
                    source_column: source_column.clone(),
                    source_type,
                    target_table,
                    target_column: target_column.clone(),
                    target_type,
                }
            },
        )
        .collect()
}

/// Load all pre-computed JOIN paths for a datasource from nl2sql_join_paths.
/// Returns Vec of (path_text, sql_joins) tuples, sorted by fewest hops.
pub(crate) async fn load_join_paths_for_datasource(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
) -> Vec<(String, String)> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT path_text, sql_joins FROM nl2sql_join_paths \
         WHERE datasource_id = ? AND deleted_at IS NULL ORDER BY hops ASC LIMIT 200",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

fn sql_knowledge_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "knowledge_tree".to_string(),
            description: Some(
                "List SQL Knowledge files like a virtual folder tree. Use this first when you need to understand available SQL/Markdown assets before searching.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Optional business concept, metric, table, or filename hint used to rank files." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 80 }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_rg".to_string(),
            description: Some(
                "Ripgrep-style full-text search across SQL Knowledge files. Use exact metric names, table names, business terms, aliases, or filename fragments; call repeatedly with refined queries.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Exact keyword, phrase, metric, table, alias, or business term to search for." },
                    "filename": { "type": "string", "description": "Optional fileId or filename/path fragment to restrict search." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_list".to_string(),
            description: Some(
                "List the most relevant SQL Knowledge Base files and chunks available for this datasource before deciding what to read.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The business question or concept to list related knowledge for." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 8 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_search".to_string(),
            description: Some(
                "Search trusted SQL Knowledge Base chunks for reusable SQL examples, metric definitions, business rules, and query demos.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query in the user's business language." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 6 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_read".to_string(),
            description: Some(
                "Read exact lines from a SQL Knowledge file. Prefer fileId from knowledge_tree/knowledge_rg. Use startLine/endLine for Codex-like line-range reads; otherwise it reads a large useful range.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fileId": { "type": "string", "description": "Known SQL Knowledge file id to read." },
                    "startLine": { "type": "integer", "minimum": 1 },
                    "endLine": { "type": "integer", "minimum": 1 },
                    "query": { "type": "string", "description": "Fallback focused read query when fileId is unknown." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 4 }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "sql_example_open".to_string(),
            description: Some(
                "Open a full SQL example or a wide SQL context. Use this after knowledge_rg finds a promising SQL file so the full CTE chain can be adapted instead of copied blindly.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fileId": { "type": "string", "description": "Known SQL example file id to open." },
                    "startLine": { "type": "integer", "minimum": 1 },
                    "endLine": { "type": "integer", "minimum": 1 },
                    "query": { "type": "string", "description": "Fallback query describing the SQL example to open." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 4 }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_outline".to_string(),
            description: Some(
                "Summarize the structure of a SQL/Markdown knowledge file: CTEs, tables, metrics, parameters, headings. Use this before editing long SQL.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fileId": { "type": "string", "description": "Known SQL Knowledge file id to outline." },
                    "query": { "type": "string", "description": "Fallback query to find and outline relevant files." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 12 }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "knowledge_related".to_string(),
            description: Some(
                "Find files or chunks related to a promising SQL file by shared tables, metrics, columns, or directory context. Use this when one SQL example is close but may need supporting definitions.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fileId": { "type": "string", "description": "Known SQL Knowledge file id used as the anchor." },
                    "query": { "type": "string", "description": "Fallback business concept, metric, or table to find related knowledge." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 12 }
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "schema_search".to_string(),
            description: Some(
                "Search the live datasource schema for relevant tables and columns. Live schema always wins over old SQL examples.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Table, field, or metric concept to find in the live schema." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 12 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
    ]
}

fn sql_knowledge_tool_max_rounds() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_TOOL_MAX_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(6)
        .min(8)
}

fn sql_knowledge_tool_uses_per_round() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_TOOL_USES_PER_ROUND")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(6)
        .min(10)
}

fn sql_knowledge_prompt_max_snippets() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_PROMPT_MAX_SNIPPETS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(16)
        .min(24)
}

fn sql_knowledge_auto_open_file_limit() -> usize {
    std::env::var("NL2SQL_SQL_KNOWLEDGE_AUTO_OPEN_FILES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(3)
        .min(6)
}

fn should_enable_sql_generation_tool_loop() -> bool {
    std::env::var("NL2SQL_ENABLE_SQL_GENERATION_TOOL_LOOP")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn sql_generation_tool_max_rounds() -> usize {
    std::env::var("NL2SQL_SQL_GENERATION_TOOL_MAX_ROUNDS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(2)
        .min(8)
}

fn sql_generation_tool_total_result_max_chars() -> usize {
    std::env::var("NL2SQL_SQL_GENERATION_TOOL_TOTAL_RESULT_MAX_CHARS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value >= 8_000)
        .unwrap_or(48_000)
        .min(128_000)
}

fn sql_generation_tool_result_max_chars(tool_name: &str) -> usize {
    let default = match tool_name {
        "knowledge_read" | "sql_example_open" => 32_000,
        "knowledge_rg" | "knowledge_outline" | "knowledge_related" => 18_000,
        "knowledge_tree" => 12_000,
        "schema_search" => 10_000,
        _ => 8_000,
    };
    std::env::var("NL2SQL_SQL_GENERATION_TOOL_RESULT_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 2_000)
        .unwrap_or(default)
        .min(80_000)
}

fn compact_tool_text(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("\n...");
    }
    out
}

fn sql_knowledge_tool_payload(
    tool_name: &str,
    query: &str,
    snippets: &[ReferencePromptSnippet],
    content_max_chars: usize,
) -> serde_json::Value {
    json!({
        "tool": tool_name,
        "query": query,
        "count": snippets.len(),
        "items": snippets.iter().map(|snippet| {
            json!({
                "packId": snippet.pack_id,
                "packName": snippet.pack_name,
                "fileId": snippet.file_id,
                "filename": snippet.filename,
                "chunkId": snippet.chunk_id,
                "chunkType": snippet.chunk_type,
                "language": snippet.language,
                "lines": [snippet.start_line, snippet.end_line],
                "score": snippet.score,
                "reason": snippet.reason,
                "verified": snippet.verified,
                "stale": snippet.stale,
                "content": compact_tool_text(&snippet.content, content_max_chars)
            })
        }).collect::<Vec<_>>()
    })
}

fn merge_reference_snippets(
    base: &[ReferencePromptSnippet],
    extra: Vec<ReferencePromptSnippet>,
    max_items: usize,
) -> Vec<ReferencePromptSnippet> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for snippet in base.iter().cloned().chain(extra.into_iter()) {
        let key = reference_snippet_dedupe_key(&snippet);
        if seen.insert(key) {
            merged.push(snippet);
        }
        if merged.len() >= max_items {
            break;
        }
    }
    merged
}

fn reference_snippet_dedupe_key(snippet: &ReferencePromptSnippet) -> String {
    if snippet.chunk_id.starts_with("schema-search-") {
        return format!("schema:{}:{}", snippet.filename, snippet.reason);
    }
    if !snippet.file_id.trim().is_empty() {
        return format!(
            "file:{}:{}:{}:{}",
            snippet.file_id, snippet.start_line, snippet.end_line, snippet.chunk_type
        );
    }
    snippet.chunk_id.clone()
}

fn is_sql_knowledge_example_snippet(snippet: &ReferencePromptSnippet) -> bool {
    snippet.chunk_type == "sql_example"
        || matches!(snippet.language.as_deref(), Some("sql"))
        || snippet.filename.to_ascii_lowercase().ends_with(".sql")
}

fn should_auto_open_sql_knowledge_snippet(snippet: &ReferencePromptSnippet) -> bool {
    if snippet.stale
        || snippet.file_id.trim().is_empty()
        || !is_sql_knowledge_example_snippet(snippet)
    {
        return false;
    }

    snippet.verified || snippet.score >= 0.05
}

fn question_core_metric_terms(question: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();

    let mut ascii = String::new();
    for ch in question.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ascii.push(ch);
            continue;
        }
        if !ascii.is_empty() {
            let raw = std::mem::take(&mut ascii);
            let token = raw.to_ascii_lowercase();
            if is_ascii_core_metric_token(&raw) && seen.insert(token.clone()) {
                terms.push(token);
            }
        }
    }

    // Chinese metric names are open-ended, but production naming conventions
    // consistently expose a metric noun or suffix. Keep this intentionally
    // narrower than general knowledge tokenization: a false positive here can
    // promote an unrelated SQL file into executable evidence.
    let chinese_exact_metrics = [
        "收入",
        "成本",
        "利润",
        "收益",
        "营收",
        "流水",
        "销量",
        "单量",
        "订单数",
        "用户数",
        "活跃用户",
        "新增用户",
        "留存",
        "留存率",
        "转化率",
        "点击率",
        "完成率",
        "命中率",
        "成功率",
        "失败率",
        "客单价",
        "均价",
        "时长",
    ];
    let chinese_metric_suffixes = [
        "率", "量", "数", "额", "收入", "成本", "利润", "收益", "单价", "均价", "时长", "次数",
    ];
    for term in question_knowledge_terms(question) {
        if term.chars().any(|ch| ch.is_ascii()) {
            continue;
        }
        let len = term.chars().count();
        if !(2..=12).contains(&len) {
            continue;
        }
        let is_metric = chinese_exact_metrics.contains(&term.as_str())
            || chinese_metric_suffixes
                .iter()
                .any(|suffix| term.ends_with(suffix));
        if is_metric && seen.insert(term.clone()) {
            terms.push(term);
        }
    }

    terms.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });
    terms
}

fn is_ascii_core_metric_token(raw: &str) -> bool {
    let token = raw.trim().to_ascii_lowercase();
    if !(2..=64).contains(&token.len()) || token.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let common_metrics = [
        "aipu",
        "aip",
        "arpu",
        "arppu",
        "cac",
        "cpc",
        "cpm",
        "ctr",
        "cvr",
        "dau",
        "ecpm",
        "gmv",
        "kpi",
        "ltv",
        "mau",
        "orders",
        "profit",
        "revenue",
        "roas",
        "roi",
        "spend",
        "cost",
        "retention",
        "users",
        "uv",
        "pv",
        "wau",
    ];
    if common_metrics.contains(&token.as_str()) {
        return true;
    }
    let metric_parts = [
        "amount", "avg", "count", "cost", "gmv", "income", "margin", "metric", "profit", "rate",
        "revenue", "roas", "roi", "score", "spend", "total", "value",
    ];
    if token.contains('_') && token.split('_').any(|part| metric_parts.contains(&part)) {
        return true;
    }
    let uppercase_acronym = raw.len() <= 12
        && raw.chars().any(|ch| ch.is_ascii_uppercase())
        && raw
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .all(|ch| ch.is_ascii_uppercase());
    uppercase_acronym
}

fn sql_code_without_comments_or_string_literals(sql: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Code,
        SingleQuoted,
        LineComment,
        BlockComment,
    }

    let mut state = State::Code;
    let mut chars = sql.chars().peekable();
    let mut out = String::with_capacity(sql.len());
    while let Some(ch) = chars.next() {
        match state {
            State::Code => match (ch, chars.peek().copied()) {
                ('-', Some('-')) => {
                    chars.next();
                    out.push(' ');
                    state = State::LineComment;
                }
                ('/', Some('*')) => {
                    chars.next();
                    out.push(' ');
                    state = State::BlockComment;
                }
                ('\'', _) => {
                    out.push(' ');
                    state = State::SingleQuoted;
                }
                _ => out.push(ch),
            },
            State::SingleQuoted => {
                if ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    } else {
                        state = State::Code;
                    }
                }
                if ch == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            State::LineComment => {
                if ch == '\n' {
                    out.push('\n');
                    state = State::Code;
                } else {
                    out.push(' ');
                }
            }
            State::BlockComment => {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    out.push(' ');
                    state = State::Code;
                } else if ch == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
        }
    }
    out
}

fn sql_code_contains_ascii_metric_identifier(sql: &str, metric: &str) -> bool {
    let metric = metric.trim().to_ascii_lowercase();
    if metric.is_empty() {
        return false;
    }
    let code = sql_code_without_comments_or_string_literals(sql);
    let mut identifier = String::new();
    let matches_identifier = |identifier: &str| {
        identifier.split('_').any(|part| {
            part == metric || part.trim_end_matches(|ch: char| ch.is_ascii_digit()) == metric
        })
    };
    for ch in code.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            identifier.push(ch.to_ascii_lowercase());
        } else if !identifier.is_empty() {
            if matches_identifier(&identifier) {
                return true;
            }
            identifier.clear();
        }
    }
    false
}

fn sql_content_contains_core_metric(content: &str, metric: &str) -> bool {
    if metric
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return sql_code_contains_ascii_metric_identifier(content, metric);
    }
    sql_code_without_comments_or_string_literals(content).contains(metric)
}

fn sql_knowledge_snippet_matches_core_metric(
    question: &str,
    snippet: &ReferencePromptSnippet,
) -> bool {
    let ascii_core_terms = question_core_metric_terms(question)
        .into_iter()
        .filter(|term| term.is_ascii())
        .collect::<Vec<_>>();
    if ascii_core_terms.is_empty() {
        return true;
    }
    ascii_core_terms
        .iter()
        .any(|term| sql_content_contains_core_metric(&snippet.content, term))
}

fn filter_sql_knowledge_snippets_by_core_metric(
    question: &str,
    snippets: Vec<ReferencePromptSnippet>,
) -> Vec<ReferencePromptSnippet> {
    if !question_core_metric_terms(question)
        .iter()
        .any(|term| term.is_ascii())
    {
        return snippets;
    }
    let matching_files = snippets
        .iter()
        .filter(|snippet| sql_knowledge_snippet_matches_core_metric(question, snippet))
        .map(|snippet| snippet.file_id.clone())
        .filter(|file_id| !file_id.trim().is_empty())
        .collect::<std::collections::HashSet<_>>();
    snippets
        .into_iter()
        .filter(|snippet| {
            sql_knowledge_snippet_matches_core_metric(question, snippet)
                || (!snippet.file_id.trim().is_empty() && matching_files.contains(&snippet.file_id))
        })
        .collect()
}

fn sql_knowledge_auto_open_file_candidates(
    question: &str,
    snippets: &[ReferencePromptSnippet],
    max_files: usize,
) -> Vec<String> {
    if max_files == 0 {
        return Vec::new();
    }

    let mut ranked = snippets
        .iter()
        .filter(|snippet| should_auto_open_sql_knowledge_snippet(snippet))
        .filter(|snippet| sql_knowledge_snippet_matches_core_metric(question, snippet))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        bounded_reference_relevance(question, right)
            .partial_cmp(&bounded_reference_relevance(question, left))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.verified.cmp(&left.verified))
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.filename.cmp(&right.filename))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for snippet in ranked {
        if seen.insert(snippet.file_id.clone()) {
            out.push(snippet.file_id.clone());
        }
        if out.len() >= max_files {
            break;
        }
    }
    out
}

async fn auto_open_relevant_sql_knowledge_files(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
    question: &str,
    reference_snippets: &[ReferencePromptSnippet],
    max_files_override: Option<usize>,
) -> Vec<ReferencePromptSnippet> {
    let file_ids = sql_knowledge_auto_open_file_candidates(
        question,
        reference_snippets,
        max_files_override.unwrap_or_else(sql_knowledge_auto_open_file_limit),
    );
    if file_ids.is_empty() {
        return Vec::new();
    }

    let mut opened = Vec::new();
    for file_id in file_ids {
        match self::reference::sql_knowledge_read_for_tool(
            state,
            &claims.tenant_id,
            datasource_id,
            &file_id,
            Some(1),
            None,
            sql_generation_tool_result_max_chars("sql_example_open"),
        )
        .await
        {
            Ok((mut snippets, _payload)) => {
                for snippet in &mut snippets {
                    snippet.score = snippet.score.max(3.25);
                    snippet
                        .reason
                        .push_str("; deterministic Codex-like sql_example_open");
                }
                opened.append(&mut snippets);
            }
            Err(e) => tracing::warn!(
                error = %e,
                datasource_id,
                file_id,
                "nl2sql deterministic SQL knowledge auto-open failed"
            ),
        }
    }

    if !opened.is_empty() {
        self::reference::persist_sql_knowledge_usage_events(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some(datasource_id),
            "deterministic_sql_example_open",
            Some(question),
            None,
            &opened,
        )
        .await;
    }
    opened
}

const BOUNDED_SQL_KNOWLEDGE_MAX_FILES: usize = 2;
const BOUNDED_SQL_KNOWLEDGE_MAX_CHARS: usize = 18_000;

fn bounded_evidence_terms(question: &str) -> Vec<String> {
    let mut terms = question_knowledge_terms(question);
    terms.retain(|term| !matches!(term.as_str(), "app" | "apps" | "sql" | "查询" | "分析"));
    terms.sort_by(|left, right| {
        let left_ascii = left
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        let right_ascii = right
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        right_ascii
            .cmp(&left_ascii)
            .then_with(|| right.chars().count().cmp(&left.chars().count()))
            .then_with(|| left.cmp(right))
    });
    terms.dedup();
    terms.truncate(24);
    terms
}

fn bounded_reference_relevance(question: &str, snippet: &ReferencePromptSnippet) -> f64 {
    let core_hits = question_core_metric_terms(question)
        .iter()
        .filter(|term| sql_content_contains_core_metric(&snippet.content, term))
        .count() as f64;
    let evidence = format!("{}\n{}", snippet.filename, snippet.content).to_lowercase();
    let exact_hits = bounded_evidence_terms(question)
        .iter()
        .filter(|term| evidence.contains(term.as_str()))
        .count() as f64;
    core_hits * 1_000.0 + exact_hits * 100.0
}

fn append_bounded_excerpt_section(
    output: &mut String,
    label: &str,
    lines: &[&str],
    start: usize,
    end: usize,
    max_chars: usize,
) {
    if start >= end || output.chars().count() >= max_chars {
        return;
    }
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(label);
    output.push('\n');
    for line in &lines[start..end] {
        let line_chars = line.chars().count() + 1;
        if output.chars().count() + line_chars > max_chars {
            output.push_str("...[excerpt truncated]");
            break;
        }
        output.push_str(line);
        output.push('\n');
    }
}

fn focused_bounded_sql_excerpt(content: &str, question: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let lines = content.lines().collect::<Vec<_>>();
    let terms = bounded_evidence_terms(question);
    let mut matched_lines = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line = line.to_lowercase();
            let hits = terms
                .iter()
                .filter(|term| line.contains(term.as_str()))
                .count();
            (hits > 0).then_some((hits, idx))
        })
        .collect::<Vec<_>>();
    matched_lines.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let mut output = String::new();
    append_bounded_excerpt_section(
        &mut output,
        "-- [file header]",
        &lines,
        0,
        lines.len().min(24),
        max_chars,
    );
    let mut windows = matched_lines
        .into_iter()
        .take(4)
        .map(|(_, idx)| (idx.saturating_sub(35), (idx + 36).min(lines.len())))
        .collect::<Vec<_>>();
    windows.sort_unstable();
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in windows {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1 + 4) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    for (start, end) in merged {
        append_bounded_excerpt_section(
            &mut output,
            &format!("-- [relevant lines {}-{}]", start + 1, end),
            &lines,
            start,
            end,
            max_chars,
        );
    }
    if output.chars().count() < max_chars / 3 {
        let tail_start = lines.len().saturating_sub(48);
        append_bounded_excerpt_section(
            &mut output,
            &format!("-- [file tail lines {}-{}]", tail_start + 1, lines.len()),
            &lines,
            tail_start,
            lines.len(),
            max_chars,
        );
    }
    output.chars().take(max_chars).collect()
}

pub(crate) fn focus_bounded_sql_knowledge_references(
    question: &str,
    snippets: &[ReferencePromptSnippet],
) -> Vec<ReferencePromptSnippet> {
    let mut ranked = snippets
        .iter()
        .filter(|snippet| !snippet.stale)
        .filter(|snippet| sql_knowledge_snippet_matches_core_metric(question, snippet))
        .cloned()
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        bounded_reference_relevance(question, right)
            .partial_cmp(&bounded_reference_relevance(question, left))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.verified.cmp(&left.verified))
            .then_with(|| left.filename.cmp(&right.filename))
    });

    let mut seen_files = std::collections::HashSet::new();
    let mut selected = Vec::new();
    let mut remaining = BOUNDED_SQL_KNOWLEDGE_MAX_CHARS;
    for mut snippet in ranked {
        let identity = if snippet.file_id.trim().is_empty() {
            snippet.chunk_id.clone()
        } else {
            snippet.file_id.clone()
        };
        if !seen_files.insert(identity) {
            continue;
        }
        let slots_left = BOUNDED_SQL_KNOWLEDGE_MAX_FILES - selected.len();
        let content_budget = (remaining / slots_left.max(1)).min(10_000);
        snippet.content = focused_bounded_sql_excerpt(&snippet.content, question, content_budget);
        remaining = remaining.saturating_sub(snippet.content.chars().count());
        snippet
            .reason
            .push_str("; bounded attribution evidence window");
        selected.push(snippet);
        if selected.len() >= BOUNDED_SQL_KNOWLEDGE_MAX_FILES || remaining == 0 {
            break;
        }
    }
    selected
}

fn question_knowledge_terms(question: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();

    for token in crate::routes::nl2sql::reference::tokenize_for_sql_knowledge_tool(question) {
        if is_likely_knowledge_metric_token(&token) && seen.insert(token.to_lowercase()) {
            terms.push(token.to_lowercase());
        }
    }

    let mut ascii = String::new();
    let mut cjk = String::new();
    for ch in question.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if !cjk.is_empty() {
                push_cjk_ngrams(&cjk, &mut seen, &mut terms);
                cjk.clear();
            }
            ascii.push(ch.to_ascii_lowercase());
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            if !ascii.is_empty() {
                let token = std::mem::take(&mut ascii);
                if is_likely_knowledge_metric_token(&token) && seen.insert(token.clone()) {
                    terms.push(token);
                }
            }
            cjk.push(ch);
        } else {
            if !ascii.is_empty() {
                let token = std::mem::take(&mut ascii);
                if is_likely_knowledge_metric_token(&token) && seen.insert(token.clone()) {
                    terms.push(token);
                }
            }
            if !cjk.is_empty() {
                push_cjk_ngrams(&cjk, &mut seen, &mut terms);
                cjk.clear();
            }
        }
    }
    if !ascii.is_empty() {
        let token = std::mem::take(&mut ascii);
        if is_likely_knowledge_metric_token(&token) && seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    if !cjk.is_empty() {
        push_cjk_ngrams(&cjk, &mut seen, &mut terms);
    }

    terms
}

fn push_cjk_ngrams(
    text: &str,
    seen: &mut std::collections::HashSet<String>,
    terms: &mut Vec<String>,
) {
    let chars: Vec<char> = text.chars().collect();
    for width in 2..=chars.len().min(8) {
        for start in 0..=chars.len().saturating_sub(width) {
            let token: String = chars[start..start + width].iter().collect();
            if is_likely_knowledge_metric_token(&token) && seen.insert(token.clone()) {
                terms.push(token);
            }
            if terms.len() >= 80 {
                return;
            }
        }
    }
}

fn is_likely_knowledge_metric_token(token: &str) -> bool {
    let token = token.trim().to_lowercase();
    let len = token.chars().count();
    if !(2..=64).contains(&len) || token.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let generic_stopwords = [
        "select",
        "from",
        "where",
        "join",
        "left",
        "right",
        "inner",
        "outer",
        "group",
        "order",
        "limit",
        "with",
        "case",
        "when",
        "then",
        "else",
        "end",
        "and",
        "or",
        "not",
        "null",
        "true",
        "false",
        "查询",
        "统计",
        "分析",
        "看看",
        "多少",
        "如何",
        "怎么",
        "昨天",
        "今天",
        "明天",
        "前天",
        "最近",
        "本周",
        "上周",
        "本月",
        "上月",
        "今年",
        "去年",
        "分布",
        "趋势",
        "对比",
        "明细",
        "详情",
        "信息",
        "数据",
        "结果",
        "一下",
        "是多少",
    ];
    if generic_stopwords.iter().any(|word| token == *word) {
        return false;
    }
    true
}

fn knowledge_metric_candidates_from_references(
    question: &str,
    reference_snippets: &[ReferencePromptSnippet],
) -> Vec<(String, String, Option<String>)> {
    if reference_snippets.is_empty() {
        return Vec::new();
    }
    let terms = question_knowledge_terms(question);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for term in terms {
        let matched = reference_snippets.iter().any(|snippet| {
            if snippet.stale || snippet.score < 0.20 {
                return false;
            }
            let content = snippet.content.to_lowercase();
            let filename = snippet.filename.to_lowercase();
            content.contains(&term) || filename.contains(&term)
        });
        if matched && seen.insert(term.clone()) {
            out.push((term.clone(), term, None));
        }
        if out.len() >= 12 {
            break;
        }
    }
    out
}

fn has_strong_sql_knowledge_context(
    schema: &serde_json::Value,
    reference_snippets: &[ReferencePromptSnippet],
) -> bool {
    if reference_snippets.is_empty() {
        return false;
    }
    let table_count = schema.as_array().map(|arr| arr.len()).unwrap_or(0);
    let max_score = reference_snippets
        .iter()
        .filter(|snippet| !snippet.stale)
        .map(|snippet| snippet.score)
        .fold(0.0_f64, f64::max);
    let has_sql_example = reference_snippets.iter().any(|snippet| {
        !snippet.stale
            && (snippet.chunk_type == "sql_example"
                || matches!(snippet.language.as_deref(), Some("sql"))
                || snippet.filename.to_ascii_lowercase().ends_with(".sql"))
            && snippet.score >= 0.20
    });
    let has_verified = reference_snippets
        .iter()
        .any(|snippet| !snippet.stale && snippet.verified && snippet.score >= 0.20);

    if table_count == 0 {
        return has_sql_example || has_verified || max_score >= 0.60;
    }
    has_sql_example && max_score >= 0.45 || has_verified && max_score >= 0.45 || max_score >= 1.50
}

fn schema_search_summary(
    schema: &serde_json::Value,
    query: &str,
    limit: usize,
) -> serde_json::Value {
    let tokens = crate::routes::nl2sql::reference::tokenize_for_sql_knowledge_tool(query);
    let mut hits = Vec::new();
    let tables = schema
        .get("tables")
        .and_then(|v| v.as_array())
        .or_else(|| schema.as_array())
        .cloned()
        .unwrap_or_default();
    for table in tables {
        let table_name = table
            .get("table_name")
            .or_else(|| table.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let table_desc = table
            .get("ai_description")
            .or_else(|| table.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let columns = table
            .get("columns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut matched_columns = Vec::new();
        let mut score = 0usize;
        let haystack = format!("{table_name} {table_desc}").to_lowercase();
        for token in &tokens {
            if haystack.contains(token) {
                score += 2;
            }
        }
        for col in columns {
            let name = col
                .get("name")
                .or_else(|| col.get("column_name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let desc = col
                .get("description")
                .or_else(|| col.get("ai_description"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let col_haystack = format!("{name} {desc}").to_lowercase();
            if tokens.iter().any(|token| col_haystack.contains(token)) {
                score += 1;
                matched_columns.push(json!({ "name": name, "description": desc }));
            }
            if matched_columns.len() >= 12 {
                break;
            }
        }
        if score > 0 {
            hits.push(json!({
                "table": table_name,
                "description": table_desc,
                "matchedColumns": matched_columns,
                "score": score
            }));
        }
    }
    hits.sort_by(|a, b| {
        b.get("score")
            .and_then(|v| v.as_u64())
            .cmp(&a.get("score").and_then(|v| v.as_u64()))
    });
    serde_json::Value::Array(hits.into_iter().take(limit.max(1).min(20)).collect())
}

async fn execute_sql_knowledge_tool(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
    fallback_question: &str,
    schema: &serde_json::Value,
    tool_name: &str,
    input: &serde_json::Value,
    usage_context: &str,
    content_max_chars: usize,
) -> (Vec<ReferencePromptSnippet>, serde_json::Value) {
    let query = input
        .get("query")
        .or_else(|| input.get("q"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_question)
        .trim()
        .to_string();
    let limit = input
        .get("limit")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(match tool_name {
            "knowledge_tree" => 40,
            "knowledge_rg" => 12,
            "knowledge_outline" | "knowledge_related" => 8,
            "schema_search" => 8,
            "knowledge_read" | "sql_example_open" => 3,
            _ => 5,
        })
        .clamp(
            1,
            match tool_name {
                "knowledge_tree" => 80,
                "knowledge_rg" => 20,
                "knowledge_outline" | "knowledge_related" | "schema_search" => 12,
                _ => 8,
            },
        );
    let file_id = input
        .get("fileId")
        .or_else(|| input.get("file_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string());
    let filename = input
        .get("filename")
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::trim);
    let start_line = input
        .get("startLine")
        .or_else(|| input.get("start_line"))
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());
    let end_line = input
        .get("endLine")
        .or_else(|| input.get("end_line"))
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok());

    let (snippets, payload) = match tool_name {
        "knowledge_tree" => {
            match self::reference::sql_knowledge_tree_for_tool(
                state,
                &claims.tenant_id,
                datasource_id,
                Some(&query),
                limit,
            )
            .await
            {
                Ok(payload) => (Vec::new(), payload),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        datasource_id,
                        tool = %tool_name,
                        "nl2sql SQL knowledge tree tool failed"
                    );
                    (
                        Vec::new(),
                        json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                    )
                }
            }
        }
        "knowledge_rg" => {
            match self::reference::sql_knowledge_rg_for_tool(
                state,
                &claims.tenant_id,
                datasource_id,
                &query,
                filename,
                limit,
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        datasource_id,
                        tool = %tool_name,
                        "nl2sql SQL knowledge rg tool failed"
                    );
                    (
                        Vec::new(),
                        json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                    )
                }
            }
        }
        "knowledge_list" | "knowledge_search" => {
            match self::reference::resolve_auto_query_references(
                state,
                &claims.tenant_id,
                datasource_id,
                &query,
                limit,
            )
            .await
            {
                Ok(snippets) => {
                    let payload =
                        sql_knowledge_tool_payload(tool_name, &query, &snippets, content_max_chars);
                    (snippets, payload)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        datasource_id,
                        tool = %tool_name,
                        "nl2sql SQL knowledge tool execution failed"
                    );
                    (
                        Vec::new(),
                        json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                    )
                }
            }
        }
        "knowledge_read" | "sql_example_open" => {
            if let Some(file_id) = file_id.as_deref() {
                match self::reference::sql_knowledge_read_for_tool(
                    state,
                    &claims.tenant_id,
                    datasource_id,
                    file_id,
                    start_line.or_else(|| (tool_name == "sql_example_open").then_some(1)),
                    end_line,
                    content_max_chars,
                )
                .await
                {
                    Ok(result) => result,
                    Err(exact_error) => match self::reference::sql_knowledge_rg_for_tool(
                        state,
                        &claims.tenant_id,
                        datasource_id,
                        &query,
                        filename,
                        limit,
                    )
                    .await
                    {
                        Ok((snippets, search_payload)) => {
                            if snippets.is_empty() {
                                tracing::warn!(
                                    miss_reason = %exact_error,
                                    datasource_id,
                                    tool = %tool_name,
                                    file_id,
                                    "nl2sql SQL knowledge exact read missed and fallback found no matching evidence"
                                );
                            } else {
                                tracing::info!(
                                    miss_reason = %exact_error,
                                    datasource_id,
                                    tool = %tool_name,
                                    file_id,
                                    "nl2sql SQL knowledge identifier miss recovered with full-text search"
                                );
                            }
                            (
                                snippets,
                                json!({
                                    "tool": tool_name,
                                    "query": query,
                                    "exactFileId": file_id,
                                    "exactReadError": exact_error.to_string(),
                                    "fallback": "knowledge_rg",
                                    "result": search_payload,
                                }),
                            )
                        }
                        Err(search_error) => {
                            tracing::warn!(
                                exact_error = %exact_error,
                                search_error = %search_error,
                                datasource_id,
                                tool = %tool_name,
                                "nl2sql SQL knowledge exact read and fallback search failed"
                            );
                            (
                                Vec::new(),
                                json!({
                                    "tool": tool_name,
                                    "fileId": file_id,
                                    "query": query,
                                    "error": exact_error.to_string(),
                                    "fallbackError": search_error.to_string(),
                                }),
                            )
                        }
                    },
                }
            } else {
                let resolved = self::reference::sql_knowledge_rg_for_tool(
                    state,
                    &claims.tenant_id,
                    datasource_id,
                    &query,
                    filename,
                    limit,
                )
                .await;
                match resolved {
                    Ok((snippets, _)) => {
                        let payload = sql_knowledge_tool_payload(
                            tool_name,
                            &query,
                            &snippets,
                            content_max_chars,
                        );
                        (snippets, payload)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            datasource_id,
                            tool = %tool_name,
                            "nl2sql SQL knowledge read fallback search failed"
                        );
                        (
                            Vec::new(),
                            json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                        )
                    }
                }
            }
        }
        "knowledge_outline" => {
            match self::reference::sql_knowledge_outline_for_tool(
                state,
                &claims.tenant_id,
                datasource_id,
                file_id.as_deref(),
                &query,
                limit,
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        datasource_id,
                        tool = %tool_name,
                        "nl2sql SQL knowledge outline tool failed"
                    );
                    (
                        Vec::new(),
                        json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                    )
                }
            }
        }
        "knowledge_related" => {
            match self::reference::sql_knowledge_related_for_tool(
                state,
                &claims.tenant_id,
                datasource_id,
                file_id.as_deref(),
                &query,
                limit,
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        datasource_id,
                        tool = %tool_name,
                        "nl2sql SQL knowledge related tool failed"
                    );
                    (
                        Vec::new(),
                        json!({ "tool": tool_name, "query": query, "error": e.to_string() }),
                    )
                }
            }
        }
        "schema_search" => {
            let summary = schema_search_summary(schema, &query, limit);
            let snippets = if summary.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                vec![ReferencePromptSnippet {
                    pack_id: "live-schema".to_string(),
                    pack_name: "Live Schema".to_string(),
                    file_id: "schema_search".to_string(),
                    filename: "schema_search.json".to_string(),
                    chunk_id: format!("schema-search-{}", uuid::Uuid::new_v4()),
                    language: Some("json".to_string()),
                    start_line: 1,
                    end_line: 1,
                    score: 1.0,
                    reason: format!("schema_search tool query: {query}"),
                    chunk_type: "schema_reference".to_string(),
                    verified: true,
                    stale: false,
                    content: summary.to_string(),
                }]
            } else {
                Vec::new()
            };
            let payload = json!({
                "tool": tool_name,
                "query": query,
                "count": snippets.len(),
                "items": summary,
            });
            (snippets, payload)
        }
        other => (
            Vec::new(),
            json!({ "tool": other, "query": query, "error": "unknown tool" }),
        ),
    };

    let snippets = if tool_name == "schema_search" {
        snippets
    } else {
        filter_sql_knowledge_snippets_by_core_metric(fallback_question, snippets)
    };
    let payload = if matches!(
        tool_name,
        "knowledge_rg"
            | "knowledge_list"
            | "knowledge_search"
            | "knowledge_read"
            | "sql_example_open"
            | "knowledge_outline"
            | "knowledge_related"
    ) {
        sql_knowledge_tool_payload(tool_name, &query, &snippets, content_max_chars)
    } else {
        payload
    };

    if !snippets.is_empty() {
        self::reference::persist_sql_knowledge_usage_events(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            Some(datasource_id),
            usage_context,
            Some(&query),
            None,
            &snippets,
        )
        .await;
    }

    (snippets, payload)
}

async fn run_sql_knowledge_tool_prefetch(
    state: &AppState,
    claims: &Claims,
    datasource_id: &str,
    question: &str,
    schema: &serde_json::Value,
    chat_cfg: &crate::nl2sql::ChatTenantConfig,
) -> Vec<ReferencePromptSnippet> {
    let max_tool_rounds = sql_knowledge_tool_max_rounds();
    let max_tool_uses_per_round = sql_knowledge_tool_uses_per_round();
    let mut messages = vec![InputMessage {
        role: "user".to_string(),
        content: vec![InputContentBlock::Text {
            text: format!(
                "Question: {question}\n\nWork like Codex inside a trusted SQL knowledge workspace. Before SQL generation, freely inspect the virtual file set: start with knowledge_tree when the available files are unclear, use knowledge_rg repeatedly with different metric/table/business terms, then knowledge_read exact line ranges or sql_example_open for full SQL/CTE context. Use knowledge_outline for long SQL and knowledge_related for supporting metric definitions. Use schema_search only to verify live table/column facts. Do not answer the user here; gather enough context to generate the most accurate SQL."
            ),
        }],
    }];
    let mut out = Vec::new();

    for round in 0..max_tool_rounds {
        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens: 512,
            messages: messages.clone(),
            system: Some(
                "You are a Codex-like SQL workspace navigator inside AOS. Your job is to find and read the right SQL/Markdown evidence, not to answer. Use the tools like a virtual filesystem: knowledge_tree -> knowledge_rg with refined queries -> knowledge_read/sql_example_open -> knowledge_outline/knowledge_related -> schema_search for verification. Prefer opening full SQL examples before deciding. If the first search is weak, rewrite the search terms and try again. Never invent table or field names. Stop only when the retrieved knowledge is sufficient.".to_string(),
            ),
            tools: Some(sql_knowledge_tool_definitions()),
            tool_choice: Some(ToolChoice::Auto),
            stream: false,
            temperature: Some(0.0),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };

        let response = match chat_cfg.client.send_message(&request).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    datasource_id,
                    round = round + 1,
                    "nl2sql SQL knowledge tool prefetch failed; continuing with deterministic references"
                );
                break;
            }
        };

        let tool_uses: Vec<(String, String, serde_json::Value)> = response
            .content
            .into_iter()
            .filter_map(|block| match block {
                OutputContentBlock::ToolUse { id, name, input } => Some((id, name, input)),
                _ => None,
            })
            .take(max_tool_uses_per_round)
            .collect();

        if tool_uses.is_empty() {
            break;
        }

        messages.push(InputMessage {
            role: "assistant".to_string(),
            content: tool_uses
                .iter()
                .map(|(id, name, input)| InputContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                })
                .collect(),
        });

        for (tool_use_id, name, input) in tool_uses {
            let content_limit = sql_generation_tool_result_max_chars(&name);
            let (mut snippets, payload) = execute_sql_knowledge_tool(
                state,
                claims,
                datasource_id,
                question,
                schema,
                &name,
                &input,
                "tool_prefetch",
                content_limit,
            )
            .await;
            out.append(&mut snippets);

            let payload_text = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize tool result\"}".to_string());
            messages.push(InputMessage::user_tool_result(
                tool_use_id,
                compact_tool_text(&payload_text, content_limit),
                false,
            ));
        }
    }

    merge_reference_snippets(&[], out, sql_knowledge_prompt_max_snippets())
}

async fn send_generation_request_with_sql_tools(
    state: &AppState,
    claims: &Claims,
    datasource_id: Option<&str>,
    question: &str,
    schema: &serde_json::Value,
    chat_cfg: &crate::nl2sql::ChatTenantConfig,
    request: &MessageRequest,
    allow_tool_loop: bool,
) -> std::result::Result<(api::MessageResponse, Vec<ReferencePromptSnippet>), api::ApiError> {
    let Some(datasource_id) = datasource_id else {
        return chat_cfg
            .client
            .send_message(request)
            .await
            .map(|response| (response, Vec::new()));
    };
    if !allow_tool_loop || !should_enable_sql_generation_tool_loop() {
        return chat_cfg
            .client
            .send_message(request)
            .await
            .map(|response| (response, Vec::new()));
    }

    let mut messages = request.messages.clone();
    let mut gathered = Vec::new();
    let mut working = request.clone();
    working.tools = Some(sql_knowledge_tool_definitions());
    working.tool_choice = Some(ToolChoice::Auto);
    working.system = request.system.as_ref().map(|system| {
        format!(
            "{system}\n\nYou may call SQL Knowledge tools before final SQL generation. Treat them like a Codex-style virtual folder: list files, rg exact terms, read line ranges, open full SQL examples, inspect outlines, find related definitions, then verify against live schema. Do not rely on a single retrieved chunk when a full SQL file or related metric definition is available. If search results are weak, refine the query and search again. SQL example literals are parameters, not current user filters: do not inherit dates, experiment IDs, app/product identifiers, countries, versions, cohorts, or other predicates unless the current question or confirmed context explicitly supplies them. Return a single final SQL statement only after the evidence is sufficient."
        )
    });
    let mut remaining_tool_result_chars = sql_generation_tool_total_result_max_chars();

    for round in 0..sql_generation_tool_max_rounds() {
        working.messages = messages.clone();
        let response = chat_cfg.client.send_message(&working).await?;
        let tool_uses: Vec<(String, String, serde_json::Value)> = response
            .content
            .iter()
            .filter_map(|block| match block {
                OutputContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .take(sql_knowledge_tool_uses_per_round())
            .collect();

        if tool_uses.is_empty() {
            return Ok((
                response,
                merge_reference_snippets(&[], gathered, sql_knowledge_prompt_max_snippets()),
            ));
        }

        tracing::info!(
            datasource_id,
            round = round + 1,
            tool_use_count = tool_uses.len(),
            "nl2sql SQL generation tool loop requested context"
        );
        messages.push(InputMessage {
            role: "assistant".to_string(),
            content: tool_uses
                .iter()
                .map(|(id, name, input)| InputContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                })
                .collect(),
        });

        for (tool_use_id, name, input) in tool_uses {
            if remaining_tool_result_chars == 0 {
                messages.push(InputMessage::user_tool_result(
                    tool_use_id,
                    "{\"error\":\"SQL knowledge tool result budget exhausted; generate SQL from the evidence already returned\"}".to_string(),
                    false,
                ));
                continue;
            }
            let content_limit =
                sql_generation_tool_result_max_chars(&name).min(remaining_tool_result_chars);
            let (mut snippets, payload) = execute_sql_knowledge_tool(
                state,
                claims,
                datasource_id,
                question,
                schema,
                &name,
                &input,
                "generation_tool_loop",
                content_limit,
            )
            .await;
            gathered.append(&mut snippets);

            let payload_text = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize tool result\"}".to_string());
            let compact_payload = compact_tool_text(&payload_text, content_limit);
            remaining_tool_result_chars =
                remaining_tool_result_chars.saturating_sub(compact_payload.chars().count());
            messages.push(InputMessage::user_tool_result(
                tool_use_id,
                compact_payload,
                false,
            ));
        }
        if remaining_tool_result_chars == 0 {
            tracing::info!(
                datasource_id,
                round = round + 1,
                total_result_max_chars = sql_generation_tool_total_result_max_chars(),
                "nl2sql SQL generation tool result budget exhausted; requesting final SQL"
            );
            break;
        }
    }

    let mut final_request = request.clone();
    messages.push(InputMessage {
        role: "user".to_string(),
        content: vec![InputContentBlock::Text {
            text: "Tool-call budget is exhausted. Generate the final answer now: return only a single safe SELECT SQL statement, or CLARIFICATION_NEEDED: <question> if the SQL cannot be made reliable.".to_string(),
        }],
    });
    final_request.messages = messages;
    final_request.system = working.system.clone();
    final_request.tools = None;
    final_request.tool_choice = None;
    let response = chat_cfg.client.send_message(&final_request).await?;
    Ok((
        response,
        merge_reference_snippets(&[], gathered, sql_knowledge_prompt_max_snippets()),
    ))
}

fn append_canonical_semantic_intent(system_prompt: &mut String, semantic_intent_json: &str) {
    crate::behavior_trace("SQL-001");
    debug_assert!(!semantic_intent_json.trim().is_empty());
    system_prompt.push_str(
        "\n\nCanonical analytic intent (authoritative semantic input; do not ignore or silently broaden it):\n",
    );
    system_prompt.push_str(semantic_intent_json);
    system_prompt.push_str(
        "\nGenerate SQL that satisfies every resolved metric, dimension, population, time and unresolved-field constraint. If an unresolved field changes the result, return CLARIFICATION_NEEDED instead of guessing.",
    );
}

pub(crate) async fn generate_sql(
    state: &AppState,
    claims: &Claims,
    datasource_id: Option<&str>,
    question: &str,
    schema: &serde_json::Value,
    foreign_keys: &[ForeignKeyPrompt],
    join_paths: &[(String, String)], // (path_text, sql_joins) per source→target pair
    history: ConversationHistory,
    clarification_ctx: Option<&crate::nl2sql::ClarificationContext>,
    qu_result: Option<&crate::nl2sql::query_understanding::QueryUnderstandingResult>,
    db_type: &str,
    large_schema_mode: bool,
    // P1-2: Reusable business metrics for SQL generation prompt injection
    metrics: &[(String, String, Option<&str>)], // (name, expression, filter_conditions)
    matched_metrics: &[String],
    reference_snippets: &[ReferencePromptSnippet],
    business_domain_context: Option<&str>,
    preferred_model: Option<&str>,
    allow_tool_loop: bool,
    semantic_intent_json: &str,
) -> Result<GenerateSqlResult> {
    if semantic_intent_json.trim().is_empty() {
        return Err(AppError::ValidationError(
            "canonical analytic intent is required before SQL generation".to_string(),
        ));
    }
    let gen_started = std::time::Instant::now();
    let mut chat_candidates = crate::nl2sql::resolve_chat_config_candidates(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    .map_err(|e| AppError::Internal(format!("failed to create LLM client: {e}")))?;
    crate::nl2sql::prioritize_chat_candidates(&mut chat_candidates, preferred_model);

    if chat_candidates.is_empty() {
        return Err(AppError::Internal(
            "failed to create LLM client: no candidate API keys".to_string(),
        ));
    }
    let total_candidates = chat_candidates.len();

    let mut key_name_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for key_id in chat_candidates.iter().filter_map(|c| c.key_id.as_deref()) {
        if key_name_by_id.contains_key(key_id) {
            continue;
        }
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM api_keys WHERE id = ? AND tenant_id = ? LIMIT 1",
        )
        .bind(key_id)
        .bind(&claims.tenant_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| key_id.to_string());
        key_name_by_id.insert(key_id.to_string(), name);
    }

    tracing::info!(
        tenant_id = %claims.tenant_id,
        candidate_count = total_candidates,
        "nl2sql generate_sql resolved candidate chat keys"
    );

    // P1-4: Two-pass schema generation for large schemas.
    // Pass 1 (table selection): ask LLM to pick the most relevant tables from the overview.
    // Pass 2 (SQL generation): inject full column details ONLY for selected tables.
    //
    // This prevents context overflow for schemas with >20 tables by ensuring the LLM
    // only sees column details for tables it actually needs.
    let pass1_started = std::time::Instant::now();
    let selected_tables: Vec<String> = if large_schema_mode {
        let pass1_candidate = chat_candidates
            .iter()
            .find(|candidate| !nl2sql_candidate_is_suppressed(&claims.tenant_id, candidate))
            .unwrap_or(&chat_candidates[0]);
        const MAX_SELECTED: usize = 8;
        const MAX_TABLE_NAME_LEN: usize = 128;
        let overview_prompt = build_schema_overview_prompt(schema, db_type);
        let mut pass1_system =
            "You are a precise table selector. Respond with ONLY a JSON array of table names."
                .to_string();
        if let Some(context) = business_domain_context.filter(|value| !value.trim().is_empty()) {
            pass1_system.push_str("\n\n");
            pass1_system.push_str(context);
        }
        let pass1_request = MessageRequest {
            model: pass1_candidate.model.clone(),
            max_tokens: 128,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: format!(
                        "{}\n\nUser question: {}\n\nSelect the most relevant tables (max {}):",
                        overview_prompt, question, MAX_SELECTED
                    ),
                }],
            }],
            system: Some(pass1_system),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.0),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body: None,
        };
        match pass1_candidate.client.send_message(&pass1_request).await {
            Ok(resp) => {
                let text = resp
                    .content
                    .iter()
                    .find_map(|b| match b {
                        OutputContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let parsed: Vec<String> = serde_json::from_str(&text).unwrap_or_default();
                let valid: Vec<String> = parsed
                    .into_iter()
                    .filter(|t| !t.is_empty() && t.len() <= MAX_TABLE_NAME_LEN)
                    .take(MAX_SELECTED)
                    .collect();
                tracing::info!(
                    schema_tables = schema.as_array().map(|a| a.len()).unwrap_or(0),
                    selected = valid.len(),
                    "P1-4 Pass 1: table selection returned {} tables",
                    valid.len()
                );
                valid
            }
            Err(e) => {
                tracing::warn!("P1-4 Pass 1 failed: {}, falling back to all tables", e);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    tracing::info!(
        large_schema_mode,
        selected_tables = selected_tables.len(),
        elapsed_ms = pass1_started.elapsed().as_millis() as u64,
        "nl2sql generate_sql pass1 finished"
    );

    // R-9: weave coreference resolution against the most recent turn into the
    // summary slot so follow-ups like "上月呢" / "排除退货的" / "只看 VIP"
    // carry their implicit context into the LLM prompt.
    let enriched_summary = history.enriched_summary(question);
    let mut system_prompt = build_nl2sql_prompt(
        schema,
        foreign_keys,
        join_paths,
        enriched_summary.as_deref(),
        clarification_ctx,
        qu_result,
        db_type,
        large_schema_mode,
        metrics,
        if selected_tables.is_empty() {
            None
        } else {
            Some(&selected_tables)
        },
        reference_snippets,
    );
    if let Some(context) = business_domain_context.filter(|value| !value.trim().is_empty()) {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(context);
    }
    append_canonical_semantic_intent(&mut system_prompt, semantic_intent_json);

    let history_section = if history.messages.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = history
            .messages
            .iter()
            .rev()
            .map(|(q, a)| serde_json::json!({ "Q": q, "A": a }).to_string())
            .collect();
        format!("\nConversation history:\n{}\n\n", lines.join("\n"))
    };
    let question_with_metric_hint =
        augment_question_for_metric_generation(question, matched_metrics);
    let user_prompt =
        format!("{history_section}Question: {question_with_metric_hint}\n\nGenerate SQL:");

    // BUG-FIX: Adaptive max_tokens based on schema complexity.
    // Complex schemas (large_schema_mode or >30 tables) need more tokens for full SQL output.
    // Fixed 1024 tokens was insufficient for schemas with many columns and QU constraints.
    let table_count = schema.as_array().map(|a| a.len()).unwrap_or(0);
    let default_complex_tokens = std::env::var("NL2SQL_COMPLEX_SQL_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v >= 8_192)
        .unwrap_or(24_576);
    let deadline_bounded = !allow_tool_loop;
    let task_max_tokens: u32 = if deadline_bounded {
        std::env::var("NL2SQL_BOUNDED_SQL_MAX_TOKENS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .map(|value| value.clamp(2_048, 16_384))
            .unwrap_or(8_192)
    } else if large_schema_mode || table_count > 30 {
        default_complex_tokens
    } else if table_count > 10 {
        16_384
    } else {
        12_288
    };

    // Retry budget: 3 attempts with 1s / 2s backoff between them.
    // Covers both transient network/5xx LLM failures and shape errors
    // where the model ignored the "SQL-only" system prompt and returned
    // markdown or commentary that fails `is_safe_sql`. On the final
    // attempt we return the best error we saw rather than a generic
    // "LLM call failed".
    // P2-3: Tracing span for SQL generation
    let _sql_gen_span = tracing::info_span!(
        "nl2sql_sql_generation",
        db_type = %db_type,
        schema_tables = schema.get("tables").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        history_len = history.messages.len(),
    );

    const ATTEMPTS: u32 = 3;
    let mut last_err: Option<String> = None;
    let mut quota_failed_key_labels: Vec<String> = Vec::new();
    for (candidate_idx, chat_cfg) in chat_candidates.into_iter().enumerate() {
        let key_label = chat_cfg
            .key_id
            .as_deref()
            .and_then(|id| key_name_by_id.get(id).cloned())
            .or(chat_cfg.key_id.clone())
            .unwrap_or_else(|| "env-fallback".to_string());
        if total_candidates > 1
            && candidate_idx + 1 < total_candidates
            && nl2sql_candidate_is_suppressed(&claims.tenant_id, &chat_cfg)
        {
            tracing::info!(
                candidate_index = candidate_idx + 1,
                total_candidates,
                key_name = %key_label,
                provider = %chat_cfg.provider,
                model = %chat_cfg.model,
                "nl2sql generate_sql skipping temporarily suppressed unusable candidate"
            );
            continue;
        }
        let respect_suppression = total_candidates > 1 && candidate_idx + 1 < total_candidates;
        let Some(candidate_attempt) =
            acquire_nl2sql_candidate_attempt(&claims.tenant_id, &chat_cfg, respect_suppression)
                .await
        else {
            tracing::info!(
                candidate_index = candidate_idx + 1,
                total_candidates,
                key_name = %key_label,
                provider = %chat_cfg.provider,
                model = %chat_cfg.model,
                "nl2sql generate_sql skipped candidate after waiting for its health probe"
            );
            continue;
        };
        tracing::info!(
            candidate_index = candidate_idx + 1,
            total_candidates,
            key_id = ?chat_cfg.key_id,
            key_name = %key_label,
            provider = %chat_cfg.provider,
            model = %chat_cfg.model,
            "nl2sql generate_sql trying chat key"
        );

        let max_tokens = task_max_tokens.min(chat_cfg.max_output_tokens).max(1);
        let mut extra_body = None;
        if deadline_bounded
            && api::supports_official_deepseek_v4_thinking_control(
                &chat_cfg.model,
                chat_cfg.client.base_url(),
            )
        {
            extra_body = Some(serde_json::Map::from_iter([(
                "thinking".to_string(),
                serde_json::json!({"type": "disabled"}),
            )]));
        }
        let request = MessageRequest {
            model: chat_cfg.model.clone(),
            max_tokens,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: user_prompt.clone(),
                }],
            }],
            system: Some(system_prompt.clone()),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: Some(0.0),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            reasoning_effort: None,
            include_reasoning: None,
            use_max_completion_tokens: None,
            extra_body,
        };

        let mut move_next_key = false;
        let mut shape_repair_hint: Option<String> = None;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                let backoff_secs = 1u64 << (attempt - 1); // 1s, 2s
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            }

            let mut attempt_request = request.clone();
            if let Some(hint) = shape_repair_hint.as_deref() {
                attempt_request.messages.push(InputMessage {
                    role: "user".to_string(),
                    content: vec![InputContentBlock::Text {
                        text: format!(
                            "Your previous response was rejected before database execution: {hint}\n\
                             Retry the original request now. Return exactly one complete executable SELECT or WITH statement as plain text. \
                             Do not include analysis, Markdown fences, JSON, labels, or text before/after the SQL. \
                             If the request truly cannot be resolved from the supplied evidence, return exactly CLARIFICATION_NEEDED: <specific question>."
                        ),
                    }],
                });
            }

            let attempt_started = std::time::Instant::now();
            let (response, tool_reference_snippets) = match send_generation_request_with_sql_tools(
                state,
                claims,
                datasource_id,
                question,
                schema,
                &chat_cfg,
                &attempt_request,
                allow_tool_loop,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    if is_non_retryable_key_error(&e.to_string()) {
                        let msg = format!(
                            "LLM call failed on key \"{key_label}\": non-retryable provider/config error: {e}"
                        );
                        tracing::warn!(
                            candidate_index = candidate_idx + 1,
                            total_candidates,
                            key_name = %key_label,
                            error = %e,
                            elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                            "NL2SQL key has non-retryable provider/config error, failing over to next key"
                        );
                        if total_candidates > 1 {
                            suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
                        }
                        last_err = Some(msg);
                        move_next_key = true;
                        break;
                    }
                    if is_quota_or_billing_error(&e.to_string()) {
                        tracing::warn!(
                            candidate_index = candidate_idx + 1,
                            total_candidates,
                            key_name = %key_label,
                            error = %e,
                            elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                            "NL2SQL key is out of quota/billing, failing over to next key"
                        );
                        if total_candidates > 1 {
                            suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
                        }
                        quota_failed_key_labels.push(key_label.clone());
                        last_err = Some(format!("LLM call failed on key \"{key_label}\": {e}"));
                        move_next_key = true;
                        break;
                    }
                    let msg = format!("LLM call failed on key \"{key_label}\": {e}");
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        error = %e,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL generate attempt failed"
                    );
                    last_err = Some(msg);
                    if total_candidates > 1 {
                        suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
                        move_next_key = true;
                        break;
                    }
                    continue;
                }
            };

            let text_content = collect_output_text(&response.content);

            let sql = extract_sql_from_llm_output(&text_content);
            // Detect clarification request from LLM
            if let Some(rest) = sql.strip_prefix("CLARIFICATION_NEEDED:") {
                if llm_clarification_reasks_metric(rest.trim(), question, metrics, matched_metrics)
                {
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        clarification = %rest.trim(),
                        "LLM asked redundant metric clarification despite explicit metric mention; retrying"
                    );
                    last_err = Some(
                        "LLM asked redundant metric clarification despite explicit metric mention"
                            .to_string(),
                    );
                    shape_repair_hint = Some(
                        "the response asked for a metric that is already explicit in the question"
                            .to_string(),
                    );
                    continue;
                }
                mark_nl2sql_candidate_success(
                    &claims.tenant_id,
                    &chat_cfg,
                    candidate_attempt.failure_generation,
                );
                return Ok(GenerateSqlResult {
                    sql: String::new(),
                    clarification_question: Some(rest.trim().to_string()),
                    usage: Some(response.usage.clone()),
                    model: Some(chat_cfg.model.clone()),
                    api_key_id: chat_cfg.key_id.clone(),
                    provider: Some(chat_cfg.provider.clone()),
                    tool_reference_snippets,
                });
            }
            if sql.is_empty() {
                let thinking_only_length =
                    is_thinking_only_length_response(&response, &text_content);
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
                last_err = Some("LLM returned empty response".to_owned());
                shape_repair_hint = Some(
                    "the response contained no usable SQL text; make sure the full statement is present"
                        .to_string(),
                );
                tracing::warn!(
                    candidate_index = candidate_idx + 1,
                    total_candidates,
                    attempt = attempt + 1,
                    key_name = %key_label,
                    elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                    failover = total_candidates > 1,
                    stop_reason = ?response.stop_reason,
                    content_block_types = %content_block_types,
                    content_block_count = response.content.len(),
                    text_chars = text_content.chars().count(),
                    input_tokens = response.usage.input_tokens,
                    output_tokens = response.usage.output_tokens,
                    "NL2SQL LLM returned empty response"
                );
                self::agent_async::emit_agent_stage_detail(
                    "generate_sql_retry",
                    "模型未返回可用 SQL，正在收紧输出格式后重试",
                    serde_json::json!({
                        "kind": "sql_generation_retry",
                        "attempt": attempt + 1,
                        "status": "retrying",
                        "reason": "empty_response",
                    }),
                );
                if total_candidates > 1 {
                    suppress_nl2sql_candidate(&claims.tenant_id, &chat_cfg);
                    if thinking_only_length {
                        tracing::info!(
                            tenant_id = %claims.tenant_id,
                            key_name = %key_label,
                            provider = %chat_cfg.provider,
                            model = %chat_cfg.model,
                            cooldown_secs = nl2sql_candidate_suppression_secs(),
                            "temporarily suppressing NL2SQL candidate after thinking-only length truncation"
                        );
                    }
                }
                if total_candidates > 1 {
                    move_next_key = true;
                    break;
                }
                continue;
            }
            match classify_sql(&sql) {
                SqlSafetyResult::Safe => {}
                SqlSafetyResult::SyntaxError { message } => {
                    last_err = Some(format!(
                        "[syntax_error] LLM returned unparseable SQL ({}): {}",
                        message,
                        sql.chars().take(160).collect::<String>()
                    ));
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL LLM returned syntactically invalid SQL"
                    );
                    self::agent_async::emit_agent_stage_detail(
                        "generate_sql_retry",
                        "模型返回的 SQL 未通过语法校验，正在携带解析错误重试",
                        serde_json::json!({
                            "kind": "sql_generation_retry",
                            "attempt": attempt + 1,
                            "status": "retrying",
                            "reason": "syntax_error",
                            "error": message.clone(),
                            "sql": sql.chars().take(12_000).collect::<String>(),
                        }),
                    );
                    shape_repair_hint = Some(format!(
                        "the SQL parser reported {message}; return a complete statement with balanced quotes and parentheses"
                    ));
                    continue;
                }
                SqlSafetyResult::ForbiddenOperation { statement_type } => {
                    last_err = Some(format!(
                        "[forbidden_operation] LLM returned a non-SELECT statement ({statement_type}). \
                         Only SELECT statements are permitted."
                    ));
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL LLM returned a forbidden statement type"
                    );
                    self::agent_async::emit_agent_stage_detail(
                        "generate_sql_retry",
                        "模型返回了非只读语句，正在按只读 SQL 约束重试",
                        serde_json::json!({
                            "kind": "sql_generation_retry",
                            "attempt": attempt + 1,
                            "status": "retrying",
                            "reason": "forbidden_operation",
                            "statementType": statement_type.clone(),
                        }),
                    );
                    shape_repair_hint = Some(format!(
                        "the response used forbidden statement type {statement_type}; only a read-only SELECT or WITH statement is allowed"
                    ));
                    continue;
                }
                SqlSafetyResult::MultipleStatements => {
                    last_err = Some(
                        "[multiple_statements] LLM returned multiple statements. \
                         Only a single SELECT statement is allowed."
                            .to_owned(),
                    );
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL LLM returned multiple statements"
                    );
                    self::agent_async::emit_agent_stage_detail(
                        "generate_sql_retry",
                        "模型返回了多条 SQL，正在合并为单条只读查询后重试",
                        serde_json::json!({
                            "kind": "sql_generation_retry",
                            "attempt": attempt + 1,
                            "status": "retrying",
                            "reason": "multiple_statements",
                        }),
                    );
                    shape_repair_hint = Some(
                        "the response contained multiple SQL statements; combine the analysis into one SELECT or WITH statement"
                            .to_string(),
                    );
                    continue;
                }
                SqlSafetyResult::ForbiddenFunction { function_name } => {
                    last_err = Some(format!(
                        "[forbidden_function] LLM emitted a forbidden function call ({function_name}). \
                         This function is not permitted in NL2SQL queries."
                    ));
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        function = %function_name,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL LLM emitted a forbidden function"
                    );
                    self::agent_async::emit_agent_stage_detail(
                        "generate_sql_retry",
                        "模型 SQL 使用了受限函数，正在改写为安全只读查询",
                        serde_json::json!({
                            "kind": "sql_generation_retry",
                            "attempt": attempt + 1,
                            "status": "retrying",
                            "reason": "forbidden_function",
                            "functionName": function_name.clone(),
                        }),
                    );
                    shape_repair_hint = Some(format!(
                        "the response used forbidden function {function_name}; rewrite the query using safe read-only SQL functions"
                    ));
                    continue;
                }
                SqlSafetyResult::ForbiddenIntoClause => {
                    last_err = Some(
                        "[forbidden_into_clause] LLM emitted INTO OUTFILE / INTO DUMPFILE. \
                         This writes to the database server's filesystem and is not permitted."
                            .to_owned(),
                    );
                    tracing::warn!(
                        candidate_index = candidate_idx + 1,
                        total_candidates,
                        attempt = attempt + 1,
                        key_name = %key_label,
                        elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                        "NL2SQL LLM emitted INTO OUTFILE / INTO DUMPFILE"
                    );
                    self::agent_async::emit_agent_stage_detail(
                        "generate_sql_retry",
                        "模型 SQL 包含写出语句，正在移除写操作后重试",
                        serde_json::json!({
                            "kind": "sql_generation_retry",
                            "attempt": attempt + 1,
                            "status": "retrying",
                            "reason": "forbidden_into_clause",
                        }),
                    );
                    shape_repair_hint = Some(
                        "the response attempted to write a file with an INTO clause; return a read-only query without INTO"
                            .to_string(),
                    );
                    continue;
                }
            }
            tracing::info!(
                candidate_index = candidate_idx + 1,
                total_candidates,
                attempt = attempt + 1,
                key_name = %key_label,
                elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                total_elapsed_ms = gen_started.elapsed().as_millis() as u64,
                "nl2sql generate_sql succeeded"
            );
            mark_nl2sql_candidate_success(
                &claims.tenant_id,
                &chat_cfg,
                candidate_attempt.failure_generation,
            );
            return Ok(GenerateSqlResult {
                sql,
                clarification_question: None,
                usage: Some(response.usage.clone()),
                model: Some(chat_cfg.model.clone()),
                api_key_id: chat_cfg.key_id.clone(),
                provider: Some(chat_cfg.provider.clone()),
                tool_reference_snippets,
            });
        }

        if move_next_key {
            continue;
        }
        tracing::warn!(
            candidate_index = candidate_idx + 1,
            total_candidates,
            key_name = %key_label,
            "NL2SQL candidate exhausted retries, trying next key"
        );
    }

    tracing::error!(
        total_elapsed_ms = gen_started.elapsed().as_millis() as u64,
        "nl2sql generate_sql failed after retries"
    );

    if !quota_failed_key_labels.is_empty() {
        quota_failed_key_labels.sort();
        quota_failed_key_labels.dedup();
        notify_nl2sql_key_quota_notification(state, claims, &quota_failed_key_labels).await;
        return Err(AppError::Internal(format!(
            "LLM API key(s) [{}] are out of quota or billing balance. Please recharge or switch API keys in API Keys.",
            quota_failed_key_labels.join(", ")
        )));
    }

    Err(AppError::Internal(last_err.unwrap_or_else(|| {
        "NL2SQL generation failed after 3 attempts".to_owned()
    })))
}

fn should_enable_sql_semantic_review() -> bool {
    std::env::var("NL2SQL_ENABLE_SQL_SEMANTIC_REVIEW")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

fn sql_semantic_review_timeout_secs() -> u64 {
    std::env::var("NL2SQL_SQL_SEMANTIC_REVIEW_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 5)
        .unwrap_or(45)
        .min(120)
}

fn extract_json_object_from_text(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then(|| text[start..=end].to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SqlSemanticReviewResponse {
    verdict: String,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    issues: Vec<String>,
}

#[derive(Debug)]
struct SqlSemanticReviewOutcome {
    reviewed_sql: Option<String>,
    issues: Vec<String>,
    verdict: String,
}

async fn review_generated_sql_semantics(
    state: &AppState,
    claims: &Claims,
    question: &str,
    sql: &str,
    schema: &serde_json::Value,
    db_type: &str,
    reference_snippets: &[ReferencePromptSnippet],
) -> Option<SqlSemanticReviewOutcome> {
    if !should_enable_sql_semantic_review() {
        return None;
    }

    #[derive(Serialize)]
    struct ReviewInput<'a> {
        question: &'a str,
        db_type: &'a str,
        sql: &'a str,
        schema: &'a serde_json::Value,
        references: Vec<serde_json::Value>,
    }

    let references = reference_snippets
        .iter()
        .take(sql_knowledge_prompt_max_snippets())
        .map(|snippet| {
            json!({
                "file": snippet.filename,
                "lines": [snippet.start_line, snippet.end_line],
                "type": snippet.chunk_type,
                "score": snippet.score,
                "verified": snippet.verified,
                "stale": snippet.stale,
                "reason": snippet.reason,
                "content": compact_tool_text(&snippet.content, 12_000),
            })
        })
        .collect::<Vec<_>>();

    let prompt = match serde_json::to_string(&ReviewInput {
        question,
        db_type,
        sql,
        schema,
        references,
    }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "nl2sql semantic SQL review prompt serialization failed");
            return None;
        }
    };

    let chat_cfg = match crate::nl2sql::resolve_chat_config(
        state.config_registry(),
        &claims.tenant_id,
        &claims.sub,
        &state.default_model,
        Some("nl2sql"),
    )
    .await
    {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(error = %e, "nl2sql semantic SQL review skipped: chat config unavailable");
            return None;
        }
    };

    let system = r#"You are a strict NL2SQL semantic reviewer for an enterprise data workspace.
Check whether the SQL actually answers the user's question using the live schema and SQL Knowledge references.

Rules:
- Prefer fixing the SQL over asking for clarification.
- SQL Knowledge examples are parameterized evidence. Reject or rewrite SQL that copies a fixed date, experiment ID, app/product identifier, country, version, cohort, or other literal predicate not explicitly present in the current question or confirmed context. Questions spanning multiple entities must not be narrowed to one example entity.
- If live schema is empty but high-relevance SQL Knowledge is present, treat those SQL examples and metric definitions as authoritative workspace context.
- Metadata discovery can be partial even when query access works. If a current high-relevance SQL example supplies an exact missing table or column, validate it through execution/correction instead of discarding the evidence; never invent identifiers.
- Verify metric formulas, date filters, grouping dimensions, joins, table/column compatibility, and whether the SQL returns aggregate/report rows or raw rows as requested.
- If the SQL is correct enough, return verdict "pass".
- If the SQL is wrong but fixable, return verdict "rewrite" and a complete single SELECT statement.
- Never return markdown. Never include explanations outside JSON.

Return JSON only:
{"verdict":"pass|rewrite","sql":null,"issues":["short issue"]}"#;

    let request = MessageRequest {
        model: chat_cfg.model.clone(),
        max_tokens: 8_192.min(chat_cfg.max_output_tokens).max(1),
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt }],
        }],
        system: Some(system.to_string()),
        tools: None,
        tool_choice: None,
        stream: false,
        temperature: Some(0.0),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        include_reasoning: None,
        use_max_completion_tokens: None,
        extra_body: None,
    };

    let response = match chat_cfg.client.send_message(&request).await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!(error = %e, "nl2sql semantic SQL review LLM call failed");
            return None;
        }
    };
    let text = response
        .content
        .iter()
        .find_map(|block| match block {
            OutputContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let json_text = extract_json_object_from_text(&text).unwrap_or(text);
    let parsed = match serde_json::from_str::<SqlSemanticReviewResponse>(&json_text) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(error = %e, "nl2sql semantic SQL review returned invalid JSON");
            return None;
        }
    };

    let verdict = parsed.verdict.trim().to_ascii_lowercase();
    if verdict == "rewrite" {
        if let Some(raw_sql) = parsed.sql.as_deref() {
            let reviewed_sql = extract_sql_from_llm_output(raw_sql);
            if !reviewed_sql.is_empty()
                && matches!(classify_sql(&reviewed_sql), SqlSafetyResult::Safe)
            {
                return Some(SqlSemanticReviewOutcome {
                    reviewed_sql: Some(reviewed_sql),
                    issues: parsed.issues,
                    verdict,
                });
            }
            tracing::warn!(
                verdict = %verdict,
                "nl2sql semantic SQL review rewrite was empty or unsafe; keeping generated SQL"
            );
        }
    }

    Some(SqlSemanticReviewOutcome {
        reviewed_sql: None,
        issues: parsed.issues,
        verdict,
    })
}

fn should_enable_sql_explain_preflight() -> bool {
    std::env::var("NL2SQL_ENABLE_SQL_EXPLAIN_PREFLIGHT")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

fn sql_explain_preflight_timeout_secs() -> u64 {
    std::env::var("NL2SQL_SQL_EXPLAIN_PREFLIGHT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 5)
        .unwrap_or(15)
        .min(120)
}

async fn explain_trino_or_presto_sql(
    state: &AppState,
    claims: &Claims,
    data_source_id: &str,
    sql: &str,
    request_budget: Arc<agent_executor::DatasourceRequestBudget>,
) -> std::result::Result<(), String> {
    #[derive(Debug, Deserialize)]
    struct TrinoConfig {
        host: String,
        port: u16,
        catalog: String,
        #[serde(default)]
        schema: String,
        username: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        ssl: Option<bool>,
        #[serde(default)]
        basic_auth: Option<bool>,
    }

    let config_json =
        sqlx::query_scalar::<_, serde_json::Value>("SELECT config FROM data_sources WHERE id = ?")
            .bind(data_source_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("load datasource config failed: {e}"))?
            .ok_or_else(|| "data source not found".to_string())?;
    let config_val = crate::routes::data_sources::decrypt_config(
        &config_json,
        &state.data_dir,
        &claims.tenant_id,
        data_source_id,
    )
    .map_err(|e| format!("decrypt datasource config failed: {e}"))?;
    let cfg: TrinoConfig = serde_json::from_value(config_val)
        .map_err(|e| format!("invalid trino/presto config: {e}"))?;
    let normalized_host = nl2sql_domain::datasource_config::normalize_host_input(&cfg.host);
    let port = normalized_host.port.unwrap_or(cfg.port);
    let secure = cfg.ssl.or(normalized_host.secure).unwrap_or(port == 443);
    let mut builder = trino_rust_client::ClientBuilder::new(&cfg.username, &normalized_host.host)
        .port(port)
        .catalog(&cfg.catalog)
        .schema(&cfg.schema)
        .secure(secure);
    if cfg.basic_auth.unwrap_or(!cfg.password.is_empty()) {
        builder = builder.auth(trino_rust_client::auth::Auth::Basic(
            cfg.username.clone(),
            Some(cfg.password.clone()),
        ));
    }
    let cli = builder
        .max_attempt(0)
        .build()
        .map_err(|e| format!("trino client build failed: {e}"))?;
    let explain_sql = format!("EXPLAIN {}", sql.trim().trim_end_matches(';').trim());
    agent_executor::execute_trino_query_bounded(
        cli,
        explain_sql,
        sql_explain_preflight_timeout_secs(),
        &claims.tenant_id,
        &claims.sub,
        "Trino SQL EXPLAIN preflight",
        request_budget,
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("EXPLAIN failed: {e}"))
}

fn is_quota_or_billing_error(msg: &str) -> bool {
    let text = msg.to_ascii_lowercase();
    [
        "insufficient_quota",
        "quota exceeded",
        "quota_exceeded",
        "insufficient balance",
        "insufficient_balance",
        "insufficient credits",
        "insufficient_credits",
        "credit balance",
        "billing",
        "payment required",
        "recharge",
        "余额不足",
        "额度不足",
        "欠费",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

async fn notify_nl2sql_key_quota_notification(
    state: &AppState,
    claims: &Claims,
    key_labels: &[String],
) {
    if key_labels.is_empty() {
        return;
    }

    let key_text = key_labels.join(", ");
    let title = "NL2SQL API Key 余额不足";
    let body = format!(
        "NL2SQL 调用失败：API Key [{}] 余额不足或已欠费。请到 API Keys 页面充值或切换可用密钥。",
        key_text
    );

    // De-duplicate within 10 minutes to avoid notification spam on repeated retries.
    let recent_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM notifications \
         WHERE tenant_id = ? AND user_id = ? AND title = ? AND body = ? \
           AND created_at >= datetime(CURRENT_TIMESTAMP, '-10 minutes')",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(title)
    .bind(&body)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0);

    if recent_count > 0 {
        return;
    }

    if let Err(e) = sqlx::query(
        "INSERT INTO notifications (id, tenant_id, user_id, title, body, level) \
         VALUES (uuid(), ?, ?, ?, ?, 'warning')",
    )
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(title)
    .bind(&body)
    .execute(&state.db)
    .await
    {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            user_id = %claims.sub,
            error = %e,
            "failed to insert NL2SQL quota notification"
        );
    }
}

fn is_non_retryable_key_error(msg: &str) -> bool {
    let text = msg.to_ascii_lowercase();
    let has_html_payload = text.contains("<!doctype html")
        || text.contains("<html")
        || text.contains("<head>")
        || text.contains("<meta charset=");
    let parse_openai_response = text.contains("failed to parse openai response");
    let json_parse_at_col1 = text.contains("expected value at line 1 column 1");
    // Typical when custom base_url points to a web page instead of an OpenAI-compatible API endpoint.
    has_html_payload || (parse_openai_response && json_parse_at_col1)
}

// Prompt builders moved to routes/nl2sql/prompts.rs.

/// Raw foreign key from the schema JSON (from discovery).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ForeignKeyRaw {
    pub source_table: String,
    pub source_column: String,
    pub source_column_type: String,
    pub target_table: String,
    pub target_column: String,
    pub target_column_type: String,
}

/// Extract the `tables` array and `foreign_keys` array from the enriched schema_info JSON.
///
/// The schema_info is now stored as:
/// `{ "tables": [...], "foreign_keys": [...] }`
///
/// For backward compatibility, also handles the legacy flat-array format `[{table_name, columns}]`.
pub(crate) fn extract_schema_tables_and_fks(
    schema_info: &serde_json::Value,
) -> (serde_json::Value, Vec<ForeignKeyRaw>) {
    // Try new enriched format first
    if let Some(obj) = schema_info.as_object() {
        let tables = obj
            .get("tables")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));
        let foreign_keys: Vec<ForeignKeyRaw> = obj
            .get("foreign_keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|fk| {
                        Some(ForeignKeyRaw {
                            source_table: fk.get("source_table")?.as_str()?.to_owned(),
                            source_column: fk.get("source_column")?.as_str()?.to_owned(),
                            source_column_type: fk
                                .get("source_column_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                            target_table: fk.get("target_table")?.as_str()?.to_owned(),
                            target_column: fk.get("target_column")?.as_str()?.to_owned(),
                            target_column_type: fk
                                .get("target_column_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (tables, foreign_keys);
    }

    // Legacy flat-array format: treat as just tables, no FKs
    if schema_info.as_array().is_some() {
        return (schema_info.clone(), Vec::new());
    }

    (serde_json::json!([]), Vec::new())
}

/// Enriches the raw schema JSON with AI-generated and user-edited semantic descriptions.
///
/// User descriptions take precedence over AI descriptions. The enriched schema includes:
/// - `ai_description` / `user_description` on each table
/// - `description` on each column (user > AI > null)
///
/// This ensures the LLM generating SQL sees semantic context — not just column names and types.
async fn enrich_schema_with_semantics(
    db: &sqlx::SqlitePool,
    datasource_id: &str,
    schema_tables: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let tables = schema_tables
        .as_array()
        .context("schema_tables must be a JSON array")?;

    // Batch-load all column semantics for this datasource in a single query.
    let col_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT table_name, column_name, COALESCE(semantic_description, '') \
         FROM nl2sql_table_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .context("failed to load column semantics")?;

    let col_map: std::collections::HashMap<(String, String), String> = col_rows
        .into_iter()
        .map(|(t, c, ai)| ((t, c), ai))
        .collect();

    // Batch-load all table-level semantics.
    let table_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, COALESCE(ai_description, '') \
         FROM nl2sql_table_desc_semantics WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .context("failed to load table-level semantics")?;

    let table_map: std::collections::HashMap<String, String> =
        table_rows.into_iter().map(|(t, ai)| (t, ai)).collect();

    // Batch-load column synonyms for this datasource in a single query.
    let synonym_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT term, canonical_table, canonical_column \
         FROM nl2sql_synonyms WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Build (canonical_table, canonical_column) → [synonyms] map.
    let mut synonym_map: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    for (term, canonical_table, canonical_column) in synonym_rows {
        synonym_map
            .entry((canonical_table, canonical_column))
            .or_default()
            .push(term);
    }

    let enriched: Vec<serde_json::Value> = tables
        .iter()
        .map(|table| {
            let mut enriched_table = table.clone();
            if let Some(obj) = enriched_table.as_object_mut() {
                // Extract table_name first (immutable borrow)
                let table_name = obj
                    .get("table_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned();
                // Enrich columns with semantic descriptions and synonyms.
                if let Some(cols) = obj.get_mut("columns").and_then(|v| v.as_array_mut()) {
                    for col in cols {
                        if let Some(col_obj) = col.as_object_mut() {
                            let name = col_obj
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_owned();

                            // Add semantic description.
                            if let Some(desc) = col_map.get(&(table_name.clone(), name.clone())) {
                                if !desc.is_empty() {
                                    col_obj.insert(
                                        "description".to_owned(),
                                        serde_json::Value::String(desc.clone()),
                                    );
                                }
                            }

                            // Add synonyms for this column.
                            if let Some(synonyms) =
                                synonym_map.get(&(table_name.clone(), name.clone()))
                            {
                                if !synonyms.is_empty() {
                                    col_obj
                                        .insert("synonyms".to_owned(), serde_json::json!(synonyms));
                                }
                            }
                        }
                    }
                }
                // Enrich table with its ai_description / user_description.
                if let Some(desc) = table_map.get(&table_name) {
                    if !desc.is_empty() {
                        // Store the merged description so the LLM prompt sees it.
                        obj.insert(
                            "ai_description".to_owned(),
                            serde_json::Value::String(desc.clone()),
                        );
                    }
                }
            }
            enriched_table
        })
        .collect();

    Ok(serde_json::Value::Array(enriched))
}

// ── Route handlers ────────────────────────────────────────────────────────────

/// POST /api/v1/nl2sql/query — generate SQL from natural language.
pub(crate) async fn query(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>> {
    self::query_async::emit_stage("request_validation", "开始校验数据源访问");
    require_nl2sql_embedding_config(&state, &claims.tenant_id).await?;

    let req_started_at = std::time::Instant::now();
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut applied_rules: Vec<AppliedRuleHit> = Vec::new();
    let route_confidence = req.route_confidence.map(|c| c.clamp(0.0, 1.0));
    let routing_method = req
        .routing_method
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let semantic_context = req.semantic_context.clone();

    let _query_span = tracing::info_span!(
        "nl2sql_query",
        tenant_id = %claims.tenant_id,
        datasource_id = %req.data_source_id,
        question_len = req.question.chars().count(),
    );

    tracing::info!(
        tenant_id = %claims.tenant_id,
        user_id = %claims.sub,
        datasource_id = %req.data_source_id,
        question_len = req.question.chars().count(),
        query_id = %query_id,
        "nl2sql /query started"
    );

    let access_check_started = std::time::Instant::now();
    let db_type = validate_data_source_access(
        &state,
        &claims.tenant_id,
        &claims.sub,
        &claims.role,
        &req.data_source_id,
    )
    .await?;
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = access_check_started.elapsed().as_millis() as u64,
        db_type = %db_type,
        "nl2sql /query access check finished"
    );
    self::query_async::emit_stage("request_validation", "数据源访问校验通过");

    // Same guard as `execute`: refuse up-front for non-SQL types rather
    // than burning an LLM call to produce SQL we could never run.
    if !matches!(
        db_type.as_str(),
        "mysql" | "tidb" | "postgres" | "clickhouse" | "presto" | "trino" | "mongodb"
    ) {
        return Err(AppError::ValidationError(format!(
            "NL2SQL is not supported for db_type: {db_type}. Pick a supported data source (mysql, tidb, postgres, clickhouse, presto, trino, mongodb)."
        )));
    }

    // Rate-limit gate. Consumes one token from the tenant's bucket; on
    // rejection returns 429 with a retry-after hint.
    // This is the user-visible operation, so we gate at the request level
    // (not per LLM call) — a single user click consumes one token even if
    // the handler internally makes multiple LLM calls.
    let rate_limit_started = std::time::Instant::now();
    if !state
        .nl2sql_rate_limiter
        .try_acquire(&state.db, &claims.tenant_id)
        .await
    {
        tracing::warn!(
            tenant_id = %claims.tenant_id,
            datasource_id = %req.data_source_id,
            elapsed_ms = rate_limit_started.elapsed().as_millis() as u64,
            total_elapsed_ms = req_started_at.elapsed().as_millis() as u64,
            "nl2sql /query rate limit rejected"
        );
        let retry_after = state
            .nl2sql_rate_limiter
            .retry_after_secs(&claims.tenant_id);
        return Err(AppError::TooManyRequests(format!(
            "NL2SQL rate limit exceeded ({}/min). Retry after {}s.",
            state.nl2sql_rate_limiter.limit(),
            retry_after
        )));
    }
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = rate_limit_started.elapsed().as_millis() as u64,
        "nl2sql /query rate limit passed"
    );

    // Generate or use provided conversation_id for multi-turn context
    let conversation_id = req
        .conversation_id
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let schema_load_started = std::time::Instant::now();
    self::query_async::emit_stage("load_schema", "正在加载 Schema");
    let config_json =
        sqlx::query_scalar::<_, serde_json::Value>("SELECT config FROM data_sources WHERE id = ?")
            .bind(&req.data_source_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("data source not found".into()))?;
    let schema_info: serde_json::Value = {
        let row = sqlx::query("SELECT schema_info FROM data_sources WHERE id = ?")
            .bind(&req.data_source_id)
            .fetch_optional(&state.db)
            .await?;
        match row {
            Some(r) => r
                .get::<Option<serde_json::Value>, _>("schema_info")
                .unwrap_or(serde_json::json!({"tables": [], "foreign_keys": []})),
            None => return Err(AppError::NotFound("data source not found".into())),
        }
    };
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = schema_load_started.elapsed().as_millis() as u64,
        "nl2sql /query schema_info loaded"
    );
    self::query_async::emit_stage("load_schema", "Schema 加载完成");

    // Extract schema tables and foreign keys from the new enriched schema structure.
    let (schema_tables, foreign_keys) = extract_schema_tables_and_fks(&schema_info);
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

    // Append manual user-defined FKs (user-defined FKs take precedence).
    let manual_fk_started = std::time::Instant::now();
    let manual_fks =
        load_manual_foreign_keys(&state.db, &claims.tenant_id, &req.data_source_id).await;
    let manual_fk_count = manual_fks.len();
    foreign_key_prompts.extend(manual_fks);
    tracing::info!(
        datasource_id = %req.data_source_id,
        manual_fk_count,
        total_fk_count = foreign_key_prompts.len(),
        elapsed_ms = manual_fk_started.elapsed().as_millis() as u64,
        "nl2sql /query foreign keys prepared"
    );
    if manual_fk_count > 0 {
        push_rule_hit(
            &mut applied_rules,
            "manual_foreign_keys_loaded",
            "Manual Foreign Keys",
            Some(format!("{manual_fk_count} user-defined FK(s) loaded")),
        );
    }

    // P1-2: Load business metrics for SQL generation prompt injection.
    let metrics_load_started = std::time::Instant::now();
    self::query_async::emit_stage("load_context", "正在加载指标与上下文");
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
        .bind(&claims.tenant_id)
        .bind(&req.data_source_id)
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
             WHERE tenant_id = ? AND datasource_id = ? AND status = 'published' AND deleted_at IS NULL",
        )
        .bind(&claims.tenant_id)
        .bind(&req.data_source_id)
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
    tracing::info!(
        datasource_id = %req.data_source_id,
        metrics_count = metrics.len(),
        elapsed_ms = metrics_load_started.elapsed().as_millis() as u64,
        "nl2sql /query metrics loaded"
    );
    let matched_metrics = matched_metric_names(&req.question, &metric_candidates);
    let metric_hard_constraint =
        resolve_metric_hard_constraint(&matched_metrics, &metric_candidates);
    if !matched_metrics.is_empty() {
        push_rule_hit(
            &mut applied_rules,
            "metric_resolved",
            "Metric Resolution",
            Some(format!("matched metrics: {}", matched_metrics.join(", "))),
        );
    }
    let synonym_hits = detect_synonym_hits(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
        &req.question,
    )
    .await;
    if !synonym_hits.is_empty() {
        let preview = synonym_hits
            .iter()
            .take(3)
            .map(|(term, table, col)| format!("{term}->{table}.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        push_rule_hit(
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

    // Load conversation history before the LLM call so we can pass it in.
    let history_load_started = std::time::Instant::now();
    let history =
        load_conversation_history(&state.db, &claims.tenant_id, &conversation_id, 8).await;
    tracing::info!(
        datasource_id = %req.data_source_id,
        conversation_id = %conversation_id,
        history_turns = history.messages.len(),
        elapsed_ms = history_load_started.elapsed().as_millis() as u64,
        "nl2sql /query conversation history loaded"
    );
    self::query_async::emit_stage("load_context", "上下文加载完成");

    // Load AI + user descriptions for tables and columns.
    // User description takes precedence over AI description.
    let semantics_load_started = std::time::Instant::now();
    let mut schema_tables =
        enrich_schema_with_semantics(&state.db, &req.data_source_id, schema_tables.clone())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    datasource_id = %req.data_source_id,
                    error = %e,
                    "failed to load semantics, falling back to raw schema"
                );
                schema_tables
            });
    let datasource_domains = match crate::nl2sql::routing::resolve_business_domains(
        &state.db,
        Some(&req.data_source_id),
    )
    .await
    {
        Ok(domains) => domains,
        Err(e) => {
            tracing::warn!(
                error = %e,
                datasource_id = %req.data_source_id,
                "failed to resolve business domains for /query"
            );
            Vec::new()
        }
    };
    let matched_business_domain = business_domain_context_for_question(
        &datasource_domains,
        &req.data_source_id,
        &req.question,
    );
    let business_domain_context = matched_business_domain
        .as_ref()
        .map(BusinessDomainQuestionContext::system_prompt);
    let semantic_question = matched_business_domain.as_ref().map_or_else(
        || req.question.clone(),
        |context| context.semantic_question(&req.question),
    );
    // Compile and persist the semantic request before any provider is asked to
    // write SQL. A failed/timeout generation therefore still leaves an
    // auditable intent and the next repair attempt can reuse the same IR.
    let mut question_intent = semantic_audit::compile_question_intent(
        &claims.tenant_id,
        &req.data_source_id,
        &semantic_question,
        &matched_metrics,
    );
    // Resolve metric aliases against tenant-owned, versioned contracts before
    // SQL generation.  A missing/ambiguous contract remains explicit in the
    // IR and is handled by the semantic verifier; it is never silently
    // replaced with a prompt-only definition.
    let mut loaded_metric_contracts = Vec::new();
    match crate::semantic_kernel_store::load_metric_contracts(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
        &question_intent
            .metrics
            .iter()
            .map(|metric| metric.id.clone())
            .collect::<Vec<_>>(),
    )
    .await
    {
        Ok(stored) => {
            loaded_metric_contracts = stored
                .iter()
                .map(|item| item.contract.clone())
                .collect::<Vec<_>>();
            semantic_audit::bind_metric_contracts(&mut question_intent, &loaded_metric_contracts);
            if question_intent
                .metrics
                .iter()
                .any(|metric| metric.version.is_some())
            {
                push_rule_hit(
                    &mut applied_rules,
                    "metric_contract_bound",
                    "Metric Contract",
                    Some(format!(
                        "bound {} versioned metric(s)",
                        question_intent
                            .metrics
                            .iter()
                            .filter(|metric| metric.version.is_some())
                            .count()
                    )),
                );
            }
        }
        Err(error) => {
            tracing::warn!(tenant_id = %claims.tenant_id, error = %error, "metric contract lookup failed; retaining unresolved metric semantics");
            question_intent
                .unresolved
                .push(nl2sql_core::semantic_ir::SemanticAmbiguity {
                    field: "metric_contract_store".into(),
                    candidates: Vec::new(),
                    impact: "metric contract lookup failed; semantic release is blocked".into(),
                });
        }
    }
    let loaded_join_contracts = match crate::semantic_kernel_store::load_join_contracts(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
    )
    .await
    {
        Ok(stored) => stored
            .into_iter()
            .map(|item| item.contract)
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(tenant_id = %claims.tenant_id, error = %error, "join contract lookup failed; join verification will remain conservative");
            Vec::new()
        }
    };
    if let Some(domain_match) = matched_business_domain.as_ref() {
        push_rule_hit(
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
    let strict_allow_tables = if datasource_domains.is_empty() {
        None
    } else {
        {
            let strict_match = strict_domain_tables_for_question(
                &datasource_domains,
                &req.data_source_id,
                &req.question,
            );
            if strict_match.allowed_tables.is_empty() {
                None
            } else {
                let before_count = schema_tables.as_array().map(|a| a.len()).unwrap_or(0);
                schema_tables =
                    filter_schema_tables_by_allowlist(&schema_tables, &strict_match.allowed_tables);
                let after_count = schema_tables.as_array().map(|a| a.len()).unwrap_or(0);
                let matched = if strict_match.matched_domains.is_empty() {
                    "n/a".to_string()
                } else {
                    strict_match.matched_domains.join(",")
                };
                push_rule_hit(
                    &mut applied_rules,
                    "strict_domain_filter",
                    "Strict Business Domain Filter",
                    Some(format!(
                        "domains={matched}; restricted schema tables: {} -> {}",
                        before_count, after_count
                    )),
                );
                Some(strict_match.allowed_tables)
            }
        }
    };
    tracing::info!(
        datasource_id = %req.data_source_id,
        table_count = schema_tables.as_array().map(|a| a.len()).unwrap_or(0),
        elapsed_ms = semantics_load_started.elapsed().as_millis() as u64,
        "nl2sql /query schema semantics prepared"
    );

    // ── Query Understanding (optional, controlled by env) ────────────────────────
    let qu_started = std::time::Instant::now();
    self::query_async::emit_stage("query_understanding", "正在做意图分析");
    let mut qu_result: Option<crate::nl2sql::query_understanding::QueryUnderstandingResult> =
        if should_enable_qu() {
            let chat_cfg = match crate::nl2sql::resolve_chat_config(
                state.config_registry(),
                &claims.tenant_id,
                &claims.sub,
                &state.default_model,
                Some("nl2sql"),
            )
            .await
            {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "QU: failed to resolve chat config, skipping QU");
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
                        &req.data_source_id,
                        &claims.tenant_id,
                        &schema_for_qu,
                        &history.messages,
                    )
                    .await
                {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "QU: understand() failed, skipping");
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
        let removed = remove_business_domain_derived_filters(qu, domain_match);
        if removed > 0 {
            push_rule_hit(
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
        semantic_audit::apply_query_understanding(&mut question_intent, qu);
    }
    if let Some(qu) = qu_result.as_ref() {
        push_rule_hit(
            &mut applied_rules,
            "query_understanding",
            "Query Understanding",
            Some(format!(
                "intent={}, confidence={:.2}",
                qu.intent, qu.confidence
            )),
        );
        if let Some(time) = qu.entities.time.as_ref() {
            push_rule_hit(
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
        push_rule_hit(
            &mut applied_rules,
            "query_understanding_enabled",
            "Query Understanding",
            Some("enabled".to_string()),
        );
    }
    tracing::info!(
        datasource_id = %req.data_source_id,
        enabled = should_enable_qu(),
        qu_hit = qu_result.is_some(),
        elapsed_ms = qu_started.elapsed().as_millis() as u64,
        "nl2sql /query query-understanding finished"
    );
    self::query_async::emit_stage("query_understanding", "意图分析完成");

    let references_active = req
        .reference_bindings
        .as_ref()
        .map(ReferenceBindingRequest::is_active)
        .unwrap_or(false);
    self::query_async::emit_stage("load_context", "正在检索绑定参考");
    let mut reference_snippets = resolve_query_references(
        &state,
        &claims.tenant_id,
        &req.data_source_id,
        &req.question,
        req.reference_bindings.as_ref(),
        sql_knowledge_prompt_max_snippets().min(12),
    )
    .await?;
    let deterministic_reference_count = reference_snippets.len();
    self::query_async::emit_stage("load_context", "正在进行知识库工具检索");
    // The SQL generation request already owns the model-selected knowledge
    // tool loop. Running a second model prefetch here duplicates the same
    // exploration and can add several provider round trips per attribution
    // step. Keep prefetch only as compatibility when generation tools are
    // explicitly disabled; deterministic reference retrieval above remains
    // active in both modes.
    if !should_enable_sql_generation_tool_loop() {
        match crate::nl2sql::resolve_chat_config_candidates(
            state.config_registry(),
            &claims.tenant_id,
            &claims.sub,
            &state.default_model,
            Some("nl2sql"),
        )
        .await
        {
            Ok(chat_candidates) => {
                if let Some(chat_cfg) = chat_candidates.first() {
                    let tool_prefetch_snippets = run_sql_knowledge_tool_prefetch(
                        &state,
                        &claims,
                        &req.data_source_id,
                        &req.question,
                        &schema_tables,
                        chat_cfg,
                    )
                    .await;
                    if !tool_prefetch_snippets.is_empty() {
                        push_rule_hit(
                            &mut applied_rules,
                            "sql_knowledge_tool_loop",
                            "SQL Knowledge Tool Loop",
                            Some(format!(
                                "{} snippet(s) retrieved by model-selected knowledge/schema tools",
                                tool_prefetch_snippets.len()
                            )),
                        );
                        reference_snippets = merge_reference_snippets(
                            &reference_snippets,
                            tool_prefetch_snippets,
                            sql_knowledge_prompt_max_snippets(),
                        );
                    }
                }
            }
            Err(e) => tracing::warn!(
                error = %e,
                datasource_id = %req.data_source_id,
                "nl2sql SQL knowledge tool prefetch skipped because chat config resolution failed"
            ),
        }
    }
    if reference_snippets.is_empty()
        && schema_tables
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
    {
        match self::reference::resolve_recent_sql_examples_for_datasource(
            &state,
            &claims.tenant_id,
            &req.data_source_id,
            sql_knowledge_prompt_max_snippets().min(8),
        )
        .await
        {
            Ok(fallback_refs) if !fallback_refs.is_empty() => {
                push_rule_hit(
                    &mut applied_rules,
                    "sql_knowledge_empty_schema_fallback",
                    "SQL Knowledge Empty Schema Fallback",
                    Some(format!(
                        "{} recent SQL example(s) loaded because live schema is empty and direct retrieval was empty",
                        fallback_refs.len()
                    )),
                );
                reference_snippets = merge_reference_snippets(
                    &reference_snippets,
                    fallback_refs,
                    sql_knowledge_prompt_max_snippets(),
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                datasource_id = %req.data_source_id,
                "nl2sql empty-schema SQL knowledge fallback failed"
            ),
        }
    }
    let auto_opened_sql_files = auto_open_relevant_sql_knowledge_files(
        &state,
        &claims,
        &req.data_source_id,
        &req.question,
        &reference_snippets,
        None,
    )
    .await;
    if !auto_opened_sql_files.is_empty() {
        push_rule_hit(
            &mut applied_rules,
            "sql_knowledge_auto_open",
            "SQL Knowledge Auto Open",
            Some(format!(
                "{} full SQL file context snippet(s) opened deterministically",
                auto_opened_sql_files.len()
            )),
        );
        reference_snippets = merge_reference_snippets(
            &auto_opened_sql_files,
            reference_snippets,
            sql_knowledge_prompt_max_snippets(),
        );
    }
    reference_snippets =
        merge_reference_snippets(&[], reference_snippets, sql_knowledge_prompt_max_snippets());
    let hydrated = discover_knowledge_schema_tables(
        &state,
        &claims,
        &req.data_source_id,
        &db_type,
        &config_json,
        &schema_tables,
        &reference_snippets,
        None,
    )
    .await;
    if hydrated.as_array().is_some_and(|tables| !tables.is_empty()) {
        self::query_async::emit_stage("load_schema", "已按 SQL 知识库命中的表按需确认 Schema");
        schema_tables = merge_schema_tables(&schema_tables, &hydrated);
        if let Some(allowed_tables) = strict_allow_tables.as_ref() {
            schema_tables = filter_schema_tables_by_allowlist(&schema_tables, allowed_tables);
        }
    }
    let explicit_dimension_columns = synonym_hits
        .iter()
        .map(|(_, _, column)| column.clone())
        .collect::<Vec<_>>();
    semantic_audit::bind_schema_dimensions(
        &mut question_intent,
        &schema_tables,
        &explicit_dimension_columns,
    );
    if let Err(error) = crate::semantic_kernel_store::persist_nl2sql_intent_ir(
        &state.db,
        &claims.tenant_id,
        &conversation_id,
        &query_id,
        &query_id,
        &serde_json::to_value(&question_intent).unwrap_or_else(|_| serde_json::json!({})),
    )
    .await
    {
        return Err(AppError::Internal(format!(
            "failed to persist bound analytic intent before SQL generation: {error}"
        )));
    }
    push_rule_hit(
        &mut applied_rules,
        "semantic_ir_compiled_before_sql",
        "Analytics Semantic Compiler",
        Some(format!(
            "metrics={}, dimensions={}, unresolved={}",
            question_intent.metrics.len(),
            question_intent.dimensions.len(),
            question_intent.unresolved.len()
        )),
    );
    let mut used_references: Vec<ReferenceUsageDto> = reference_snippets
        .iter()
        .map(ReferencePromptSnippet::to_usage_dto)
        .collect();
    let sql_knowledge_available = match self::reference::has_indexed_sql_knowledge_for_datasource(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
    )
    .await
    {
        Ok(available) => available,
        Err(e) => {
            tracing::warn!(
                error = %e,
                datasource_id = %req.data_source_id,
                "nl2sql failed to check SQL knowledge availability; keeping conservative no-cache behavior"
            );
            true
        }
    };
    if references_active {
        push_rule_hit(
            &mut applied_rules,
            "reference_context_bound",
            "Reference Context",
            Some(format!(
                "{} reference snippet(s) retrieved from selected pack/file bindings",
                used_references.len()
            )),
        );
    } else if !used_references.is_empty() {
        push_rule_hit(
            &mut applied_rules,
            "sql_knowledge_auto_retrieved",
            "SQL Knowledge Auto Retrieval",
            Some(format!(
                "{} SQL knowledge snippet(s) retrieved automatically ({} deterministic, {} tool-loop/schema)",
                used_references.len(),
                deterministic_reference_count,
                used_references.len().saturating_sub(deterministic_reference_count)
            )),
        );
    }
    self::query_async::emit_stage("load_context", "绑定参考检索完成");

    // Load pre-computed JOIN paths for multi-table queries.
    let join_paths_load_started = std::time::Instant::now();
    let join_paths = load_join_paths_for_datasource(&state.db, &req.data_source_id).await;
    tracing::info!(
        datasource_id = %req.data_source_id,
        join_path_count = join_paths.len(),
        elapsed_ms = join_paths_load_started.elapsed().as_millis() as u64,
        "nl2sql /query join paths loaded"
    );
    if !join_paths.is_empty() {
        push_rule_hit(
            &mut applied_rules,
            "join_paths_loaded",
            "Join Path Modeling",
            Some(format!("{} join path(s) available", join_paths.len())),
        );
    }

    // ── Result Cache lookup ───────────────────────────────────────────────────
    let cache_lineage = crate::nl2sql::result_cache::CacheLineage {
        intent_hash: crate::semantic_kernel_store::sha256_json(
            &serde_json::to_value(&question_intent).unwrap_or_default(),
        ),
        schema_hash: crate::semantic_kernel_store::sha256_json(&schema_tables),
        metric_contracts_hash: crate::semantic_kernel_store::sha256_json(
            &serde_json::to_value(&loaded_metric_contracts).unwrap_or_default(),
        ),
        join_contracts_hash: crate::semantic_kernel_store::sha256_json(
            &serde_json::to_value(&loaded_join_contracts).unwrap_or_default(),
        ),
        policy_hash: self::queries::query_policy_lineage_hash(
            &state.db,
            &claims.tenant_id,
            &req.data_source_id,
            &claims.sub,
            &claims.email,
        )
        .await?,
        compiler_version: "analytics-semantic-compiler-v2".into(),
    };
    let cache_hash = crate::nl2sql::result_cache::question_hash(
        &claims.tenant_id,
        &req.data_source_id,
        &req.question,
    );
    let cache_lookup_started = std::time::Instant::now();
    self::query_async::emit_stage("cache_lookup", "正在检查缓存命中");
    if used_references.is_empty() && !sql_knowledge_available && matched_business_domain.is_none() {
        if let Some(hit) = crate::nl2sql::result_cache::lookup(
            &state.db,
            &claims.tenant_id,
            &req.data_source_id,
            &cache_hash,
            &cache_lineage,
        )
        .await
        {
            let mut cached_sql = hit.generated_sql;
            let cache_policy = enforce_query_policy(
                &state.db,
                &claims.tenant_id,
                &req.data_source_id,
                &claims.sub,
                &claims.email,
                &extract_tables_from_sql(&cached_sql),
                &extract_columns_from_sql(&cached_sql),
            )
            .await?;
            let mut cache_release_error = cache_policy
                .is_denied()
                .then(|| query_policy_denial_message(&cache_policy));
            if cache_release_error.is_none() {
                if let Some(row_filter) = cache_policy.row_filter_expr.as_deref() {
                    cached_sql = inject_query_policy_row_filter(&cached_sql, row_filter).map_err(
                        |error| {
                            AppError::ValidationError(format!(
                                "cached SQL policy filter could not be applied safely: {error}"
                            ))
                        },
                    )?;
                }
                match semantic_audit::compile_canonical_intent_with_contracts_and_joins(
                    &question_intent,
                    &cached_sql,
                    &loaded_metric_contracts,
                    &loaded_join_contracts,
                ) {
                    Some(audit) => {
                        let verification = semantic_audit::verification_json(&audit);
                        let decision = serde_json::to_string(&audit.verification.release_decision)
                            .unwrap_or_else(|_| "\"Reject\"".into())
                            .trim_matches('"')
                            .to_string();
                        if let Err(error) =
                            semantic_audit::require_execution_validation_decision(&decision)
                        {
                            crate::semantic_kernel_store::persist_nl2sql_repair_verification(
                                &state.db,
                                &claims.tenant_id,
                                &query_id,
                                &cached_sql,
                                &verification,
                                &decision,
                                f64::from(audit.verification.confidence_basis.calibrated_score),
                            )
                            .await
                            .map_err(|persist_error| {
                                AppError::Internal(format!(
                                    "failed to persist rejected cache verification: {persist_error}"
                                ))
                            })?;
                            cache_release_error = Some(error);
                        } else {
                            crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
                                &state.db,
                                &claims.tenant_id,
                                &req.data_source_id,
                                &conversation_id,
                                &query_id,
                                &semantic_audit::intent_json(&audit),
                                &verification,
                                &decision,
                                f64::from(audit.verification.confidence_basis.calibrated_score),
                            )
                            .await
                            .map_err(|persist_error| {
                                AppError::Internal(format!(
                                    "failed to persist cache semantic release: {persist_error}"
                                ))
                            })?;
                        }
                    }
                    None => {
                        cache_release_error =
                            Some("cached SQL could not be parsed by the semantic verifier".into());
                    }
                }
            }
            if let Some(error) = cache_release_error {
                tracing::warn!(
                    tenant_id = %claims.tenant_id,
                    datasource_id = %req.data_source_id,
                    query_id = %query_id,
                    error,
                    "NL2SQL cache candidate failed current semantic release; invalidating cache"
                );
                crate::nl2sql::result_cache::invalidate_datasource(
                    &state.db,
                    &claims.tenant_id,
                    &req.data_source_id,
                )
                .await;
            } else {
                push_rule_hit(
                    &mut applied_rules,
                    "result_cache_hit",
                    "Result Cache",
                    Some("cache lineage and semantic release re-verified".to_string()),
                );
                self::query_async::emit_stage("done", "命中缓存，已重新验证 SQL");
                sqlx::query(
                    "INSERT INTO nl2sql_queries \
                     (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, ?)",
                )
                .bind(&query_id)
                .bind(&claims.tenant_id)
                .bind(&claims.sub)
                .bind(&req.data_source_id)
                .bind(&conversation_id)
                .bind(&req.question)
                .bind(&cached_sql)
                .bind(route_confidence)
                .bind(routing_method.as_deref())
                .bind(semantic_context.clone())
                .bind(applied_rules_json_value(&applied_rules))
                .execute(&state.db)
                .await?;
                tracing::info!(
                    datasource_id = %req.data_source_id,
                    cache_lookup_ms = cache_lookup_started.elapsed().as_millis() as u64,
                    total_elapsed_ms = req_started_at.elapsed().as_millis() as u64,
                    "nl2sql /query finished from verified cache"
                );
                return Ok(Json(QueryResponse {
                    sql: Some(cached_sql),
                    explanation: None,
                    error: None,
                    clarification_question: None,
                    confirmed_requirements: None,
                    missing_requirements: None,
                    query_id,
                    conversation_id: Some(conversation_id),
                    summary_version: None,
                    query_understanding: qu_result.clone(),
                    intent: qu_result.as_ref().map(|q| q.intent.to_string()),
                    cache_hit: true,
                    applied_rules,
                    used_references: Vec::new(),
                }));
            }
        }
    } else {
        let (rule_id, reason) = if !used_references.is_empty() {
            (
                "result_cache_bypassed_for_references",
                "cache bypassed because SQL knowledge references can change SQL semantics",
            )
        } else {
            (
                "result_cache_bypassed_for_sql_knowledge",
                "cache bypassed because this datasource has SQL knowledge files and retrieval/generation must inspect the latest workspace",
            )
        };
        push_rule_hit(
            &mut applied_rules,
            rule_id,
            "Result Cache",
            Some(reason.to_string()),
        );
    }
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = cache_lookup_started.elapsed().as_millis() as u64,
        "nl2sql /query cache miss"
    );
    self::query_async::emit_stage("clarification_gate", "正在检查澄清必要性");

    // Stage-2: Requirement completeness gate.
    // Before generating SQL, enforce a minimum set of business constraints
    // so boss-facing answers do not silently guess missing semantics.
    let knowledge_metric_candidates =
        knowledge_metric_candidates_from_references(&req.question, &reference_snippets);
    let mut requirement_metrics = metrics.clone();
    if !knowledge_metric_candidates.is_empty() {
        let matched_knowledge_metric_names: Vec<String> = knowledge_metric_candidates
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect();
        push_rule_hit(
            &mut applied_rules,
            "sql_knowledge_metric_resolved",
            "SQL Knowledge Metric Resolution",
            Some(format!(
                "resolved from SQL knowledge: {}",
                matched_knowledge_metric_names.join(", ")
            )),
        );
        requirement_metrics.extend(knowledge_metric_candidates);
    }
    let requirement_metric_hints: Vec<String> = matched_metrics
        .iter()
        .cloned()
        .chain(
            requirement_metrics
                .iter()
                .map(|(name, _, _)| name.clone())
                .filter(|name| !metrics.iter().any(|(m, _, _)| m == name)),
        )
        .collect();
    let requirement_question =
        augment_question_for_metric_hint(&semantic_question, &requirement_metric_hints);
    let requirement_question = augment_follow_up_requirement_context(
        &requirement_question,
        history
            .messages
            .first()
            .map(|(question, sql)| (question.as_str(), sql.as_str())),
    );
    let req_check = parse_requirements_from_question(
        &requirement_question,
        qu_result.as_ref(),
        &schema_tables,
        &requirement_metrics,
    );
    let strong_sql_knowledge_context =
        has_strong_sql_knowledge_context(&schema_tables, &reference_snippets);
    let explicit_business_domain_match = matched_business_domain.is_some();
    if !req_check.missing.is_empty()
        && !strong_sql_knowledge_context
        && !explicit_business_domain_match
    {
        push_rule_hit(
            &mut applied_rules,
            "clarification_required",
            "Requirement Clarification Gate",
            Some(format!(
                "{} unresolved requirement(s)",
                req_check.missing.len()
            )),
        );
        self::query_async::emit_stage("clarification_gate", "需要澄清，等待用户补充");
        let clarification_question = build_requirement_clarification_question(&req_check.missing);
        let clarify_ctx = crate::nl2sql::ClarificationContext {
            original_question: req.question.clone(),
            clarification_question: clarification_question.clone(),
            options: vec![crate::nl2sql::ClarificationOption {
                option_index: 0,
                data_source_id: req.data_source_id.clone(),
                table_name: "当前数据源".to_string(),
                column_name: "补充需求".to_string(),
                reason: "需求关键信息不足，需要先补齐业务约束".to_string(),
                sim_score: 1.0,
                business_meaning: "请直接输入缺失条件（时间、指标、维度、筛选等）".to_string(),
            }],
            confirmed_requirements: req_check.confirmed.clone(),
            missing_requirements: req_check.missing.clone(),
            missing_requirement_reasons: req_check.missing_reasons.clone(),
            clarification_history: Vec::new(),
            turn: 0,
            conversation_id: conversation_id.clone(),
        };
        let ts = now_ms();
        let clarify_record = super::chat::SessionMessageRecord {
            role: "clarification".to_string(),
            content: serde_json::json!(clarify_ctx),
            timestamp_ms: ts,
        };
        if let Err(e) = super::chat::append_message(
            &state.data_dir,
            &claims.tenant_id,
            &claims.sub,
            &conversation_id,
            &clarify_record,
        ) {
            tracing::warn!(
                error = %e,
                tenant_id = %claims.tenant_id,
                user_id = %claims.sub,
                conversation_id = %conversation_id,
                "failed to persist query-stage clarification context to session"
            );
        }
        // Persist the initial clarification prompt into DB timeline so it can be
        // rendered when loading historical conversations from `/conversations/:id`.
        if let (Ok(confirmed), Ok(missing)) = (
            serde_json::to_value(&req_check.confirmed),
            serde_json::to_value(&req_check.missing),
        ) {
            if let Err(e) = sqlx::query(
                "INSERT INTO nl2sql_clarification_messages \
                 (id, tenant_id, user_id, conversation_id, session_id, turn, original_question, clarification_question, user_input, confirmed_requirements, missing_requirements) \
                 VALUES (?, ?, ?, ?, NULL, ?, ?, ?, '', ?, ?)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(&conversation_id)
            .bind(0u32)
            .bind(&req.question)
            .bind(&clarification_question)
            .bind(confirmed)
            .bind(missing)
            .execute(&state.db)
            .await
            {
                tracing::warn!(
                    error = %e,
                    tenant_id = %claims.tenant_id,
                    user_id = %claims.sub,
                    conversation_id = %conversation_id,
                    "failed to persist initial clarification message to db"
                );
            }
        }
        sqlx::query(
            "INSERT INTO nl2sql_queries \
             (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, 0, 0, ?, ?, ?, ?)",
        )
        .bind(&query_id)
        .bind(&claims.tenant_id)
        .bind(&claims.sub)
        .bind(&req.data_source_id)
        .bind(&conversation_id)
        .bind(&req.question)
        .bind(route_confidence)
        .bind(routing_method.as_deref())
        .bind(semantic_context.clone())
        .bind(applied_rules_json_value(&applied_rules))
        .execute(&state.db)
        .await?;
        persist_reference_usages_for_query(
            &state,
            &claims,
            &query_id,
            &req.data_source_id,
            &req.question,
            &reference_snippets,
        )
        .await;
        upsert_nl2sql_conversation(
            &state.db,
            &claims.tenant_id,
            &claims.sub,
            &conversation_id,
            &req.question,
        )
        .await;
        return Ok(Json(QueryResponse {
            sql: None,
            explanation: None,
            error: None,
            clarification_question: Some(clarification_question),
            confirmed_requirements: Some(req_check.confirmed),
            missing_requirements: Some(req_check.missing),
            query_id,
            conversation_id: Some(conversation_id),
            summary_version: None,
            query_understanding: qu_result.clone(),
            intent: qu_result.as_ref().map(|q| q.intent.to_string()),
            cache_hit: false,
            applied_rules,
            used_references: used_references.clone(),
        }));
    } else if !req_check.missing.is_empty() && strong_sql_knowledge_context {
        push_rule_hit(
            &mut applied_rules,
            "clarification_bypassed_by_sql_knowledge",
            "Requirement Clarification Gate",
            Some(format!(
                "bypassed {} missing requirement(s) because high-relevance SQL Knowledge can supply the working context",
                req_check.missing.len()
            )),
        );
    } else if !req_check.missing.is_empty() && explicit_business_domain_match {
        push_rule_hit(
            &mut applied_rules,
            "clarification_bypassed_by_business_domain",
            "Requirement Clarification Gate",
            Some(format!(
                "bypassed {} missing requirement(s) because the question explicitly matched configured business-domain routing metadata",
                req_check.missing.len()
            )),
        );
    }

    // P1-4: Three-layer schema compression
    // Layer 1 (overview): always injected as table list before full schema
    // For large schemas (>LARGE_SCHEMA_THRESHOLD tables), Layer 1 is prominent;
    const LARGE_SCHEMA_THRESHOLD: usize = 20;
    let table_count = schema_tables.as_array().map(|a| a.len()).unwrap_or(0);
    let large_schema_mode = table_count > LARGE_SCHEMA_THRESHOLD;

    let planning_start = std::time::Instant::now();
    self::query_async::emit_stage("generate_sql", "正在生成 SQL");
    let semantic_intent_json = serde_json::to_string(&question_intent).map_err(|error| {
        AppError::Internal(format!("failed to serialize analytic intent: {error}"))
    })?;
    let sql_result = generate_sql(
        &state,
        &claims,
        Some(&req.data_source_id),
        &semantic_question,
        &schema_tables,
        &foreign_key_prompts,
        &join_paths,
        history,
        None,
        qu_result.as_ref(),
        &db_type,
        large_schema_mode,
        &metrics
            .iter()
            .map(|(n, e, f)| (n.clone(), e.clone(), f.as_deref()))
            .collect::<Vec<_>>(),
        &matched_metrics,
        &reference_snippets,
        business_domain_context.as_deref(),
        None,
        true,
        &semantic_intent_json,
    )
    .await;

    let planning_ms = planning_start.elapsed().as_millis() as i64;
    tracing::info!(
        datasource_id = %req.data_source_id,
        large_schema_mode,
        planning_ms,
        "nl2sql /query sql generation finished"
    );
    self::query_async::emit_stage("generate_sql", "SQL 生成完成");

    let mut sql = match sql_result {
        Ok(r) => {
            if !r.tool_reference_snippets.is_empty() {
                push_rule_hit(
                    &mut applied_rules,
                    "sql_generation_tool_loop",
                    "SQL Generation Tool Loop",
                    Some(format!(
                        "{} extra snippet(s) read while generating SQL",
                        r.tool_reference_snippets.len()
                    )),
                );
                reference_snippets = merge_reference_snippets(
                    &reference_snippets,
                    r.tool_reference_snippets.clone(),
                    sql_knowledge_prompt_max_snippets(),
                );
                used_references = reference_snippets
                    .iter()
                    .map(ReferencePromptSnippet::to_usage_dto)
                    .collect();
            }
            if let (Some(usage), Some(model)) = (r.usage.as_ref(), r.model.as_deref()) {
                record_nl2sql_token_usage(
                    &state,
                    &claims,
                    &conversation_id,
                    Some(&query_id),
                    usage,
                    model,
                    r.api_key_id.clone(),
                    r.provider.clone(),
                )
                .await;
            }
            // LLM requested clarification instead of generating SQL
            if let Some(cq) = r.clarification_question {
                push_rule_hit(
                    &mut applied_rules,
                    "model_clarification_requested",
                    "Model Clarification",
                    Some("model asked for extra constraints".to_string()),
                );
                self::query_async::emit_stage("clarification_gate", "模型请求澄清，等待用户补充");
                sqlx::query(
                    "INSERT INTO nl2sql_queries \
                     (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                     VALUES (?, ?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?)",
                )
                .bind(&query_id)
                .bind(&claims.tenant_id)
                .bind(&claims.sub)
                .bind(&req.data_source_id)
                .bind(&conversation_id)
                .bind(&req.question)
                .bind(planning_ms)
                .bind(route_confidence)
                .bind(routing_method.as_deref())
                .bind(semantic_context.clone())
                .bind(applied_rules_json_value(&applied_rules))
                .execute(&state.db)
                .await?;
                persist_reference_usages_for_query(
                    &state,
                    &claims,
                    &query_id,
                    &req.data_source_id,
                    &req.question,
                    &reference_snippets,
                )
                .await;
                upsert_nl2sql_conversation(
                    &state.db,
                    &claims.tenant_id,
                    &claims.sub,
                    &conversation_id,
                    &req.question,
                )
                .await;
                return Ok(Json(QueryResponse {
                    sql: None,
                    explanation: None,
                    error: None,
                    clarification_question: Some(cq),
                    confirmed_requirements: None,
                    missing_requirements: None,
                    query_id,
                    conversation_id: Some(conversation_id),
                    summary_version: None,
                    query_understanding: qu_result.clone(),
                    intent: qu_result.as_ref().map(|q| q.intent.to_string()),
                    cache_hit: false,
                    applied_rules,
                    used_references: used_references.clone(),
                }));
            }
            r.sql
        }
        Err(e) => {
            sqlx::query(
                "INSERT INTO nl2sql_queries \
                 (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, error_message, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&query_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(&req.data_source_id)
            .bind(&conversation_id)
            .bind(&req.question)
            .bind(e.to_string())
            .bind(planning_ms)
            .bind(route_confidence)
            .bind(routing_method.as_deref())
            .bind(semantic_context.clone())
            .bind(applied_rules_json_value(&applied_rules))
            .execute(&state.db)
            .await?;
            persist_reference_usages_for_query(
                &state,
                &claims,
                &query_id,
                &req.data_source_id,
                &req.question,
                &reference_snippets,
            )
            .await;
            upsert_nl2sql_conversation(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                &conversation_id,
                &req.question,
            )
            .await;
            return Ok(Json(QueryResponse {
                sql: None,
                explanation: None,
                error: Some(e.to_string()),
                clarification_question: None,
                confirmed_requirements: None,
                missing_requirements: None,
                query_id,
                conversation_id: Some(conversation_id.clone()),
                summary_version: fetch_summary_version_i32(&state.db, &conversation_id).await,
                query_understanding: qu_result.clone(),
                intent: qu_result.as_ref().map(|q| q.intent.to_string()),
                cache_hit: false,
                applied_rules,
                used_references: used_references.clone(),
            }));
        }
    };

    if let Some(constraint) = metric_hard_constraint.as_ref() {
        if let Some(rewritten) = enforce_metric_hard_constraint_sql(&sql, constraint) {
            sql = rewritten;
            push_rule_hit(
                &mut applied_rules,
                "metric_hard_enforced",
                "Metric Hard Constraint",
                Some(format!("enforced metric: {}", constraint.metric_name)),
            );
        } else {
            tracing::warn!(
                metric_name = %constraint.metric_name,
                "metric hard-constraint rewrite skipped due to parse/shape mismatch"
            );
        }
    }

    sql = normalize_sql_time_filters_with_qu(&sql, qu_result.as_ref());
    let (normalized_sql, time_conversion_rewrites) =
        normalize_generated_time_conversions(&sql, &schema_tables, &db_type);
    if !time_conversion_rewrites.is_empty() {
        let detail = time_conversion_rewrites
            .iter()
            .map(|r| format!("{}:{}->{}", r.column, r.type_name, r.strategy))
            .collect::<Vec<_>>()
            .join("; ");
        tracing::info!(
            datasource_id = %req.data_source_id,
            rewrites = %detail,
            "nl2sql generated SQL time conversions normalized"
        );
        push_rule_hit(
            &mut applied_rules,
            "time_conversion_normalized",
            "Time Conversion Normalized",
            Some(detail),
        );
        sql = normalized_sql;
    }

    self::query_async::emit_stage("semantic_review", "正在复核 SQL 语义和口径");
    let semantic_review = tokio::time::timeout(
        std::time::Duration::from_secs(sql_semantic_review_timeout_secs()),
        review_generated_sql_semantics(
            &state,
            &claims,
            &semantic_question,
            &sql,
            &schema_tables,
            &db_type,
            &reference_snippets,
        ),
    )
    .await;
    let semantic_review = match semantic_review {
        Ok(review) => review,
        Err(_) => {
            tracing::warn!(
                datasource_id = %req.data_source_id,
                timeout_secs = sql_semantic_review_timeout_secs(),
                "nl2sql semantic SQL review timed out; returning the generated SQL after deterministic safety checks"
            );
            push_rule_hit(
                &mut applied_rules,
                "sql_semantic_review_timeout",
                "SQL Semantic Review",
                Some(format!(
                    "review exceeded {}s; continued with generated SQL",
                    sql_semantic_review_timeout_secs()
                )),
            );
            None
        }
    };
    if let Some(review) = semantic_review {
        let detail = if review.issues.is_empty() {
            format!("verdict={}", review.verdict)
        } else {
            format!(
                "verdict={}; issues={}",
                review.verdict,
                review
                    .issues
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            )
        };
        if let Some(reviewed_sql) = review.reviewed_sql {
            if reviewed_sql != sql {
                sql = reviewed_sql;
                if let Some(constraint) = metric_hard_constraint.as_ref() {
                    if let Some(rewritten) = enforce_metric_hard_constraint_sql(&sql, constraint) {
                        sql = rewritten;
                    }
                }
                sql = normalize_sql_time_filters_with_qu(&sql, qu_result.as_ref());
                let (review_normalized_sql, _) =
                    normalize_generated_time_conversions(&sql, &schema_tables, &db_type);
                sql = review_normalized_sql;
                push_rule_hit(
                    &mut applied_rules,
                    "sql_semantic_review_rewritten",
                    "SQL Semantic Review",
                    Some(detail),
                );
            }
        } else {
            push_rule_hit(
                &mut applied_rules,
                "sql_semantic_review_passed",
                "SQL Semantic Review",
                Some(detail),
            );
        }
    } else if should_enable_sql_semantic_review()
        && !applied_rules
            .iter()
            .any(|hit| hit.rule_key == "sql_semantic_review_timeout")
    {
        push_rule_hit(
            &mut applied_rules,
            "sql_semantic_review_skipped",
            "SQL Semantic Review",
            Some("review unavailable; continuing with generated SQL".to_string()),
        );
    }
    self::query_async::emit_stage("semantic_review", "SQL 语义复核完成");

    if let Some(domain_context) = matched_business_domain.as_ref() {
        let suspicious = business_domain_derived_sql_literals(
            &sql,
            &semantic_question,
            &domain_context.matched_domains,
        );
        if !suspicious.is_empty() {
            let guard_error = format!(
                "Business-domain labels are routing metadata, but the generated SQL used domain-derived literal predicates: {}. Regenerate the SQL without those predicates unless they are independently present in the user's semantic request.",
                suspicious.join(", ")
            );
            let mut correction_context = SelfCorrectContext::default();
            let repaired = correct_sql(
                &state,
                &claims,
                &sql,
                &guard_error,
                &semantic_question,
                &schema_tables,
                &foreign_key_prompts,
                &join_paths,
                &conversation_id,
                &mut correction_context,
                None,
                &db_type,
                &req.data_source_id,
                None,
                false,
            )
            .await;
            let repaired = extract_sql_from_llm_output(&repaired);
            let remaining = business_domain_derived_sql_literals(
                &repaired,
                &semantic_question,
                &domain_context.matched_domains,
            );
            if repaired.is_empty() || !remaining.is_empty() {
                push_rule_hit(
                    &mut applied_rules,
                    "business_domain_literal_blocked",
                    "Business Domain Literal Guard",
                    Some(format!(
                        "blocked domain-derived literals: {}",
                        suspicious.join(", ")
                    )),
                );
                return Err(AppError::ValidationError(
                    "Generated SQL incorrectly used a business-domain label as a row filter. AOS blocked the query instead of executing invented conditions; please retry."
                        .to_string(),
                ));
            }
            sql = repaired;
            push_rule_hit(
                &mut applied_rules,
                "business_domain_literal_repaired",
                "Business Domain Literal Guard",
                Some(format!(
                    "removed domain-derived literals: {}",
                    suspicious.join(", ")
                )),
            );
        }
    }

    if should_enable_sql_explain_preflight() && matches!(db_type.as_str(), "trino" | "presto") {
        self::query_async::emit_stage("explain_preflight", "正在 EXPLAIN 预校验 SQL");
        let mut preflight_context = SelfCorrectContext::default();
        let mut preflight_passed = false;
        let preflight_budget = agent_executor::DatasourceRequestBudget::new(3);
        for attempt in 0..=max_self_correct_attempts().max(1) {
            match explain_trino_or_presto_sql(
                &state,
                &claims,
                &req.data_source_id,
                &sql,
                preflight_budget.clone(),
            )
            .await
            {
                Ok(()) => {
                    preflight_passed = true;
                    if attempt == 0 {
                        push_rule_hit(
                            &mut applied_rules,
                            "sql_explain_preflight_passed",
                            "SQL EXPLAIN Preflight",
                            Some("Trino/Presto EXPLAIN passed before execution".to_string()),
                        );
                    } else {
                        push_rule_hit(
                            &mut applied_rules,
                            "sql_explain_preflight_repaired",
                            "SQL EXPLAIN Preflight",
                            Some(format!("EXPLAIN passed after {attempt} repair attempt(s)")),
                        );
                    }
                    break;
                }
                Err(e)
                    if attempt < max_self_correct_attempts().max(1)
                        && execution_support::SqlExecErrorKind::new(&e).is_retryable()
                        && !execution_support::SqlExecErrorKind::new(&e)
                            .allows_model_recovery_strategy() =>
                {
                    tracing::warn!(
                        datasource_id = %req.data_source_id,
                        attempt = attempt + 1,
                        error = %e,
                        "nl2sql generated SQL failed EXPLAIN preflight; attempting repair"
                    );
                    let repaired = correct_sql(
                        &state,
                        &claims,
                        &sql,
                        &e,
                        &req.question,
                        &schema_tables,
                        &foreign_key_prompts,
                        &join_paths,
                        &conversation_id,
                        &mut preflight_context,
                        None,
                        &db_type,
                        &req.data_source_id,
                        None,
                        false,
                    )
                    .await;
                    let repaired = extract_sql_from_llm_output(&repaired);
                    if repaired.is_empty() || repaired == sql {
                        push_rule_hit(
                            &mut applied_rules,
                            "sql_explain_preflight_repair_failed",
                            "SQL EXPLAIN Preflight",
                            Some(format!("repair produced no better SQL: {e}")),
                        );
                        break;
                    }
                    sql = repaired;
                    if let Some(constraint) = metric_hard_constraint.as_ref() {
                        if let Some(rewritten) =
                            enforce_metric_hard_constraint_sql(&sql, constraint)
                        {
                            sql = rewritten;
                        }
                    }
                    sql = normalize_sql_time_filters_with_qu(&sql, qu_result.as_ref());
                    let (preflight_normalized_sql, _) =
                        normalize_generated_time_conversions(&sql, &schema_tables, &db_type);
                    sql = preflight_normalized_sql;
                }
                Err(e) => {
                    let repairable = execution_support::SqlExecErrorKind::new(&e).is_retryable();
                    push_rule_hit(
                        &mut applied_rules,
                        "sql_explain_preflight_failed",
                        "SQL EXPLAIN Preflight",
                        Some(if repairable {
                            e
                        } else {
                            format!(
                                "operational EXPLAIN failure; skipped SQL repair and continued to execution validation: {e}"
                            )
                        }),
                    );
                    break;
                }
            }
        }
        if !preflight_passed {
            tracing::warn!(
                datasource_id = %req.data_source_id,
                "nl2sql EXPLAIN preflight did not pass; returning SQL with execution-stage fallback still available"
            );
        }
        self::query_async::emit_stage("explain_preflight", "EXPLAIN 预校验完成");
    }

    // ── Query Policy Enforcement ───────────────────────────────────────────────
    let target_tables = extract_tables_from_sql(&sql);
    let target_columns = extract_columns_from_sql(&sql);
    let policy_started = std::time::Instant::now();
    self::query_async::emit_stage("policy_enforcement", "正在执行策略校验");
    let policy_decision = enforce_query_policy(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
        &claims.sub,
        &claims.email,
        &target_tables,
        &target_columns,
    )
    .await?;
    if policy_decision.had_policy {
        push_rule_hit(
            &mut applied_rules,
            "query_policy_applied",
            "Query Policy",
            Some("access policy matched for current user".to_string()),
        );
    }
    if policy_decision.is_denied() {
        push_rule_hit(
            &mut applied_rules,
            "query_policy_will_block_execution",
            "Query Policy",
            Some(query_policy_denial_message(&policy_decision)),
        );
    }
    if let Some(row_filter) = policy_decision.row_filter_expr {
        push_rule_hit(
            &mut applied_rules,
            "row_filter_injected",
            "Row Filter Injection",
            Some(row_filter.clone()),
        );
        sql = inject_query_policy_row_filter(&sql, &row_filter).map_err(|error| {
            AppError::ValidationError(format!(
                "query policy row filter could not be applied safely: {error}"
            ))
        })?;
    }
    if let Some(allow) = strict_allow_tables.as_ref() {
        let sql_tables = extract_top_level_tables(&sql);
        let disallowed: Vec<String> = sql_tables
            .into_iter()
            .filter(|t| !table_ref_matches_set(t, allow))
            .collect();
        if !disallowed.is_empty() {
            push_rule_hit(
                &mut applied_rules,
                "strict_domain_sql_blocked",
                "Strict Business Domain SQL Guard",
                Some(format!("blocked tables: {}", disallowed.join(", "))),
            );
            sqlx::query(
                "INSERT INTO nl2sql_queries \
                 (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, error_message, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&query_id)
            .bind(&claims.tenant_id)
            .bind(&claims.sub)
            .bind(&req.data_source_id)
            .bind(&conversation_id)
            .bind(&req.question)
            .bind(format!(
                "Strict business domain policy blocked SQL. Disallowed tables: {}",
                disallowed.join(", ")
            ))
            .bind(planning_ms)
            .bind(route_confidence)
            .bind(routing_method.as_deref())
            .bind(semantic_context.clone())
            .bind(applied_rules_json_value(&applied_rules))
            .execute(&state.db)
            .await?;
            persist_reference_usages_for_query(
                &state,
                &claims,
                &query_id,
                &req.data_source_id,
                &req.question,
                &reference_snippets,
            )
            .await;
            upsert_nl2sql_conversation(
                &state.db,
                &claims.tenant_id,
                &claims.sub,
                &conversation_id,
                &req.question,
            )
            .await;
            return Ok(Json(QueryResponse {
                sql: None,
                explanation: None,
                error: Some(format!(
                    "Strict business domain policy blocked SQL. Disallowed tables: {}",
                    disallowed.join(", ")
                )),
                query_id,
                conversation_id: Some(conversation_id.clone()),
                summary_version: None,
                clarification_question: Some(
                    "该问题命中了强过滤业务域，但生成 SQL 引用了域外表。请改为该业务域已映射表后重试。"
                        .to_string(),
                ),
                confirmed_requirements: None,
                missing_requirements: None,
                query_understanding: qu_result.clone(),
                intent: qu_result.as_ref().map(|q| q.intent.to_string()),
                cache_hit: false,
                applied_rules,
                used_references: used_references.clone(),
            }));
        }
    }
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = policy_started.elapsed().as_millis() as u64,
        "nl2sql /query policy enforcement finished"
    );
    self::query_async::emit_stage("policy_enforcement", "策略校验通过");

    // Semantic compiler gate: parse the final policy-rewritten SQL and persist
    // the deterministic intent/verifier result before the legacy query row is
    // exposed as a releasable candidate. Every non-Release decision is
    // fail-closed: a repair or unresolved semantic ambiguity must never leak a
    // SQL candidate to execution merely because it parses.
    let audit = semantic_audit::compile_canonical_intent_with_contracts_and_joins(
        &question_intent,
        &sql,
        &loaded_metric_contracts,
        &loaded_join_contracts,
    )
    .ok_or_else(|| {
        AppError::ValidationError(
            "Semantic compiler could not produce a verifiable intent for this SQL candidate."
                .to_string(),
        )
    })?;
    let intent_json = semantic_audit::intent_json(&audit);
    let verification_json = semantic_audit::verification_json(&audit);
    let release_decision = serde_json::to_string(&audit.verification.release_decision)
        .unwrap_or_else(|_| "\"NeedsClarification\"".to_string())
        .trim_matches('"')
        .to_string();
    let calibrated_score = f64::from(audit.verification.confidence_basis.calibrated_score);
    if let Err(error) = crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
        &state.db,
        &claims.tenant_id,
        &req.data_source_id,
        &conversation_id,
        &query_id,
        &intent_json,
        &verification_json,
        &release_decision,
        calibrated_score,
    )
    .await
    {
        tracing::error!(
            tenant_id = %claims.tenant_id,
            datasource_id = %req.data_source_id,
            query_id = %query_id,
            error = %error,
            "failed to persist NL2SQL semantic audit; candidate is blocked"
        );
        return Err(AppError::Internal(format!(
            "failed to persist semantic verification before SQL release: {error}"
        )));
    } else {
        push_rule_hit(
            &mut applied_rules,
            "semantic_verifier_release_decision",
            "Semantic Verifier",
            Some(format!(
                "decision={release_decision}; calibrated_score={calibrated_score:.3}"
            )),
        );
    }
    if let Err(reason) = semantic_audit::require_execution_validation_decision(&release_decision) {
        return Err(AppError::ValidationError(format!(
            "{reason}. Resolve the metric, grain, population, time or join ambiguity and retry."
        )));
    }

    let persist_started = std::time::Instant::now();
    self::query_async::emit_stage("persist_result", "正在持久化查询结果");
    sqlx::query(
        "INSERT INTO nl2sql_queries \
         (id, tenant_id, user_id, data_source_id, conversation_id, question, generated_sql, executed, planning_ms, route_confidence, routing_method, semantic_context, applied_rules_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?)",
    )
    .bind(&query_id)
    .bind(&claims.tenant_id)
    .bind(&claims.sub)
    .bind(&req.data_source_id)
    .bind(&conversation_id)
    .bind(&req.question)
    .bind(&sql)
    .bind(planning_ms)
    .bind(route_confidence)
    .bind(routing_method.as_deref())
    .bind(semantic_context)
    .bind(applied_rules_json_value(&applied_rules))
    .execute(&state.db)
    .await?;
    persist_reference_usages_for_query(
        &state,
        &claims,
        &query_id,
        &req.data_source_id,
        &req.question,
        &reference_snippets,
    )
    .await;
    tracing::info!(
        datasource_id = %req.data_source_id,
        elapsed_ms = persist_started.elapsed().as_millis() as u64,
        "nl2sql /query query row persisted"
    );
    self::query_async::emit_stage("persist_result", "持久化完成");

    // Store generated SQL in result cache (background, non-blocking).
    {
        if used_references.is_empty() {
            let db2 = state.db.clone();
            let t = claims.tenant_id.clone();
            let d = req.data_source_id.clone();
            let h = cache_hash.clone();
            let q = req.question.clone();
            let s = sql.clone();
            let query_id_clone = query_id.clone();
            let lineage = cache_lineage.clone();
            tokio::spawn(async move {
                crate::nl2sql::result_cache::store(
                    &db2,
                    &t,
                    &d,
                    &h,
                    &q,
                    &s,
                    Some(&query_id_clone),
                    None,
                    &lineage,
                )
                .await;
            });
        }
    }

    // P3-2: Upsert conversation record and trigger summary generation.
    let convo_started = std::time::Instant::now();
    upsert_nl2sql_conversation(
        &state.db,
        &claims.tenant_id,
        &claims.sub,
        &conversation_id,
        &req.question,
    )
    .await;
    tracing::info!(
        conversation_id = %conversation_id,
        elapsed_ms = convo_started.elapsed().as_millis() as u64,
        "nl2sql /query conversation upsert finished"
    );

    let summary_started = std::time::Instant::now();
    let threshold = conversation_summary_threshold() as usize;
    let current_count: (u64,) =
        sqlx::query_as("SELECT message_count FROM nl2sql_conversations WHERE id = ?")
            .bind(&conversation_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or_default()
            .unwrap_or_default();

    if current_count.0 >= threshold as u64 {
        generate_conversation_summary(&state, &claims, &conversation_id).await;
    }

    let summary_version = fetch_summary_version_i32(&state.db, &conversation_id).await;
    tracing::info!(
        conversation_id = %conversation_id,
        summary_triggered = current_count.0 >= threshold as u64,
        message_count = current_count.0,
        threshold,
        elapsed_ms = summary_started.elapsed().as_millis() as u64,
        "nl2sql /query summary check finished"
    );

    tracing::info!(
        datasource_id = %req.data_source_id,
        total_elapsed_ms = req_started_at.elapsed().as_millis() as u64,
        "nl2sql /query finished"
    );

    Ok(Json(QueryResponse {
        sql: Some(sql.clone()),
        explanation: None,
        error: None,
        clarification_question: None,
        confirmed_requirements: None,
        missing_requirements: None,
        query_id,
        conversation_id: Some(conversation_id.clone()),
        summary_version,
        query_understanding: qu_result.clone(),
        intent: qu_result.as_ref().map(|q| q.intent.to_string()),
        cache_hit: false,
        applied_rules,
        used_references,
    }))
}

/// POST /api/v1/nl2sql/execute — execute generated SQL.
/// Tunable SQL self-correction parameters (P1-1).
fn max_self_correct_attempts() -> usize {
    nl2sql_domain::config::max_self_correct_attempts()
}

fn max_operational_retry_attempts() -> usize {
    std::env::var("NL2SQL_MAX_OPERATIONAL_RETRY_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .min(5)
}

fn conversation_summary_threshold() -> u32 {
    nl2sql_domain::config::conversation_summary_threshold()
}

pub(super) async fn fetch_summary_version_i32(
    db: &sqlx::SqlitePool,
    conversation_id: &str,
) -> Option<i32> {
    sqlx::query_scalar::<_, i64>(
        "SELECT CAST(summary_version AS INTEGER) \
         FROM nl2sql_conversations \
         WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(conversation_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|v| i32::try_from(v).unwrap_or(i32::MAX))
}

/// Whether Query Understanding is enabled for the `/query` handler.
/// Enabled by default for better routing accuracy.
pub(crate) fn should_enable_qu() -> bool {
    nl2sql_domain::config::should_enable_qu()
}

pub(crate) async fn detect_synonym_hits(
    db: &sqlx::SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
    question: &str,
) -> Vec<(String, String, String)> {
    let q_norm = normalize_domain_match_text(question);
    if q_norm.is_empty() {
        return Vec::new();
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT term, canonical_table, canonical_column \
         FROM nl2sql_synonyms \
         WHERE tenant_id = ? AND datasource_id = ? AND deleted_at IS NULL \
         LIMIT 2000",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut hits: Vec<(String, String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (term, canonical_table, canonical_column) in rows {
        let term_norm = normalize_domain_match_text(&term);
        if term_norm.is_empty() || !q_norm.contains(&term_norm) {
            continue;
        }
        let key = format!("{canonical_table}.{canonical_column}:{term_norm}");
        if seen.insert(key) {
            hits.push((term, canonical_table, canonical_column));
        }
    }
    hits
}

fn question_mentions_domain(question: &str, domain_name: &str) -> bool {
    nl2sql_domain::text::question_mentions_domain(question, domain_name)
}

struct StrictDomainQuestionMatch {
    matched_domains: Vec<String>,
    allowed_tables: std::collections::HashSet<String>,
}

struct BusinessDomainQuestionContext {
    matched_domains: Vec<String>,
    mapped_tables: Vec<String>,
}

fn remove_business_domain_derived_filters(
    qu: &mut crate::nl2sql::query_understanding::QueryUnderstandingResult,
    context: &BusinessDomainQuestionContext,
) -> usize {
    let labels = context
        .matched_domains
        .iter()
        .map(|label| normalize_domain_match_text(label))
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    if labels.is_empty() || qu.entities.filters.is_empty() {
        return 0;
    }

    let before = qu.entities.filters.len();
    qu.entities.filters.retain(|filter| {
        let value = normalize_domain_match_text(&filter.value);
        let raw = normalize_domain_match_text(&filter.raw);
        !labels.iter().any(|label| {
            (!value.is_empty() && value == *label) || (!raw.is_empty() && raw == *label)
        })
    });
    before.saturating_sub(qu.entities.filters.len())
}

impl BusinessDomainQuestionContext {
    fn system_prompt(&self) -> String {
        format!(
            "Configured business-domain routing metadata:\n- Matched domain labels: {}\n- Mapped tables: {}\nThe matched domain labels are routing metadata, not literal entity or field values. Do not split a domain label into names, and do not create WHERE predicates from it unless the user supplied separate explicit filter values. Prefer the mapped tables when answering the request. If a clarification asks for all rows, everything, an overview, or equivalent wording, produce a safe unfiltered overview from the mapped table(s) instead of inventing a metric or row predicate.",
            self.matched_domains.join(", "),
            self.mapped_tables.join(", ")
        )
    }

    fn semantic_question(&self, question: &str) -> String {
        let mut sanitized = question.to_string();
        let mut labels = self.matched_domains.clone();
        labels.sort_by_key(|label| std::cmp::Reverse(label.chars().count()));
        for label in labels {
            sanitized = replace_case_insensitive(&sanitized, &label, " ");
        }
        let sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
        if sanitized.is_empty() {
            "请在已匹配的业务域映射表范围内返回数据概览，不要添加任何行过滤条件。".to_string()
        } else {
            sanitized
        }
    }
}

fn replace_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return input.to_string();
    }
    if !needle.is_ascii() {
        return input.replace(needle, replacement);
    }
    let lower_input = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    while let Some(relative) = lower_input[cursor..].find(&lower_needle) {
        let start = cursor + relative;
        out.push_str(&input[cursor..start]);
        out.push_str(replacement);
        cursor = start + needle.len();
    }
    out.push_str(&input[cursor..]);
    out
}

fn business_domain_derived_sql_literals(
    sql: &str,
    semantic_question: &str,
    domain_labels: &[String],
) -> Vec<String> {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::tokenizer::{Token, Tokenizer};

    let semantic = normalize_domain_match_text(semantic_question);
    let labels = domain_labels
        .iter()
        .map(|label| normalize_domain_match_text(label))
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>();
    let Ok(tokens) = Tokenizer::new(&GenericDialect {}, sql).tokenize() else {
        return Vec::new();
    };
    let mut suspicious = std::collections::BTreeSet::new();
    for token in tokens {
        let raw = match token {
            Token::SingleQuotedString(value)
            | Token::DoubleQuotedString(value)
            | Token::NationalStringLiteral(value)
            | Token::EscapedStringLiteral(value)
            | Token::UnicodeStringLiteral(value) => value,
            Token::Number(value, _) => value,
            _ => continue,
        };
        let normalized = normalize_domain_match_text(&raw);
        if normalized.chars().count() < 2 || semantic.contains(&normalized) {
            continue;
        }
        if labels.iter().any(|label| label.contains(&normalized)) {
            suspicious.insert(raw);
        }
    }
    suspicious.into_iter().collect()
}

fn business_domain_context_for_question(
    domains: &[crate::nl2sql::routing::BusinessDomain],
    datasource_id: &str,
    question: &str,
) -> Option<BusinessDomainQuestionContext> {
    let mut matched_domains = std::collections::BTreeSet::new();
    let mut mapped_tables = std::collections::BTreeSet::new();
    for domain in domains {
        if domain.datasource_id.as_deref() != Some(datasource_id)
            || domain.domain_name.trim().is_empty()
            || domain.tables.is_empty()
            || !question_mentions_domain(question, &domain.domain_name)
        {
            continue;
        }
        matched_domains.insert(domain.domain_name.clone());
        mapped_tables.extend(domain.tables.iter().cloned());
    }
    if matched_domains.is_empty() || mapped_tables.is_empty() {
        return None;
    }
    Some(BusinessDomainQuestionContext {
        matched_domains: matched_domains.into_iter().collect(),
        mapped_tables: mapped_tables.into_iter().collect(),
    })
}

fn strict_domain_tables_for_question(
    domains: &[crate::nl2sql::routing::BusinessDomain],
    datasource_id: &str,
    question: &str,
) -> StrictDomainQuestionMatch {
    let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut matched_domains: Vec<String> = Vec::new();
    for domain in domains {
        if !domain.routing_mode.eq_ignore_ascii_case("strict") {
            continue;
        }
        if domain.datasource_id.as_deref() != Some(datasource_id) {
            continue;
        }
        if domain.domain_name.trim().is_empty() {
            continue;
        }
        if !question_mentions_domain(question, &domain.domain_name) {
            continue;
        }
        matched_domains.push(domain.domain_name.clone());
        for table in &domain.tables {
            insert_table_name_aliases(&mut out, table);
        }
    }
    StrictDomainQuestionMatch {
        matched_domains,
        allowed_tables: out,
    }
}

fn filter_schema_tables_by_allowlist(
    schema_tables: &serde_json::Value,
    allowlist: &std::collections::HashSet<String>,
) -> serde_json::Value {
    let filtered = schema_tables
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|table| {
                    let mut aliases = std::collections::HashSet::new();
                    insert_schema_table_aliases(&mut aliases, table);
                    aliases.iter().any(|alias| allowlist.contains(alias))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::Value::Array(filtered)
}

fn apply_policy_filter_to_set_expr(
    set_expr: &mut sqlparser::ast::SetExpr,
    filter: &sqlparser::ast::Expr,
) -> bool {
    use sqlparser::ast::{BinaryOperator, Expr, SetExpr};

    match set_expr {
        SetExpr::Select(select) => {
            let scoped_filter = Expr::Nested(Box::new(filter.clone()));
            select.selection = Some(match select.selection.take() {
                Some(existing) => Expr::BinaryOp {
                    left: Box::new(Expr::Nested(Box::new(existing))),
                    op: BinaryOperator::And,
                    right: Box::new(scoped_filter),
                },
                None => scoped_filter,
            });
            true
        }
        SetExpr::Query(query) => apply_policy_filter_to_set_expr(query.body.as_mut(), filter),
        SetExpr::SetOperation { left, right, .. } => {
            let left_applied = apply_policy_filter_to_set_expr(left.as_mut(), filter);
            let right_applied = apply_policy_filter_to_set_expr(right.as_mut(), filter);
            left_applied && right_applied
        }
        _ => false,
    }
}

fn inject_query_policy_row_filter(
    sql: &str,
    row_filter: &str,
) -> std::result::Result<String, String> {
    use sqlparser::ast::{SetExpr, Statement};
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;

    let dialect = GenericDialect {};
    let filter_wrapper = format!("SELECT * FROM __aos_policy_scope WHERE {row_filter}");
    let mut filter_statements =
        Parser::parse_sql(&dialect, &filter_wrapper).map_err(|error| error.to_string())?;
    if filter_statements.len() != 1 {
        return Err("row filter must contain exactly one predicate".to_string());
    }
    let filter = match filter_statements.pop() {
        Some(Statement::Query(query)) => match *query.body {
            SetExpr::Select(select) => select
                .selection
                .ok_or_else(|| "row filter expression is empty".to_string())?,
            _ => return Err("row filter expression is not a SELECT predicate".to_string()),
        },
        _ => return Err("row filter expression is invalid".to_string()),
    };

    let mut statements = Parser::parse_sql(&dialect, sql).map_err(|error| error.to_string())?;
    if statements.len() != 1 {
        return Err("exactly one SQL statement is required".to_string());
    }
    let statement = statements
        .first_mut()
        .ok_or_else(|| "SQL statement is empty".to_string())?;
    let Statement::Query(query) = statement else {
        return Err("row filters can only be applied to SELECT queries".to_string());
    };
    if !apply_policy_filter_to_set_expr(query.body.as_mut(), &filter) {
        return Err("row filter could not be applied to this query shape".to_string());
    }
    Ok(statement.to_string())
}

/// Whether result set validation is enabled after SQL execution.
/// Enabled by default. Set `NL2SQL_ENABLE_RESULT_VALIDATION=false` to disable.
pub(crate) fn should_enable_result_validation() -> bool {
    nl2sql_domain::config::should_enable_result_validation()
}

/// Whether business domain context is injected into the LLM routing prompt.
/// Enabled by default when `NL2SQL_ENABLE_DOMAIN_ROUTING` is not explicitly false.
pub(crate) fn should_enable_domain_routing() -> bool {
    nl2sql_domain::config::should_enable_domain_routing()
}

/// Inner execution without self-correction loop.
/// Separated so the retry logic in `execute` can call it cleanly.
#[allow(dead_code)]
struct ExecOnceResult {
    columns: Vec<ColumnInfo>,
    rows: Vec<serde_json::Value>,
    rows_count: usize,
    execution_ms: u64,
    error: Option<String>,
}

/// PATCH /api/v1/nl2sql/views/:query_id — rename or update a saved view.
#[derive(Debug, Deserialize)]
pub struct PatchSavedViewRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

// Query Policy CRUD moved to routes/nl2sql/query_policies.rs.

#[derive(Debug, Deserialize)]
pub(crate) struct SuggestRequest {
    pub question: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SuggestResponse {
    pub data_source_id: Option<String>,
    pub confidence: f32,
    pub reason: Option<String>,
}

// ── Health check ──────────────────────────────────────────────────────────────

// ── Semantic Routing ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RouteRequest {
    pub question: String,
    pub data_source_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteResponse {
    pub routed: bool,
    pub result: Option<RouteResult>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RouteResult {
    pub data_source_id: String,
    pub confidence: f32,
    pub method: String,
    pub matched_tables: Vec<MatchedTableInfo>,
    /// Present when the routing LLM detected ambiguity and returned a clarification question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarification_question: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MatchedTableInfo {
    pub data_source_id: String,
    pub table_name: String,
    pub best_column: String,
    /// Semantic description of the best-matching column (AI + user combined).
    pub column_description: String,
    /// Final fused similarity score [0, 1].
    pub similarity_score: f32,
}

// ── Semantics Management ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct RefreshSemanticsResponse {
    pub tables_processed: usize,
    pub columns_processed: usize,
    /// Per-table failures, `[(table_name, error)]`. Empty on full success.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tables: Vec<(String, String)>,
}

// ── Async Refresh Task ────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskCreatedResponse {
    pub task_id: String,
    pub status: String,
}

/// Body for `POST /nl2sql/semantics/:id/refresh-async`. All fields optional:
/// an empty body refreshes the whole datasource.
#[derive(Debug, Default, Deserialize)]
pub struct RefreshAsyncRequest {
    /// When set, only these tables are re-indexed. Used by the frontend's
    /// "retry failed tables" workflow.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RefreshTaskStatusResponse {
    pub task_id: String,
    pub datasource_id: String,
    pub status: String,
    pub progress: u32,
    pub processed_tables: u32,
    pub error_message: Option<String>,
    /// Optional JSON list `[{"table": "...", "error": "..."}, ...]` for
    /// partial-failure reporting. `None` when every table succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_tables: Option<serde_json::Value>,
    pub completed_at: Option<String>,
}

/// P2-2: POST /api/v1/nl2sql/datasource/{id}/reindex
/// Manually triggers a full re-index of a datasource's embedding vectors.
/// Clears the embedding store and re-runs the full refresh cycle.
#[derive(Debug, Deserialize)]
pub struct ReindexRequest {
    /// Optional: switch to a different embedding model during re-index.
    /// If omitted, re-index uses the currently configured model.
    pub new_model: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReindexResponse {
    pub status: String,
    pub task_id: Option<String>,
    pub message: String,
}

/// Returns the expected embedding dimensions for a model name.
#[allow(dead_code)]
fn dimensions_for_model(model: &str) -> usize {
    nl2sql_domain::config::dimensions_for_model(model)
}

#[derive(Debug, Serialize)]
pub(crate) struct EmbeddingConfigResponse {
    pub available: bool,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub configured_via: &'static str,
    pub dimensions: Option<usize>,
    pub api_configured: bool,
    pub local_model: String,
    pub profiles: Vec<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct GetSemanticsResponse {
    pub columns: Vec<ColumnSemanticsInfo>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct ColumnSemanticsInfo {
    pub table_name: String,
    pub column_name: String,
    pub ai_description: String,
    pub is_indexed: bool,
    pub version: i32,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSemanticsRequest {
    pub table_name: String,
    pub column_name: String,
    pub user_description: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct UpdateSemanticsResponse {
    pub success: bool,
    /// `true` when the vector was regenerated after saving the description.
    /// `false` means the description is durable but retrieval will still
    /// match against the old vector until a refresh completes.
    pub indexed: bool,
    /// Human-readable reason when `indexed` is `false` (typically
    /// "missing embedding model configuration").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
}

// ── Table-level semantics ─────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct GetAllTableSemanticsResponse {
    pub tables: Vec<TableSemanticsResponse>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct TableSemanticsResponse {
    pub table_name: String,
    pub ai_description: Option<String>,
    pub embedding_model: Option<String>,
    pub is_indexed: bool,
    pub version: i32,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateTableSemanticsRequest {
    pub user_description: String,
}

// ── Datasource-level semantics ────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct DatasourceSemanticsResponse {
    pub ai_description: Option<String>,
    pub user_description: Option<String>,
    pub embedding_model: Option<String>,
    pub is_indexed: bool,
    pub version: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDatasourceSemanticsRequest {
    pub user_description: String,
}

// ── Manual Foreign Keys CRUD ──────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct ForeignKeyResponse {
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

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct ForeignKeyListResponse {
    pub foreign_keys: Vec<ForeignKeyResponse>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct CreateForeignKeyRequest {
    #[serde(alias = "sourceTable")]
    pub source_table: String,
    #[serde(alias = "sourceColumn")]
    pub source_column: String,
    #[serde(default, alias = "sourceType")]
    pub source_type: String,
    #[serde(alias = "targetTable")]
    pub target_table: String,
    #[serde(alias = "targetColumn")]
    pub target_column: String,
    #[serde(default, alias = "targetType")]
    pub target_type: String,
}

/// PATCH /api/v1/nl2sql/foreign-keys/:datasource_id/:fk_id — update a manual FK.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateForeignKeyRequest {
    #[serde(alias = "sourceTable")]
    pub source_table: Option<String>,
    #[serde(alias = "sourceColumn")]
    pub source_column: Option<String>,
    #[serde(alias = "sourceType")]
    pub source_type: Option<String>,
    #[serde(alias = "targetTable")]
    pub target_table: Option<String>,
    #[serde(alias = "targetColumn")]
    pub target_column: Option<String>,
    #[serde(alias = "targetType")]
    pub target_type: Option<String>,
}

/// Map the typed update error from [`SchemaDescriber`] into an HTTP error.
/// Semantic records are created by `refresh_datasource`; a missing row
/// means the client is editing something that was never indexed.
#[allow(dead_code)]
pub(crate) fn map_update_err(
    e: crate::nl2sql::schema_describer::UpdateDescriptionError,
) -> AppError {
    use crate::nl2sql::schema_describer::UpdateDescriptionError as E;
    match e {
        E::NotFound => AppError::NotFound(
            "semantic record not found; run a schema refresh before editing".into(),
        ),
        E::UpdateFailed(err) => AppError::Internal(format!("update failed: {err}")),
        E::Database(err) => AppError::Internal(format!("database error: {err}")),
        E::Other(err) => AppError::Internal(err.to_string()),
    }
}

/// Generate a natural language explanation of the given SQL query.
async fn explain_sql(
    state: &AppState,
    claims: &Claims,
    sql: &str,
    schema: &serde_json::Value,
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

    let prompt = format!(
        "Given the following SQL query and database schema, explain what this SQL does in 1-2 sentences of natural language.\n\nSchema:\n{}\n\nSQL:\n{}\n\nExplanation:",
        serde_json::to_string_pretty(schema).unwrap_or_default(),
        sql
    );

    let request = MessageRequest {
        model: chat_cfg.model,
        max_tokens: 256,
        messages: vec![InputMessage {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: prompt }],
        }],
        system: Some("You are a SQL expert. Provide clear, concise explanations.".to_string()),
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

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ClarifyRequest {
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(alias = "conversationId")]
    pub conversation_id: Option<String>,
    pub question: Option<String>,
    #[serde(alias = "clarificationContext")]
    pub clarification_context: Option<crate::nl2sql::ClarificationContext>,
    #[serde(alias = "selectedOption")]
    pub selected_option: Option<SelectedOption>,
    #[serde(alias = "freeText")]
    pub free_text: Option<String>,
    /// Optional routing confidence (0.0-1.0) carried from the original query route decision.
    #[serde(alias = "routeConfidence")]
    pub route_confidence: Option<f32>,
    /// Optional routing method carried from the original query route decision.
    #[serde(alias = "routingMethod")]
    pub routing_method: Option<String>,
    /// Optional semantic context snapshot carried from the original query route decision.
    #[serde(alias = "semanticContext")]
    pub semantic_context: Option<serde_json::Value>,
    /// Original async query task that entered `waiting_input`. Route-only
    /// clarifications do not have one.
    #[serde(alias = "sourceQueryTaskId")]
    pub source_query_task_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SelectedOption {
    #[serde(alias = "optionIndex")]
    pub option_index: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ClarifyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ClarifyResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_clarification: Option<crate::nl2sql::ClarificationContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ClarifyResponseData {
    #[serde(alias = "dataSourceId")]
    pub data_source_id: String,
    pub question: String,
    pub sql: Option<String>,
    pub explanation: Option<String>,
    pub error: Option<String>,
    #[serde(alias = "queryId")]
    pub query_id: String,
    #[serde(alias = "conversationId")]
    pub conversation_id: Option<String>,
    #[serde(alias = "clarificationContext")]
    pub clarification_context: Option<crate::nl2sql::ClarificationContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_mode: Option<String>,
    /// Incremented each time the conversation summary is regenerated.
    #[serde(alias = "summaryVersion")]
    pub summary_version: Option<i32>,
    /// Enterprise traceability: which NL2SQL rules/guards were triggered.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub applied_rules: Vec<AppliedRuleHit>,
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn now_ms() -> u64 {
    if let Some(ts) = nl2sql_domain::config::now_ms() {
        ts
    } else {
        tracing::warn!("system clock is before Unix epoch — using 0 as fallback");
        0
    }
}

pub fn routes(state: AppState) -> Router<AppState> {
    // Handler imports from sub-modules
    use self::agent::{agent_execute, get_agent_result_page};
    use self::agent_async::{get_agent_task_status, start_agent_task, stream_agent_task_events};
    use self::analytics::{
        analytics_datasource_health, analytics_overview, analytics_routing, analytics_rule_hits,
        analytics_semantic_coverage, analytics_trends, analytics_user_leaderboard, slow_queries,
    };
    use self::attribution::{
        cancel_attribution_task, delete_attribution_conversation, get_attribution_conversation,
        get_attribution_task_status, list_attribution_conversations, start_attribution_task,
        stream_attribution_task_events,
    };
    use self::clarify_async::{
        get_clarify_task_status, start_clarify_task, stream_clarify_task_events,
    };
    use self::conversations::{
        delete_conversation, get_conversation, list_conversations, patch_conversation,
    };
    use self::cross_ds::{
        auto_discover_clusters, create_cross_domain_cluster, create_cross_ds_relation,
        delete_cross_domain_cluster, delete_cross_ds_relation, list_cross_domain_clusters,
        list_cross_ds_relations, update_cross_domain_cluster, update_cross_ds_relation,
    };
    use self::domains::{
        assign_tables_to_domain, create_business_domain, delete_domain, list_business_domains,
        list_domain_table_mappings, list_domains_for_datasource, rediscover_domains,
        unassign_tables_from_domain, update_domain,
    };
    use self::feedback::{
        clear_result_cache, get_feedback_stats, set_feedback_learning_approval, submit_feedback,
    };
    use self::foreign_keys::{cancel_clarify, get_clarify};
    use self::foreign_keys::{
        create_foreign_key, delete_foreign_key, list_foreign_keys, update_foreign_key,
    };
    use self::golden_cases::{evaluate_golden_cases_route, run_golden_cases_route};
    use self::join_paths::{
        create_join_path, delete_join_path, list_join_paths, rediscover_join_paths,
        update_join_path, verify_join_path,
    };
    use self::masking_rules::{
        create_masking_rule, delete_masking_rule, list_masking_rules, update_masking_rule,
    };
    use self::metrics::{
        create_metric, delete_metric, list_metrics, metric_lookup, update_metric,
        update_metric_status,
    };
    use self::queries::{execute, explain_sql, history};
    use self::query_async::{get_query_task_status, start_query_task, stream_query_task_events};
    use self::query_policies::{qp_create, qp_delete, qp_list, qp_update};
    use self::query_understanding::{clear_qu_cache, query_understanding};
    use self::routing::{
        clarify, embedding_health, explain, get_query_result_page, get_route_task_status, route,
        start_route_task, stream_route_task_events, suggest,
    };
    use self::schema_changes::{
        approve_schema_change, get_schema, get_schema_change_detail, list_schema_changes,
        reject_schema_change,
    };
    use self::semantics::{
        get_all_table_semantics, get_datasource_semantics, get_embedding_config,
        get_refresh_task_status, get_semantics, get_table_semantics, list_refresh_tasks,
        refresh_semantics, refresh_semantics_async, reindex_datasource,
        update_datasource_semantics, update_semantics, update_table_semantics,
    };
    use self::synonyms::{
        bulk_create_synonyms, create_synonym, delete_synonym, list_synonyms, update_synonym,
    };
    use self::time_patterns::{
        create_time_pattern, delete_time_pattern, list_time_patterns, update_time_pattern,
    };
    use self::validation_rules::{
        create_validation_rule, delete_validation_rule, list_validation_rules,
        update_validation_rule,
    };
    use self::views::{delete_saved_view, patch_saved_view, save_view, views};

    Router::new()
        .merge(self::reference::routes())
        .route("/query", routing_post(query))
        .route(
            "/golden-cases/evaluate",
            routing_post(evaluate_golden_cases_route),
        )
        .route("/golden-cases/run", routing_post(run_golden_cases_route))
        .route("/query-async", routing_post(start_query_task))
        .route("/query-tasks/{task_id}", routing_get(get_query_task_status))
        .route(
            "/query-tasks/{task_id}/events",
            routing_get(stream_query_task_events),
        )
        .route("/clarify-async", routing_post(start_clarify_task))
        .route(
            "/clarify-tasks/{task_id}",
            routing_get(get_clarify_task_status),
        )
        .route(
            "/clarify-tasks/{task_id}/events",
            routing_get(stream_clarify_task_events),
        )
        .route("/execute", routing_post(execute))
        .route(
            "/execute-stream",
            routing_post(self::stream_results::execute_stream),
        )
        .route("/history", routing_get(history))
        // F-10: Query permission policies
        .route("/query-policies", routing_get(qp_list))
        .route("/query-policies", routing_post(qp_create))
        .route("/query-policies/{id}", routing_patch(qp_update))
        .route("/query-policies/{id}", routing_delete(qp_delete))
        // F-11: Query performance analysis
        .route("/analytics/slow-queries", routing_get(slow_queries))
        .route(
            "/analytics/user-leaderboard",
            routing_get(analytics_user_leaderboard),
        )
        // P3-2: Conversation summary REST API
        .route("/conversations", routing_get(list_conversations))
        .route(
            "/conversations/{conversation_id}",
            routing_get(get_conversation),
        )
        .route(
            "/conversations/{conversation_id}",
            routing_patch(patch_conversation),
        )
        .route(
            "/conversations/{conversation_id}",
            routing_delete(delete_conversation),
        )
        .route("/views", routing_get(views))
        .route("/views", routing_post(save_view))
        .route("/views/{query_id}", routing_patch(patch_saved_view))
        .route("/views/{query_id}", routing_delete(delete_saved_view))
        .route("/schema/{data_source_id}", routing_get(get_schema))
        .route("/suggest", routing_post(suggest))
        .route("/route", routing_post(route))
        .route("/route-async", routing_post(start_route_task))
        .route("/route-tasks/{task_id}", routing_get(get_route_task_status))
        .route(
            "/route-tasks/{task_id}/events",
            routing_get(stream_route_task_events),
        )
        .route("/agent/execute", routing_post(agent_execute))
        .route("/agent/execute-async", routing_post(start_agent_task))
        .route(
            "/attribution/analyze-async",
            routing_post(start_attribution_task),
        )
        .route(
            "/attribution/conversations",
            routing_get(list_attribution_conversations),
        )
        .route(
            "/attribution/conversations/{conversation_id}",
            routing_get(get_attribution_conversation),
        )
        .route(
            "/attribution/conversations/{conversation_id}",
            routing_delete(delete_attribution_conversation),
        )
        .route(
            "/attribution/tasks/{task_id}",
            routing_get(get_attribution_task_status),
        )
        .route(
            "/attribution/tasks/{task_id}/cancel",
            routing_post(cancel_attribution_task),
        )
        .route(
            "/attribution/tasks/{task_id}/events",
            routing_get(stream_attribution_task_events),
        )
        .route("/results/{query_id}", routing_get(get_query_result_page))
        .route(
            "/agent-results/{query_id}",
            routing_get(get_agent_result_page),
        )
        .route("/agent-tasks/{task_id}", routing_get(get_agent_task_status))
        .route(
            "/agent-tasks/{task_id}/events",
            routing_get(stream_agent_task_events),
        )
        .route("/embedding-health", routing_get(embedding_health))
        .route("/explain", routing_post(explain))
        .route("/explain-sql", routing_post(explain_sql))
        .route("/embedding-config", routing_get(get_embedding_config))
        .route(
            "/datasource/{datasource_id}/reindex",
            routing_post(reindex_datasource),
        )
        .route("/semantics/{datasource_id}", routing_get(get_semantics))
        .route(
            "/semantics/{datasource_id}",
            routing_post(refresh_semantics),
        )
        .route(
            "/semantics/{datasource_id}",
            routing_patch(update_semantics),
        )
        // Table-level semantics
        .route(
            "/semantics/{datasource_id}/tables",
            routing_get(get_all_table_semantics),
        )
        .route(
            "/semantics/{datasource_id}/tables/{table_name}",
            routing_get(get_table_semantics),
        )
        .route(
            "/semantics/{datasource_id}/tables/{table_name}",
            routing_patch(update_table_semantics),
        )
        // Datasource-level semantics
        .route(
            "/semantics/{datasource_id}/datasource",
            routing_get(get_datasource_semantics).patch(update_datasource_semantics),
        )
        // Async refresh task
        .route(
            "/semantics/{datasource_id}/refresh-async",
            routing_post(refresh_semantics_async),
        )
        .route(
            "/semantics-tasks/{task_id}",
            routing_get(get_refresh_task_status),
        )
        .route("/semantics-tasks", routing_get(list_refresh_tasks))
        // P3-1: Multi-turn clarification
        .route("/clarify", routing_post(clarify))
        .route(
            "/clarify/{session_id}",
            routing_get(get_clarify).delete(cancel_clarify),
        )
        // P1-2: Manual Foreign Keys CRUD
        .route(
            "/foreign-keys/{datasource_id}",
            routing_get(list_foreign_keys),
        )
        .route(
            "/foreign-keys/{datasource_id}",
            routing_post(create_foreign_key),
        )
        .route(
            "/foreign-keys/{datasource_id}/{fk_id}",
            routing_patch(update_foreign_key),
        )
        .route(
            "/foreign-keys/{datasource_id}/{fk_id}",
            routing_delete(delete_foreign_key),
        )
        // P3-Enterprise: Business domains management
        .route("/domains", routing_get(list_business_domains))
        .route(
            "/domains/{datasource_id}",
            routing_get(list_domains_for_datasource),
        )
        .route(
            "/domains/{datasource_id}",
            routing_post(create_business_domain),
        )
        .route(
            "/domains/{datasource_id}/rediscover",
            routing_post(rediscover_domains),
        )
        .route(
            "/domains/{datasource_id}/tables/{domain_id}",
            routing_patch(update_domain),
        )
        .route(
            "/domains/{datasource_id}/tables/{domain_id}",
            routing_delete(delete_domain),
        )
        .route(
            "/domains/{datasource_id}/tables/{domain_id}/mappings",
            routing_get(list_domain_table_mappings),
        )
        .route(
            "/domains/{datasource_id}/tables/{domain_id}/mappings",
            routing_post(assign_tables_to_domain),
        )
        .route(
            "/domains/{datasource_id}/tables/{domain_id}/mappings",
            routing_delete(unassign_tables_from_domain),
        )
        // P3-Enterprise: Schema change notifications
        .route("/schema-changes", routing_get(list_schema_changes))
        .route(
            "/schema-changes/{notification_id}/approve",
            routing_post(approve_schema_change),
        )
        .route(
            "/schema-changes/{notification_id}/reject",
            routing_post(reject_schema_change),
        )
        .route(
            "/schema-changes/{notification_id}",
            routing_get(get_schema_change_detail),
        )
        // P3-Enterprise: Time patterns management
        .route("/time-patterns", routing_get(list_time_patterns))
        .route("/time-patterns", routing_post(create_time_pattern))
        .route(
            "/time-patterns/{pattern_id}",
            routing_patch(update_time_pattern),
        )
        .route(
            "/time-patterns/{pattern_id}",
            routing_delete(delete_time_pattern),
        )
        // P3-Enterprise: Validation rules management
        .route(
            "/validation-rules/{datasource_id}",
            routing_get(list_validation_rules),
        )
        .route(
            "/validation-rules/{datasource_id}",
            routing_post(create_validation_rule),
        )
        .route(
            "/validation-rules/{datasource_id}/{rule_id}",
            routing_patch(update_validation_rule),
        )
        .route(
            "/validation-rules/{datasource_id}/{rule_id}",
            routing_delete(delete_validation_rule),
        )
        // R-7: Column masking rules — tenant-wide CRUD
        .route("/masking-rules", routing_get(list_masking_rules))
        .route("/masking-rules", routing_post(create_masking_rule))
        .route("/masking-rules/{id}", routing_patch(update_masking_rule))
        .route("/masking-rules/{id}", routing_delete(delete_masking_rule))
        // P3-Enterprise: Query Understanding
        .route(
            "/query-understanding/{datasource_id}",
            routing_post(query_understanding),
        )
        .route(
            "/query-understanding/{datasource_id}/cache",
            routing_delete(clear_qu_cache),
        )
        // P2-1: Synonym management
        .route("/synonyms/{datasource_id}", routing_get(list_synonyms))
        .route("/synonyms/{datasource_id}", routing_post(create_synonym))
        .route(
            "/synonyms/{datasource_id}/bulk",
            routing_post(bulk_create_synonyms),
        )
        .route(
            "/synonyms/{datasource_id}/{synonym_id}",
            routing_patch(update_synonym),
        )
        .route(
            "/synonyms/{datasource_id}/{synonym_id}",
            routing_delete(delete_synonym),
        )
        // P1-2: Metrics / Measure semantic layer
        .route("/metrics/{datasource_id}", routing_get(list_metrics))
        .route("/metrics/{datasource_id}", routing_post(create_metric))
        .route(
            "/metrics/{datasource_id}/{metric_id}",
            routing_patch(update_metric),
        )
        .route(
            "/metrics/{datasource_id}/{metric_id}",
            routing_delete(delete_metric),
        )
        .route(
            "/metrics/{datasource_id}/{metric_id}/status",
            routing_post(update_metric_status),
        )
        .route(
            "/metrics/{datasource_id}/lookup",
            routing_get(metric_lookup),
        )
        // P1-3: Join path management
        .route("/join-paths/{datasource_id}", routing_get(list_join_paths))
        .route(
            "/join-paths/{datasource_id}",
            routing_post(create_join_path),
        )
        .route(
            "/join-paths/{datasource_id}/{path_id}",
            routing_put(update_join_path),
        )
        .route(
            "/join-paths/{datasource_id}/{path_id}",
            routing_delete(delete_join_path),
        )
        .route(
            "/join-paths/{datasource_id}/rediscover",
            routing_post(rediscover_join_paths),
        )
        .route(
            "/join-paths/{datasource_id}/{path_id}/verify",
            routing_patch(verify_join_path),
        )
        // P2-2: Cross-datasource relations management
        .route("/cross-ds-relations", routing_get(list_cross_ds_relations))
        .route(
            "/cross-ds-relations",
            routing_post(create_cross_ds_relation),
        )
        .route(
            "/cross-ds-relations/{relation_id}",
            routing_patch(update_cross_ds_relation),
        )
        .route(
            "/cross-ds-relations/{relation_id}",
            routing_delete(delete_cross_ds_relation),
        )
        // P2-3: Cross-domain cluster management
        .route(
            "/cross-domain-clusters",
            routing_get(list_cross_domain_clusters),
        )
        .route(
            "/cross-domain-clusters",
            routing_post(create_cross_domain_cluster),
        )
        .route(
            "/cross-domain-clusters/{cluster_id}",
            routing_patch(update_cross_domain_cluster),
        )
        .route(
            "/cross-domain-clusters/{cluster_id}",
            routing_delete(delete_cross_domain_cluster),
        )
        .route(
            "/cross-domain-clusters/auto-discover",
            routing_post(auto_discover_clusters),
        )
        // P3-1: NL2SQL Analytics
        .route("/analytics/overview", routing_get(analytics_overview))
        .route("/analytics/routing", routing_get(analytics_routing))
        .route("/analytics/rule-hits", routing_get(analytics_rule_hits))
        .route(
            "/analytics/datasource-health",
            routing_get(analytics_datasource_health),
        )
        .route(
            "/analytics/semantic-coverage",
            routing_get(analytics_semantic_coverage),
        )
        .route("/analytics/trends", routing_get(analytics_trends))
        // Feedback
        .route("/feedback", routing_post(submit_feedback))
        .route(
            "/feedback/{feedback_id}/approval",
            routing_post(set_feedback_learning_approval),
        )
        .route(
            "/feedback/stats/{datasource_id}",
            routing_get(get_feedback_stats),
        )
        // Result cache management
        .route(
            "/result-cache/{datasource_id}",
            routing_delete(clear_result_cache),
        )
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth_middleware::require_auth,
        ))
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Business Domains Handlers
// ══════════════════════════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Schema Change Notifications Handlers
// ══════════════════════════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Time Patterns Handlers
// ══════════════════════════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Validation Rules Handlers
// ══════════════════════════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Query Understanding Handler
// ══════════════════════════════════════════════════════════════════════════════

// ══════════════════════════════════════════════════════════════════════════════
// P3-Enterprise: Request/Response DTOs
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListBusinessDomainsResponse {
    domains: Vec<BusinessDomainResponse>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BusinessDomainResponse {
    id: i64, // BIGINT UNSIGNED from nl2sql_business_domains.id (widened)
    datasource_id: String,
    domain_name: String,
    domain_description: String,
    table_count: i64,
    confidence_score: f32,
    source: String,
    domain_routing_mode: String,
    tables: Vec<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListDomainsForDatasourceResponse {
    domains: Vec<BusinessDomainResponse>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDomainRequest {
    domain_name: String,
    #[serde(default)]
    domain_description: String,
    #[serde(default)]
    domain_routing_mode: Option<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDomainResponse {
    success: bool,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteDomainResponse {
    success: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateDomainRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "table_names", alias = "tableNames")]
    pub table_names: Vec<String>,
    #[serde(default)]
    pub domain_routing_mode: Option<String>,
}

#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateDomainResponse {
    pub id: i64,
    pub datasource_id: String,
    pub domain_name: String,
    pub domain_description: Option<String>,
    pub table_count: i64,
    pub confidence_score: f64,
    pub source: String,
    pub domain_routing_mode: String,
    pub tables: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DomainTableMappingItem {
    id: i64,
    table_name: String,
    datasource_id: String,
    domain_id: i64,
    confidence_score: f64,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListDomainTableMappingsResponse {
    mappings: Vec<DomainTableMappingItem>,
}
#[derive(Deserialize)]
struct AssignTablesToDomainRequest {
    #[serde(alias = "table_names", alias = "tableNames")]
    table_names: Vec<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssignTablesToDomainResponse {
    assigned_count: i32,
}
#[derive(Deserialize)]
struct UnassignTablesFromDomainRequest {
    #[serde(alias = "table_names", alias = "tableNames")]
    table_names: Vec<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnassignTablesFromDomainResponse {
    removed_count: i32,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RediscoverDomainsResponse {
    domains_discovered: usize,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SchemaChangesQuery {
    status: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListSchemaChangesResponse {
    changes: Vec<SchemaChangeNotification>,
    total: i64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaChangeNotification {
    id: i64,
    datasource_id: String,
    change_type: String,
    details: serde_json::Value,
    recommended_action: String,
    status: String,
    affected_queries_count: i32,
    created_at: String,
    reviewed_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewed_at: Option<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SchemaChangeDetailResponse {
    datasource_id: String,
    change_type: String,
    details: serde_json::Value,
    recommended_action: String,
    status: String,
    affected_queries_count: i32,
    created_at: String,
    affected_queries: Vec<AffectedQuery>,
}
#[allow(dead_code)]
#[derive(Debug, sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AffectedQuery {
    query_id: String,
    question: Option<String>,
    generated_sql: Option<String>,
    impact_level: String,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApproveSchemaChangeResponse {
    success: bool,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RejectSchemaChangeResponse {
    success: bool,
}

#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListTimePatternsResponse {
    patterns: Vec<TimePatternRow>,
}
#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimePatternRow {
    id: i64,
    pattern_regex: String,
    pattern_display: String,
    resolved_type: String,
    granularity: String,
    offset_days: i32,
    priority: i32,
    enabled: bool,
}
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTimePatternRequest {
    pattern_regex: String,
    #[serde(default)]
    pattern_display: String,
    resolved_type: String,
    #[serde(default)]
    granularity: String,
    #[serde(default)]
    offset_days: i32,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    test_text: Option<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateTimePatternResponse {
    id: u64,
}
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateTimePatternRequest {
    pattern_regex: Option<String>,
    pattern_display: Option<String>,
    resolved_type: Option<String>,
    granularity: Option<String>,
    offset_days: Option<i32>,
    priority: Option<i32>,
    enabled: Option<bool>,
    test_text: Option<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateTimePatternResponse {
    success: bool,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteTimePatternResponse {
    success: bool,
}

#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListValidationRulesResponse {
    rules: Vec<ValidationRuleRow>,
}
#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidationRuleRow {
    id: i64,
    table_name: String,
    column_name: String,
    rule_type: String,
    rule_config: serde_json::Value,
    severity: String,
    description: String,
    enabled: bool,
}
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateValidationRuleRequest {
    table_name: String,
    column_name: String,
    rule_type: String,
    rule_config: serde_json::Value,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    description: String,
}
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateValidationRuleRequest {
    table_name: Option<String>,
    column_name: Option<String>,
    rule_type: Option<String>,
    rule_config: Option<serde_json::Value>,
    severity: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateValidationRuleResponse {
    id: u64,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateValidationRuleResponse {
    success: bool,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteValidationRuleResponse {
    success: bool,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryUnderstandingRequest {
    question: String,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryUnderstandingResponse {
    rewritten_question: String,
    intent: String,
    entities: crate::nl2sql::query_understanding::QueryEntities,
    confidence: f32,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearQUCacheResponse {
    deleted: u64,
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-1: Synonym Management Handlers
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SynonymItem {
    id: i64,
    term: String,
    canonical_table: String,
    canonical_column: String,
    term_type: String,
    created_by: Option<String>,
    created_at: String,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListSynonymsResponse {
    synonyms: Vec<SynonymItem>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSynonymRequest {
    term: String,
    canonical_table: String,
    canonical_column: String,
    #[serde(default)]
    term_type: String,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSynonymRequest {
    term: Option<String>,
    canonical_table: Option<String>,
    canonical_column: Option<String>,
    term_type: Option<String>,
}

// POST /nl2sql/synonyms/:datasource_id/bulk — bulk-create synonyms from CSV import.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkCreateSynonymRequest {
    synonyms: Vec<CreateSynonymRequest>,
}
// ══════════════════════════════════════════════════════════════════════════════
// P1-2: Metrics / Measure Semantic Layer
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricItem {
    id: i64,
    metric_name: String,
    metric_aliases: serde_json::Value,
    expression: String,
    filter_conditions: Option<serde_json::Value>,
    description: Option<String>,
    granularity: String,
    time_column: Option<String>,
    timezone: String,
    population: serde_json::Value,
    allowed_grains: Vec<String>,
    invariants: serde_json::Value,
    join_contract_ids: Vec<String>,
    created_by: Option<String>,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListMetricsResponse {
    metrics: Vec<MetricItem>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateMetricRequest {
    metric_name: String,
    #[serde(default)]
    metric_aliases: serde_json::Value,
    expression: String,
    #[serde(default)]
    filter_conditions: Option<serde_json::Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_granularity")]
    granularity: String,
    #[serde(default)]
    time_column: Option<String>,
    #[serde(default = "default_metric_timezone")]
    timezone: String,
    #[serde(default = "default_metric_population")]
    population: serde_json::Value,
    #[serde(default)]
    allowed_grains: Vec<String>,
    #[serde(default = "default_metric_invariants")]
    invariants: serde_json::Value,
    #[serde(default)]
    join_contract_ids: Vec<String>,
}
#[allow(dead_code)]
fn default_granularity() -> String {
    "day".to_string()
}
#[allow(dead_code)]
fn default_metric_timezone() -> String {
    "UTC".to_string()
}
#[allow(dead_code)]
fn default_metric_population() -> serde_json::Value {
    serde_json::json!({
        "subject": "query_rows",
        "dedup_key": null,
        "exclude_test_users": false,
        "exclude_internal_users": false,
        "valid_record_rule": null
    })
}
#[allow(dead_code)]
fn default_metric_invariants() -> serde_json::Value {
    serde_json::json!([])
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMetricRequest {
    metric_name: Option<String>,
    metric_aliases: Option<serde_json::Value>,
    expression: Option<String>,
    filter_conditions: Option<serde_json::Value>,
    description: Option<String>,
    granularity: Option<String>,
    time_column: Option<String>,
    timezone: Option<String>,
    population: Option<serde_json::Value>,
    allowed_grains: Option<Vec<String>>,
    invariants: Option<serde_json::Value>,
    join_contract_ids: Option<Vec<String>>,
}

// ══════════════════════════════════════════════════════════════════════════════
// P1-3: Join Path Management
// ══════════════════════════════════════════════════════════════════════════════

/// P1-3: A JOIN path entry returned by GET /nl2sql/join-paths/:datasource_id.
/// Aligned with the frontend JoinPathItem type:
///   path[]      — table names parsed from path_text (traversal order)
#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinPathItem {
    id: i64,
    /// Ordered list of table names in the join traversal.
    path: Vec<String>,
    /// Ordered list of FK column names (pairs: source_col, target_col per hop).
    join_columns: Vec<String>,
    /// Datasource IDs for cross-datasource paths; empty for within-ds paths.
    ds_ids: Vec<String>,
    /// Number of JOIN hops (edges) in this path.
    total_columns: i32,
    verified: bool,
    confidence: f32,
    /// How this path was discovered: 'auto' | 'manual' | 'cross_ds'
    source: String,
    created_at: String,
    /// Raw DB fields for CRUD display/edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    join_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sql_joins: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    cardinality: Option<String>,
    temporal_condition: Option<String>,
    nullable: bool,
    dedup_strategy: Option<String>,
    allowed_grains: Vec<String>,
}

impl JoinPathItem {
    /// Parse a `path_text` string like:
    ///   "orders.customer_id → customers.id, customers.region_id → regions.id (2 hops)"
    /// into `path` (table names) and `join_columns` (FK column names).
    fn from_path_text(
        id: i64,
        path_text: &str,
        hops: i32,
        verified: bool,
        confidence: f32,
        source: &str,
        created_at: String,
        source_table: Option<String>,
        target_table: Option<String>,
        source_column: Option<String>,
        target_column: Option<String>,
        join_type: Option<String>,
        notes: Option<String>,
        cardinality: Option<String>,
        temporal_condition: Option<String>,
        nullable: bool,
        dedup_strategy: Option<String>,
        allowed_grains: Vec<String>,
    ) -> Self {
        let mut path: Vec<String> = Vec::new();
        let mut join_columns: Vec<String> = Vec::new();

        // Split on "→" to extract each leg of the path.
        for leg in path_text.split('→') {
            let leg = leg
                .trim()
                .trim_end_matches(|c: char| c == '(' || c.is_numeric() || c == ' ' || c == ')');
            // Each leg looks like: "orders.customer_id" or "orders.customer_id, ..."
            for part in leg.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                // Format: "table_name.column_name"
                if let Some(dot) = part.rfind('.') {
                    let table = part[..dot].trim().to_string();
                    let col = part[dot + 1..].trim().to_string();
                    if !table.is_empty() && !path.contains(&table) {
                        path.push(table);
                    }
                    if !col.is_empty() {
                        join_columns.push(col);
                    }
                }
            }
        }

        // Deduplicate path tables (multi-hop paths may reference same table twice).
        let mut seen = std::collections::HashSet::new();
        path.retain(|t| seen.insert(t.clone()));

        Self {
            id,
            path,
            join_columns,
            ds_ids: Vec::new(),
            total_columns: hops,
            verified,
            confidence,
            source: source.to_string(),
            created_at,
            source_table,
            target_table,
            source_column,
            target_column,
            join_type,
            path_text: Some(path_text.to_string()),
            sql_joins: None,
            notes,
            cardinality,
            temporal_condition,
            nullable,
            dedup_strategy,
            allowed_grains,
        }
    }
}

#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListJoinPathsResponse {
    paths: Vec<JoinPathItem>,
}

// POST /nl2sql/join-paths/:datasource_id
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateJoinPathRequest {
    source_table: String,
    target_table: String,
    source_column: String,
    target_column: String,
    #[serde(default)]
    join_type: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    notes: Option<String>,
}

// PUT /nl2sql/join-paths/:datasource_id/:path_id
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateJoinPathRequest {
    #[serde(default)]
    source_table: Option<String>,
    #[serde(default)]
    target_table: Option<String>,
    #[serde(default)]
    source_column: Option<String>,
    #[serde(default)]
    target_column: Option<String>,
    #[serde(default)]
    join_type: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    verified: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-2: Cross-Datasource Relations Management
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CrossDSRelationItem {
    id: i64,
    left_datasource: String,
    left_table: String,
    left_column: String,
    right_datasource: String,
    right_table: String,
    right_column: String,
    match_type: String,
    confidence: f32,
    verified: bool,
    source: String,
    created_at: String,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListCrossDSRelationsResponse {
    relations: Vec<CrossDSRelationItem>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateCrossDSRelationRequest {
    left_datasource: String,
    left_table: String,
    left_column: String,
    right_datasource: String,
    right_table: String,
    right_column: String,
    #[serde(default = "default_match_type")]
    match_type: String,
}
#[allow(dead_code)]
fn default_match_type() -> String {
    "foreign_key".to_string()
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCrossDSRelationRequest {
    verified: Option<bool>,
    match_type: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
// P2-3: Cross-Domain Cluster Management
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CrossDomainClusterItem {
    id: u64,
    cluster_name: String,
    datasource_ids: serde_json::Value,
    domain_ids: serde_json::Value,
    description: Option<String>,
    auto_discovered: bool,
    created_by: Option<String>,
    created_at: String,
}
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListCrossDomainClustersResponse {
    clusters: Vec<CrossDomainClusterItem>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCrossDomainClusterRequest {
    cluster_name: String,
    #[serde(default)]
    datasource_ids: serde_json::Value,
    #[serde(default)]
    domain_ids: serde_json::Value,
    #[serde(default)]
    description: Option<String>,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCrossDomainClusterRequest {
    cluster_name: Option<String>,
    datasource_ids: Option<serde_json::Value>,
    domain_ids: Option<serde_json::Value>,
    description: Option<String>,
}

// POST /nl2sql/cross-domain-clusters/auto-discover
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoDiscoverClustersRequest {
    datasource_ids: Vec<String>,
    #[serde(default)]
    auto_save: bool,
}

// ══════════════════════════════════════════════════════════════════════════════
// P3-1: NL2SQL Analytics Dashboard API
// ══════════════════════════════════════════════════════════════════════════════

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsOverview {
    total_queries: i64,
    success_rate: f64,
    avg_route_confidence: f64,
    avg_planning_ms: f64,
    avg_execution_ms: f64,
    planning_execution_ratio: f64,
    cache_hit_queries: i64,
    cache_hit_rate: f64,
    total_datasources: i64,
    total_tables_indexed: i64,
    avg_semantic_coverage: f64,
    total_conversations: i64,
}
#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsRouting {
    confidence_distribution: Vec<serde_json::Value>,
    method_distribution: Vec<serde_json::Value>,
    top_routed_tables: Vec<serde_json::Value>,
    clarification_rate: f64,
}
#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct DatasourceCoverage {
    datasource_id: String,
    datasource_name: String,
    total_tables: i64,
    indexed_tables: i64,
    total_columns: i64,
    indexed_columns: i64,
    coverage_pct: f64,
}
#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsSemanticCoverage {
    datasources: Vec<DatasourceCoverage>,
}
#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct DailyTrend {
    date: String,
    queries: i64,
    success_rate: f64,
    avg_confidence: f64,
}
#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsTrends {
    daily: Vec<DailyTrend>,
}

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsRuleHitItem {
    rule_key: String,
    rule_name: String,
    hits: i64,
    queries: i64,
    query_hit_rate: f64,
}

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsRuleHitDaily {
    date: String,
    total_queries: i64,
    queries_with_hits: i64,
    coverage_rate: f64,
    total_hits: i64,
}

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsRuleHits {
    total_queries: i64,
    queries_with_rule_hits: i64,
    coverage_rate: f64,
    total_rule_hits: i64,
    top_rules: Vec<AnalyticsRuleHitItem>,
    daily: Vec<AnalyticsRuleHitDaily>,
}

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsDatasourceHealthRow {
    datasource_id: String,
    datasource_name: String,
    total_queries: i64,
    successful_queries: i64,
    failed_queries: i64,
    success_rate: f64,
    avg_execution_ms: f64,
    p95_execution_ms: Option<f64>,
}

#[allow(dead_code)]
#[derive(Serialize)]
pub(crate) struct AnalyticsDatasourceHealth {
    rows: Vec<AnalyticsDatasourceHealthRow>,
    total: i64,
}

// GET /nl2sql/analytics/user-leaderboard — per-user query statistics for the leaderboard.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserLeaderboardEntry {
    pub user_id: String,
    pub total_queries: i64,
    pub successful_queries: i64,
    pub success_rate: f64,
    pub avg_execution_ms: Option<f64>,
    pub avg_confidence: Option<f64>,
    pub rank: i64,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct UserLeaderboardResponse {
    pub items: Vec<UserLeaderboardEntry>,
    pub period_days: i64,
}

// ── F-11: Query Performance Analysis ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlowQueryItem {
    pub id: String,
    pub question: String,
    pub data_source_id: String,
    pub generated_sql: Option<String>,
    pub execution_ms: i64,
    pub rows_returned: Option<i64>,
    pub created_at: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub(crate) struct SlowQueriesResponse {
    pub items: Vec<SlowQueryItem>,
    pub total: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_generator_prompt_receives_the_canonical_intent() {
        let mut prompt = "base generator contract".to_string();
        append_canonical_semantic_intent(
            &mut prompt,
            r#"{"objective":"Trend","metrics":[{"id":"orders"}]}"#,
        );
        assert!(prompt.contains("Canonical analytic intent"));
        assert!(prompt.contains(r#""objective":"Trend""#));
        assert!(prompt.contains("CLARIFICATION_NEEDED"));
    }

    #[tokio::test]
    async fn nl2sql_production_flow_uses_canonical_ir_before_and_after_generation() {
        let db = crate::test_sqlite_pool().await;
        let metric_contract = nl2sql_core::semantic_ir::MetricContract {
            id: "orders".into(),
            version: 1,
            names: vec!["orders".into(), "订单数".into()],
            expression: nl2sql_core::semantic_ir::MetricExpressionIR::Aggregate {
                function: "COUNT".into(),
                expression: Box::new(nl2sql_core::semantic_ir::MetricExpressionIR::Literal(
                    "*".into(),
                )),
                distinct: false,
            },
            denominator: None,
            population: nl2sql_core::semantic_ir::PopulationDefinition {
                subject: "order".into(),
                dedup_key: None,
                exclude_test_users: false,
                exclude_internal_users: false,
                valid_record_rule: None,
            },
            default_grain: nl2sql_core::semantic_ir::Grain::Day,
            allowed_grains: vec![nl2sql_core::semantic_ir::Grain::Day],
            time_column: "stat_date".into(),
            timezone: "Asia/Shanghai".into(),
            mandatory_filters: vec![],
            join_contracts: vec![],
            invariants: vec![],
            valid_from: "2026-01-01".into(),
            valid_until: None,
            owner: Some("analytics".into()),
            evidence_refs: vec!["contract://orders/v1".into()],
        };
        sqlx::query(
            "INSERT INTO metric_contracts
                (id, tenant_id, datasource_id, source_metric_id, version, status,
                 contract_json, lineage_json, valid_from, valid_until)
             VALUES ('orders', 'tenant', 'datasource', NULL, 1, 'active', ?,
                     '{}', '2026-01-01', NULL)",
        )
        .bind(serde_json::to_string(&metric_contract).unwrap())
        .execute(&db)
        .await
        .unwrap();
        let understanding = crate::nl2sql::query_understanding::QueryUnderstandingResult {
            rewritten_question: "按日期统计已支付订单数，范围为 2026-08-01 到 2026-08-08".into(),
            intent: crate::nl2sql::query_understanding::Intent::Trend,
            entities: crate::nl2sql::query_understanding::QueryEntities {
                time: Some(crate::nl2sql::query_understanding::TimeEntity {
                    raw: "2026-08-01 到 2026-08-08".into(),
                    resolved_type: "explicit".into(),
                    granularity: "day".into(),
                    ranges: vec![("2026-08-01".into(), "2026-08-08".into())],
                }),
                subject: Some(crate::nl2sql::query_understanding::SubjectEntity {
                    tables: vec!["orders".into()],
                    columns: vec!["stat_date".into(), "status".into()],
                    raw: "orders".into(),
                }),
                filters: vec![crate::nl2sql::query_understanding::FilterEntity {
                    column: "status".into(),
                    value: "paid".into(),
                    op: "=".into(),
                    raw: "已支付订单".into(),
                }],
                aggregations: vec!["COUNT".into()],
                comparisons: Vec::new(),
            },
            confidence: 0.93,
        };
        let durable = semantic_audit::compile_bind_and_persist_intent(
            &db,
            "tenant",
            "datasource",
            "conversation",
            "clarification-query",
            "按日期统计订单数",
            &["orders".into()],
            &serde_json::json!([{
                "table_name": "orders",
                "columns": [{"name": "stat_date"}, {"name": "order_id"}, {"name": "status"}]
            }]),
            &[],
            Some(&understanding),
        )
        .await
        .expect("canonical intent must be durable before provider generation");
        let canonical = crate::semantic_kernel_store::load_nl2sql_intent_ir(
            &db,
            "tenant",
            "clarification-query",
        )
        .await
        .unwrap()
        .expect("durable canonical intent");
        assert_eq!(canonical, durable.intent);
        assert_eq!(canonical.dimensions[0].column, "stat_date");
        assert_eq!(
            canonical
                .time
                .as_ref()
                .map(|time| time.end_exclusive.as_str()),
            Some("2026-08-09")
        );

        let canonical_json = durable.intent_json().unwrap();
        let mut prompt = "base generator contract".to_string();
        append_canonical_semantic_intent(&mut prompt, &canonical_json);
        assert!(prompt.contains(&canonical_json));

        let audit = semantic_audit::compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT stat_date, COUNT(*) AS order_count FROM orders WHERE stat_date >= '2026-08-01' AND stat_date < '2026-08-09' AND status = 'paid' GROUP BY stat_date",
            &durable.metric_contracts,
            &durable.join_contracts,
        )
        .unwrap();
        assert_eq!(audit.intent, canonical);
        assert_eq!(
            audit.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );
        let release_decision = serde_json::to_string(&audit.verification.release_decision)
            .unwrap()
            .trim_matches('"')
            .to_string();
        crate::semantic_kernel_store::persist_nl2sql_semantic_audit(
            &db,
            "tenant",
            "datasource",
            "conversation",
            "clarification-query",
            &semantic_audit::intent_json(&audit),
            &semantic_audit::verification_json(&audit),
            &release_decision,
            f64::from(audit.verification.confidence_basis.calibrated_score),
        )
        .await
        .expect("semantic release must be durable before SQL exposure");

        let drifting = semantic_audit::compile_canonical_intent_with_contracts_and_joins(
            &canonical,
            "SELECT COUNT(*) AS order_count FROM orders",
            &durable.metric_contracts,
            &durable.join_contracts,
        )
        .expect("drift audit");
        assert_ne!(
            drifting.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release
        );

        let today = chrono::Utc::now()
            .with_timezone(&chrono_tz::Asia::Shanghai)
            .date_naive();
        let yesterday = today.pred_opt().unwrap();
        let misleading_understanding =
            crate::nl2sql::query_understanding::QueryUnderstandingResult {
                rewritten_question: "查询昨天订单数".into(),
                intent: crate::nl2sql::query_understanding::Intent::Count,
                entities: crate::nl2sql::query_understanding::QueryEntities {
                    time: Some(crate::nl2sql::query_understanding::TimeEntity {
                        raw: "昨天".into(),
                        resolved_type: "relative".into(),
                        granularity: "day".into(),
                        ranges: vec![("1999-01-01".into(), "1999-01-02".into())],
                    }),
                    subject: None,
                    filters: Vec::new(),
                    aggregations: vec!["COUNT".into()],
                    comparisons: Vec::new(),
                },
                confidence: 0.99,
            };
        let relative = semantic_audit::compile_bind_and_persist_intent(
            &db,
            "tenant",
            "datasource",
            "relative-conversation",
            "relative-query",
            "查询昨天订单数",
            &["orders".into()],
            &serde_json::json!([{
                "table_name": "orders",
                "columns": [{"name": "business_date"}, {"name": "order_id"}]
            }]),
            &[],
            Some(&misleading_understanding),
        )
        .await
        .expect("relative time must be resolved by the deterministic compiler");
        let relative_time = relative.intent.time.as_ref().unwrap();
        assert_eq!(
            relative_time.start_inclusive,
            yesterday.format("%Y-%m-%d").to_string()
        );
        assert_eq!(
            relative_time.end_exclusive,
            today.format("%Y-%m-%d").to_string()
        );
        let wrong_window = semantic_audit::compile_canonical_intent_with_contracts_and_joins(
            &relative.intent,
            "SELECT COUNT(*) AS order_count FROM orders WHERE business_date >= '1999-01-01' AND business_date < '1999-01-02'",
            &relative.metric_contracts,
            &relative.join_contracts,
        )
        .expect("wrong-window SQL remains parseable audit evidence");
        assert_ne!(
            wrong_window.verification.release_decision,
            nl2sql_core::semantic_ir::QueryReleaseDecision::Release,
            "a model-proposed date must not override the kernel-resolved relative window"
        );
    }

    #[test]
    fn stale_concurrent_success_does_not_clear_a_new_candidate_failure() {
        let key = "test:stale-success:provider:model";
        let (_, _, observed_generation) = candidate_health_snapshot(key);

        suppress_nl2sql_candidate_by_key(key);
        mark_nl2sql_candidate_success_by_key(key, observed_generation);

        let (suppressed, healthy, current_generation) = candidate_health_snapshot(key);
        assert!(suppressed);
        assert!(!healthy);
        assert_ne!(current_generation, observed_generation);

        mark_nl2sql_candidate_success_by_key(key, current_generation);
        let (suppressed, healthy, _) = candidate_health_snapshot(key);
        assert!(!suppressed);
        assert!(healthy);
    }

    #[tokio::test]
    async fn unknown_candidate_uses_one_health_probe_then_releases_waiters() {
        let key = "test:single-probe:provider:model";
        let first = acquire_nl2sql_candidate_attempt_by_key(key, true)
            .await
            .expect("first health probe");
        assert!(first._probe_guard.is_some());

        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            acquire_nl2sql_candidate_attempt_by_key(key, true),
        )
        .await;
        assert!(blocked.is_err(), "a second unknown probe must wait");

        mark_nl2sql_candidate_success_by_key(key, first.failure_generation);
        drop(first);
        let next = acquire_nl2sql_candidate_attempt_by_key(key, true)
            .await
            .expect("healthy candidate");
        assert!(next._probe_guard.is_none());
    }

    #[test]
    fn output_text_collection_ignores_empty_leading_blocks() {
        let blocks = vec![
            OutputContentBlock::Text {
                text: "   ".to_string(),
            },
            OutputContentBlock::Thinking {
                thinking: "private reasoning".to_string(),
                signature: None,
            },
            OutputContentBlock::Text {
                text: "SELECT 1".to_string(),
            },
            OutputContentBlock::Text {
                text: "LIMIT 1".to_string(),
            },
        ];

        assert_eq!(collect_output_text(&blocks), "SELECT 1\nLIMIT 1");
    }

    #[test]
    fn semantic_review_timeout_default_is_bounded() {
        let timeout = sql_semantic_review_timeout_secs();
        assert!((5..=120).contains(&timeout));
    }

    #[test]
    fn detects_only_length_truncated_private_thinking_as_suppressible() {
        let response = api::MessageResponse {
            id: "thinking-only".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![OutputContentBlock::Thinking {
                thinking: "private reasoning".to_string(),
                signature: None,
            }],
            model: "reasoning-model".to_string(),
            stop_reason: Some("length".to_string()),
            stop_sequence: None,
            usage: api::Usage {
                input_tokens: 27_000,
                output_tokens: 1_024,
                ..api::Usage::default()
            },
            request_id: None,
            provider_metadata: None,
        };

        assert!(is_thinking_only_length_response(&response, ""));

        let mut completed = response.clone();
        completed.stop_reason = Some("end_turn".to_string());
        assert!(!is_thinking_only_length_response(&completed, ""));

        let mut with_text = response;
        with_text.content.push(OutputContentBlock::Text {
            text: "SELECT 1".to_string(),
        });
        assert!(!is_thinking_only_length_response(&with_text, "SELECT 1"));
    }

    #[test]
    fn table_name_aliases_include_full_schema_and_bare_names() {
        let aliases = table_name_aliases("`iceberg`.`mps_prod`.`business_order`");

        assert!(aliases.contains("iceberg.mps_prod.business_order"));
        assert!(aliases.contains("mps_prod.business_order"));
        assert!(aliases.contains("business_order"));
    }

    #[test]
    fn knowledge_schema_extraction_ignores_cte_and_system_tables() {
        let names = knowledge_physical_table_names(
            "WITH base AS (SELECT * FROM orders), grouped AS (SELECT * FROM base)\n\
             SELECT * FROM grouped JOIN customer_dim c ON c.id = grouped.customer_id\n\
             JOIN information_schema.columns i ON 1 = 1",
        );

        assert_eq!(
            names,
            vec!["customer_dim".to_string(), "orders".to_string()]
        );
    }

    #[test]
    fn merge_schema_tables_preserves_existing_and_deduplicates_aliases() {
        let existing = serde_json::json!([{
            "table_name": "catalog.analytics.orders",
            "schema": "analytics",
            "physical_table_name": "orders",
            "columns": []
        }]);
        let discovered = serde_json::json!([
            {"table_name": "orders", "columns": [{"name": "id"}]},
            {"table_name": "customers", "columns": [{"name": "id"}]}
        ]);

        let merged = merge_schema_tables(&existing, &discovered);
        let tables = merged.as_array().expect("merged schema array");
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0]["table_name"], "catalog.analytics.orders");
        assert_eq!(tables[1]["table_name"], "customers");
    }

    #[test]
    fn schema_allowlist_matches_trino_table_aliases() {
        let schema_tables = serde_json::json!([
            {
                "table_name": "iceberg.mps_prod.business_order",
                "fully_qualified_name": "iceberg.mps_prod.business_order",
                "qualified_name": "mps_prod.business_order",
                "physical_table_name": "business_order",
                "name": "business_order",
                "catalog": "iceberg",
                "schema": "mps_prod"
            },
            {
                "table_name": "iceberg.mps_prod.user_profile",
                "physical_table_name": "user_profile",
                "catalog": "iceberg",
                "schema": "mps_prod"
            }
        ]);
        let mut allow = std::collections::HashSet::new();
        insert_table_name_aliases(&mut allow, "business_order");

        let filtered = filter_schema_tables_by_allowlist(&schema_tables, &allow);
        let tables = filtered.as_array().expect("filtered tables array");

        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].get("table_name").and_then(|v| v.as_str()),
            Some("iceberg.mps_prod.business_order")
        );
    }

    fn test_reference_snippet(content: &str) -> ReferencePromptSnippet {
        ReferencePromptSnippet {
            pack_id: "pack-1".to_string(),
            pack_name: "测试知识库".to_string(),
            file_id: "file-1".to_string(),
            filename: "metrics.sql".to_string(),
            chunk_id: "chunk-1".to_string(),
            language: Some("sql".to_string()),
            start_line: 1,
            end_line: 20,
            score: 2.4,
            reason: "matched: ecpm".to_string(),
            chunk_type: "sql_example".to_string(),
            verified: true,
            stale: false,
            content: content.to_string(),
        }
    }

    #[test]
    fn sql_knowledge_reference_can_resolve_ascii_metric_requirement() {
        let refs = vec![test_reference_snippet(
            "SELECT dt, ecpm_bucket, AVG(ecpm) AS ecpm FROM ads GROUP BY dt, ecpm_bucket",
        )];
        let candidates = knowledge_metric_candidates_from_references("昨天ecpm分布", &refs);

        assert!(candidates.iter().any(|(name, _, _)| name == "ecpm"));
    }

    #[test]
    fn sql_knowledge_reference_can_resolve_chinese_metric_requirement() {
        let refs = vec![test_reference_snippet(
            "SELECT dt, SUM(ad_rev) AS ad_revenue -- 广告收入 FROM ads GROUP BY dt",
        )];
        let candidates = knowledge_metric_candidates_from_references("昨天广告收入是多少", &refs);

        assert!(candidates.iter().any(|(name, _, _)| name == "广告收入"));
    }

    #[test]
    fn sql_knowledge_reference_does_not_invent_metric_without_match() {
        let refs = vec![test_reference_snippet(
            "SELECT dt, user_id FROM user_events WHERE dt = '${dt}' LIMIT 10",
        )];
        let candidates = knowledge_metric_candidates_from_references("统计最近两天的数据", &refs);

        assert!(candidates.is_empty());
    }

    #[test]
    fn strong_sql_knowledge_allows_empty_schema_flow() {
        let refs = vec![test_reference_snippet(
            "SELECT dt, AVG(ecpm) AS ecpm FROM analyst.ad_show WHERE dt='${dt}' GROUP BY dt",
        )];

        assert!(has_strong_sql_knowledge_context(
            &serde_json::json!([]),
            &refs
        ));
    }

    #[test]
    fn weak_text_knowledge_does_not_bypass_clarification_gate() {
        let mut weak = test_reference_snippet("这里是一段泛泛的数据说明，没有 SQL 示例。");
        weak.chunk_type = "text".to_string();
        weak.language = Some("markdown".to_string());
        weak.filename = "readme.md".to_string();
        weak.score = 0.1;
        weak.verified = false;

        assert!(!has_strong_sql_knowledge_context(
            &serde_json::json!([]),
            &[weak]
        ));
    }

    #[test]
    fn sql_knowledge_auto_open_candidates_prioritize_verified_sql_files() {
        let mut stale = test_reference_snippet("SELECT * FROM old_metric");
        stale.file_id = "stale-file".to_string();
        stale.filename = "old_metric.sql".to_string();
        stale.score = 99.0;
        stale.stale = true;

        let mut text = test_reference_snippet("Metric note without executable SQL.");
        text.file_id = "text-file".to_string();
        text.filename = "metric.md".to_string();
        text.chunk_type = "text".to_string();
        text.language = Some("markdown".to_string());
        text.score = 9.0;

        let mut lower_score_verified =
            test_reference_snippet("SELECT dt, AVG(ecpm) AS ecpm FROM t");
        lower_score_verified.file_id = "verified-sql".to_string();
        lower_score_verified.filename = "verified.sql".to_string();
        lower_score_verified.score = 0.01;
        lower_score_verified.verified = true;

        let mut higher_score_unverified =
            test_reference_snippet("SELECT dt, MAX(ecpm) AS peak_ecpm FROM t");
        higher_score_unverified.file_id = "unverified-sql".to_string();
        higher_score_unverified.filename = "unverified.sql".to_string();
        higher_score_unverified.score = 3.0;
        higher_score_unverified.verified = false;

        let mut too_weak_unverified =
            test_reference_snippet("SELECT dt, SUM(noise) AS noise FROM t");
        too_weak_unverified.file_id = "too-weak-sql".to_string();
        too_weak_unverified.filename = "too_weak.sql".to_string();
        too_weak_unverified.score = 0.01;
        too_weak_unverified.verified = false;

        let mut duplicate = lower_score_verified.clone();
        duplicate.chunk_id = "verified-sql-later-chunk".to_string();
        duplicate.start_line = 200;

        let selected = sql_knowledge_auto_open_file_candidates(
            "ecpm",
            &[
                stale,
                text,
                too_weak_unverified,
                higher_score_unverified,
                duplicate,
                lower_score_verified,
            ],
            4,
        );

        assert_eq!(selected[0], "verified-sql");
        assert_eq!(selected[1], "unverified-sql");
        assert_eq!(
            selected.len(),
            2,
            "stale/text/duplicate/too-weak candidates are skipped"
        );
    }

    #[test]
    fn sql_knowledge_metric_gate_rejects_verified_but_unrelated_examples() {
        let mut hit_rate = test_reference_snippet(
            "SELECT app, hit_uv / NULLIF(app_uv, 0) AS hit_rate_pct FROM rule_hits",
        );
        hit_rate.file_id = "hit-rate-file".to_string();
        hit_rate.filename = "rule_hit_rate.sql".to_string();
        hit_rate.score = 99.0;
        hit_rate.verified = true;

        let mut ecpm = test_reference_snippet("SELECT app, AVG(ecpm) AS ecpm FROM ad_metrics");
        ecpm.file_id = "ecpm-file".to_string();
        ecpm.filename = "ecpm_analysis.sql".to_string();
        ecpm.score = 88.0;
        ecpm.verified = true;

        let selected = sql_knowledge_auto_open_file_candidates(
            "有没有哪些 app 的 ROI 持续下降？原因是什么？",
            &[hit_rate, ecpm],
            2,
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn sql_metric_identifier_matching_ignores_substrings_comments_and_literals() {
        let false_positive = r#"
            -- ROI is mentioned only as an analyst note.
            SELECT 'ROI' AS label, android_device_id, hit_rate_pct
            FROM rule_hits
            WHERE rule_name = 'Android-设备状态异常_高风险'
        "#;
        let real_metric = r#"
            SELECT app_id,
                   revenue / NULLIF(cost, 0) AS ad_reward_roi
            FROM app_daily_metrics
        "#;

        assert!(!sql_code_contains_ascii_metric_identifier(
            false_positive,
            "roi"
        ));
        assert!(sql_code_contains_ascii_metric_identifier(
            real_metric,
            "roi"
        ));
    }

    #[test]
    fn tool_metric_filter_keeps_context_from_a_file_with_a_real_metric_chunk() {
        let mut header = test_reference_snippet("WITH params AS (SELECT current_date AS dt)");
        header.file_id = "roi-file".to_string();
        header.chunk_id = "roi-header".to_string();

        let mut metric = test_reference_snippet(
            "SELECT app, revenue / NULLIF(cost, 0) AS roi FROM app_daily_metrics",
        );
        metric.file_id = "roi-file".to_string();
        metric.chunk_id = "roi-metric".to_string();

        let mut unrelated =
            test_reference_snippet("SELECT android_device_id, hit_rate_pct FROM rule_hits");
        unrelated.file_id = "rule-file".to_string();
        unrelated.chunk_id = "rule-hit-rate".to_string();

        let filtered = filter_sql_knowledge_snippets_by_core_metric(
            "哪些 app 的 ROI 持续下降",
            vec![header, unrelated, metric],
        );

        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|snippet| snippet.file_id == "roi-file"));
    }

    #[test]
    fn sql_knowledge_metric_gate_keeps_matching_roi_example() {
        let mut roi = test_reference_snippet(
            "SELECT app, revenue / NULLIF(cost, 0) AS ROI FROM app_daily_metrics",
        );
        roi.file_id = "roi-file".to_string();
        roi.filename = "app_profitability.sql".to_string();

        let selected =
            sql_knowledge_auto_open_file_candidates("有没有哪些 app 的 ROI 持续下降？", &[roi], 2);

        assert_eq!(selected, vec!["roi-file"]);
    }

    #[test]
    fn sql_knowledge_metric_gate_does_not_block_non_metric_questions() {
        let mut devices =
            test_reference_snippet("SELECT device_id, device_name FROM task_dispatch_device");
        devices.file_id = "device-file".to_string();
        devices.filename = "device_inventory.sql".to_string();

        let selected = sql_knowledge_auto_open_file_candidates("查下都有哪些设备", &[devices], 2);

        assert_eq!(selected, vec!["device-file"]);
    }

    #[test]
    fn bounded_excerpt_keeps_metric_logic_near_the_end_of_long_sql() {
        let mut lines = (1..=420)
            .map(|line| format!("-- unrelated setup line {line}"))
            .collect::<Vec<_>>();
        lines.push("SELECT app, revenue / NULLIF(cost, 0) AS ROI".to_string());
        lines.push("FROM app_daily_metrics GROUP BY app".to_string());
        let excerpt =
            focused_bounded_sql_excerpt(&lines.join("\n"), "哪些 app 的 ROI 持续下降或骤降", 4_000);

        assert!(excerpt.contains("AS ROI"));
        assert!(excerpt.contains("app_daily_metrics"));
        assert!(excerpt.contains("[relevant lines"));
        assert!(excerpt.chars().count() <= 4_000);
    }

    #[test]
    fn bounded_references_limit_files_and_total_prompt_evidence() {
        let mut first = test_reference_snippet(&format!(
            "{}\nSELECT app, revenue / cost AS ROI FROM roi_daily",
            "-- header\n".repeat(8_000)
        ));
        first.file_id = "roi-file".to_string();
        first.filename = "roi.sql".to_string();
        let mut second = test_reference_snippet(&"SELECT ROI FROM second\n".repeat(2_000));
        second.file_id = "second-file".to_string();
        second.filename = "second.sql".to_string();
        let mut noise = test_reference_snippet(&"SELECT app FROM unrelated\n".repeat(2_000));
        noise.file_id = "noise-file".to_string();
        noise.filename = "noise.sql".to_string();
        noise.score = 99.0;

        let selected = focus_bounded_sql_knowledge_references(
            "哪些 app 的 ROI 持续下降",
            &[noise, second, first],
        );
        let total_chars = selected
            .iter()
            .map(|snippet| snippet.content.chars().count())
            .sum::<usize>();

        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|snippet| snippet.file_id == "roi-file"));
        assert!(total_chars <= BOUNDED_SQL_KNOWLEDGE_MAX_CHARS);
    }

    #[test]
    fn bounded_references_return_no_unrelated_files_for_explicit_metric() {
        let mut hit_rate = test_reference_snippet(
            "SELECT app, hit_uv / NULLIF(app_uv, 0) AS hit_rate_pct FROM rule_hits",
        );
        hit_rate.file_id = "hit-rate-file".to_string();
        hit_rate.filename = "rule_hit_rate.sql".to_string();
        hit_rate.score = 99.0;
        hit_rate.verified = true;

        let mut ecpm = test_reference_snippet("SELECT app, AVG(ecpm) AS ecpm FROM ad_metrics");
        ecpm.file_id = "ecpm-file".to_string();
        ecpm.filename = "ecpm_analysis.sql".to_string();
        ecpm.score = 88.0;
        ecpm.verified = true;

        let selected = focus_bounded_sql_knowledge_references(
            "有没有哪些 app 的 ROI 持续下降？原因是什么？",
            &[hit_rate, ecpm],
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn generation_tool_payload_preserves_long_sql_context() {
        let long_sql = format!(
            "WITH base AS (SELECT dt, user_id, ecpm FROM fact_ads)\n{}",
            "SELECT dt, AVG(ecpm) AS ecpm FROM base GROUP BY dt\n".repeat(120)
        );
        let refs = vec![test_reference_snippet(&long_sql)];

        let payload = sql_knowledge_tool_payload("sql_example_open", "ecpm", &refs, 12_000);
        let content = payload
            .get("items")
            .and_then(|v| v.as_array())
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(|v| v.as_str())
            .expect("payload content");

        assert!(content.contains("WITH base"));
        assert!(content.chars().count() > 5_000);
    }

    #[test]
    fn business_domain_context_marks_label_as_routing_metadata() {
        let domains = vec![crate::nl2sql::routing::BusinessDomain {
            domain_name: "设备运维域".to_string(),
            domain_description: "设备状态".to_string(),
            tables: vec!["task_dispatch_device".to_string()],
            confidence_score: 0.95,
            datasource_id: Some("ds-1".to_string()),
            routing_mode: "assist".to_string(),
        }];

        let context = business_domain_context_for_question(&domains, "ds-1", "查一下设备运维域")
            .expect("configured domain should match");
        let prompt = context.system_prompt();

        assert_eq!(context.mapped_tables, vec!["task_dispatch_device"]);
        assert!(prompt.contains("routing metadata"));
        assert!(prompt.contains("not literal entity or field values"));
    }

    #[test]
    fn business_domain_label_is_removed_from_qu_filters_but_real_filters_remain() {
        let context = BusinessDomainQuestionContext {
            matched_domains: vec!["张三李四".to_string()],
            mapped_tables: vec!["task_dispatch_device".to_string()],
        };
        let mut qu = crate::nl2sql::query_understanding::QueryUnderstandingResult {
            rewritten_question: String::new(),
            intent: crate::nl2sql::query_understanding::Intent::Select,
            entities: crate::nl2sql::query_understanding::QueryEntities {
                time: None,
                subject: None,
                filters: vec![
                    crate::nl2sql::query_understanding::FilterEntity {
                        column: "device_name".to_string(),
                        value: "张三, 李四".to_string(),
                        op: "IN".to_string(),
                        raw: "张三李四".to_string(),
                    },
                    crate::nl2sql::query_understanding::FilterEntity {
                        column: "enabled".to_string(),
                        value: "true".to_string(),
                        op: "=".to_string(),
                        raw: "启用的".to_string(),
                    },
                ],
                aggregations: Vec::new(),
                comparisons: Vec::new(),
            },
            confidence: 1.0,
        };

        assert_eq!(remove_business_domain_derived_filters(&mut qu, &context), 1);
        assert_eq!(qu.entities.filters.len(), 1);
        assert_eq!(qu.entities.filters[0].column, "enabled");
    }

    #[test]
    fn business_domain_label_is_removed_from_semantic_question() {
        let context = BusinessDomainQuestionContext {
            matched_domains: vec!["张三李四".to_string()],
            mapped_tables: vec!["task_dispatch_device".to_string()],
        };

        assert_eq!(context.semantic_question("查一下张三李四"), "查一下");
        assert_eq!(
            context.semantic_question("查一下张三李四\n补充条件：所有"),
            "查一下 补充条件：所有"
        );
        assert_eq!(
            context.semantic_question("业务域张三李四里只看 device_name 为张三"),
            "业务域 里只看 device_name 为张三"
        );
    }

    #[test]
    fn business_domain_literal_guard_blocks_invented_filters_but_keeps_explicit_values() {
        let labels = vec!["张三李四".to_string()];
        let invented = business_domain_derived_sql_literals(
            "SELECT * FROM task_dispatch_device WHERE device_name IN ('张三', '李四')",
            "查一下",
            &labels,
        );
        assert_eq!(invented, vec!["张三".to_string(), "李四".to_string()]);

        let explicit = business_domain_derived_sql_literals(
            "SELECT * FROM task_dispatch_device WHERE device_name = '张三'",
            "只看 device_name 为张三",
            &labels,
        );
        assert!(explicit.is_empty());
    }

    #[test]
    fn query_policy_row_filter_is_injected_with_and_without_existing_where() {
        let without_where = inject_query_policy_row_filter(
            "SELECT order_id FROM orders ORDER BY order_id",
            "tenant_id = 'tenant-a'",
        )
        .expect("inject filter");
        assert!(without_where.contains("WHERE (tenant_id = 'tenant-a')"));
        assert!(without_where.contains("ORDER BY order_id"));

        let with_where = inject_query_policy_row_filter(
            "SELECT order_id FROM orders WHERE status = 'paid'",
            "tenant_id = 'tenant-a'",
        )
        .expect("inject filter");
        assert!(with_where.contains("(status = 'paid') AND (tenant_id = 'tenant-a')"));
    }

    #[test]
    fn query_policy_row_filter_is_applied_to_every_union_branch() {
        let sql = inject_query_policy_row_filter(
            "SELECT id FROM current_orders UNION ALL SELECT id FROM archived_orders",
            "tenant_id = 'tenant-a'",
        )
        .expect("inject union filters");

        assert_eq!(sql.matches("tenant_id = 'tenant-a'").count(), 2);
        assert!(inject_query_policy_row_filter(
            "SELECT id FROM orders",
            "tenant_id = 'tenant-a'; SELECT 1",
        )
        .is_err());
    }
}
